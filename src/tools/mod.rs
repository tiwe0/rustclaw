use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::log;
use crate::memory::MemoryBackend;
use crate::session::SessionManager;
use crate::skills::SkillsBackend;
use crate::types::{Message, ToolCall, ToolDefinition};

pub mod http;
pub mod web;
pub mod exec;
#[cfg(feature = "mobile")]
pub mod mobile;
pub mod memory;
pub mod manager;
pub mod skills;
pub mod time;

#[async_trait]
pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn definition(&self) -> ToolDefinition;
    fn init(&self) {}
    fn finit(&self) {}
    async fn execute(&self, args: Value) -> Result<Value>;
}

pub struct ToolManager {
    plugins: HashMap<String, Box<dyn ToolPlugin>>,
    session_manager: SessionManager,
    shutdown_called: AtomicBool,
}

impl ToolManager {
    pub fn with_builtin_plugins(
        session_manager: SessionManager,
        memory_backend: Option<Arc<dyn MemoryBackend>>,
        memory_default_key: String,
        skills_backend: Option<Arc<dyn SkillsBackend>>,
        skills_default_name: String,
    ) -> Self {
        let mut manager = Self {
            plugins: HashMap::new(),
            session_manager: session_manager.clone(),
            shutdown_called: AtomicBool::new(false),
        };
        let memory_backend_for_manager = memory_backend.clone();
        let skills_backend_for_manager = skills_backend.clone();
        let mut available_tools = vec![
            "get_time".to_string(),
            "http_request".to_string(),
            "web_browser".to_string(),
            "exec_command".to_string(),
            "session_resource_manager".to_string(),
        ];

        manager.register(Box::new(time::TimeTool));
        manager.register(Box::new(http::HttpTool));
        manager.register(Box::new(web::WebBrowserTool));
        manager.register(Box::new(exec::ExecTool));
        #[cfg(feature = "mobile")]
        {
            available_tools.push("mobile_tool".to_string());
            manager.register(Box::new(mobile::MobileTool));
        }
        if let Some(backend) = memory_backend {
            available_tools.push("memory_rw".to_string());
            manager.register(Box::new(memory::MemoryTool::new(backend, memory_default_key)));
        }
        if let Some(backend) = skills_backend {
            available_tools.push("skills_manage".to_string());
            manager.register(Box::new(skills::SkillsTool::new(backend, skills_default_name)));
        }

        manager.register(Box::new(manager::SessionResourceManagerTool::new(
            session_manager,
            memory_backend_for_manager,
            skills_backend_for_manager,
            available_tools,
        )));
        manager
    }

    pub fn register(&mut self, plugin: Box<dyn ToolPlugin>) {
        plugin.init();
        self.plugins.insert(plugin.name().to_string(), plugin);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.plugins.values().map(|p| p.definition()).collect()
    }

    pub fn shutdown(&self) {
        if self.shutdown_called.swap(true, Ordering::AcqRel) {
            return;
        }
        for plugin in self.plugins.values() {
            plugin.finit();
        }
    }

    pub async fn run_tool_calls_in_loop(
        &self,
        tool_calls: &[ToolCall],
        loop_no: Option<usize>,
        session_id: Option<&str>,
    ) -> Result<Vec<Message>> {
        let session_hint = session_id.map(|v| v.to_string());
        let tasks = tool_calls.iter().map(|call| {
            let session_hint = session_hint.clone();
            async move {
            let tool_name = call.function.name.clone();
            let tool_call_id = call.id.clone();
            let call_args = inject_session_for_manager_tool(
                &tool_name,
                &call.function.arguments,
                session_hint.as_deref(),
            );
            let args_preview = preview_text(&call_args, 600);
            log::info(format!(
                "[tool_call][start] loop={} id={} name={} args={}",
                loop_label(loop_no),
                tool_call_id,
                tool_name,
                args_preview
            ));

            let message = match self.plugins.get(&tool_name) {
                Some(plugin) => {
                    let parsed_args = parse_arguments(&call_args);
                    match parsed_args {
                        Ok(args) => match plugin.execute(args.clone()).await {
                            Ok(result) => {
                                if let Some(current_session_id) = session_hint.as_deref() {
                                    self.sync_session_loaded_after_tool_call(
                                        current_session_id,
                                        &tool_name,
                                        &args,
                                        &result,
                                    );
                                }
                                log::info(format!(
                                    "[tool_call][ok] loop={} id={} name={}",
                                    loop_label(loop_no),
                                    tool_call_id,
                                    tool_name
                                ));
                                build_tool_message(tool_call_id, tool_name, result)
                            }
                            Err(err) => {
                                log::warn(format!(
                                    "[tool_call][fail] loop={} id={} name={} error={}",
                                    loop_label(loop_no),
                                    tool_call_id,
                                    tool_name,
                                    err
                                ));
                                build_tool_error_message(
                                    tool_call_id,
                                    tool_name,
                                    format!("工具执行失败: {}", err),
                                )
                            }
                        },
                        Err(err) => {
                            log::warn(format!(
                                "[tool_call][fail] loop={} id={} name={} error={}",
                                loop_label(loop_no),
                                tool_call_id,
                                tool_name,
                                err
                            ));
                            build_tool_error_message(
                                tool_call_id,
                                tool_name,
                                format!("工具参数解析失败: {}", err),
                            )
                        }
                    }
                }
                None => {
                    let err = format!("未找到工具插件: {}", tool_name);
                    log::warn(format!(
                        "[tool_call][fail] loop={} id={} name={} error={}",
                        loop_label(loop_no),
                        tool_call_id,
                        tool_name,
                        err
                    ));
                    build_tool_error_message(tool_call_id, tool_name, err)
                }
            };

            message
            }
        });

        let mut tool_messages = Vec::with_capacity(tool_calls.len());
        for result in join_all(tasks).await {
            tool_messages.push(result);
        }
        Ok(tool_messages)
    }

    fn sync_session_loaded_after_tool_call(
        &self,
        session_id: &str,
        tool_name: &str,
        args: &Value,
        result: &Value,
    ) {
        if !result
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return;
        }

        let Ok(mut session) = self.session_manager.load_session(session_id) else {
            return;
        };

        let mut changed = false;
        match tool_name {
            "memory_rw" => {
                if let Some(key) = result.get("key").and_then(|v| v.as_str()) {
                    changed = merge_entry(&mut session.memory_loaded.entries, key) || changed;
                }
            }
            "skills_manage" => {
                let action = args
                    .get("action")
                    .and_then(|v| v.as_str())
                    .map(|v| v.trim().to_ascii_lowercase())
                    .unwrap_or_default();

                if action == "delete" {
                    if let Some(skill) = result.get("deleted").and_then(|v| v.as_str()) {
                        let before = session.skills_loaded.entries.len();
                        session.skills_loaded.entries.retain(|v| v != skill);
                        changed = changed || session.skills_loaded.entries.len() != before;
                    }
                } else if matches!(action.as_str(), "load" | "save")
                    && let Some(skill) = result.get("skill").and_then(|v| v.as_str())
                {
                    changed = merge_entry(&mut session.skills_loaded.entries, skill) || changed;
                }
            }
            _ => {}
        }

        if changed {
            let _ = self.session_manager.save_session(&session);
        }
    }
}

impl Drop for ToolManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn loop_label(loop_no: Option<usize>) -> String {
    match loop_no {
        Some(v) if v > 0 => v.to_string(),
        _ => "-".to_string(),
    }
}

fn parse_arguments(arguments: &str) -> Result<Value> {
    serde_json::from_str(arguments).context("arguments 不是合法 JSON")
}

fn inject_session_for_manager_tool(
    tool_name: &str,
    raw_args: &str,
    session_id: Option<&str>,
) -> String {
    if tool_name != "session_resource_manager" {
        return raw_args.to_string();
    }

    let Some(session_id) = session_id else {
        return raw_args.to_string();
    };

    let Ok(mut value) = serde_json::from_str::<Value>(raw_args) else {
        return raw_args.to_string();
    };

    let Value::Object(ref mut map) = value else {
        return raw_args.to_string();
    };

    let has_session = map
        .get("session")
        .and_then(|v| v.as_str())
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if has_session {
        return raw_args.to_string();
    }

    map.insert("session".to_string(), Value::String(session_id.to_string()));
    value.to_string()
}

fn merge_entry(target: &mut Vec<String>, entry: &str) -> bool {
    if entry.trim().is_empty() {
        return false;
    }
    if target.iter().any(|v| v == entry) {
        return false;
    }
    target.push(entry.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use crate::types::{ToolCall, ToolFunctionCall};
    use async_trait::async_trait;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct CapturePlugin {
        captured_args: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl ToolPlugin for CapturePlugin {
        fn name(&self) -> &'static str {
            "session_resource_manager"
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                kind: "function".to_string(),
                function: crate::types::ToolSchema {
                    name: self.name().to_string(),
                    description: "test".to_string(),
                    parameters: json!({"type": "object"}),
                },
            }
        }

        async fn execute(&self, args: Value) -> Result<Value> {
            self.captured_args
                .lock()
                .expect("capture lock poisoned")
                .push(args);
            Ok(json!({"ok": true}))
        }
    }

    fn temp_workspace_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("rustclaw-tools-test-{}", nanos))
    }

    #[tokio::test]
    async fn injects_current_session_into_manager_tool_args() {
        let root = temp_workspace_root();
        fs::create_dir_all(&root).expect("create temp workspace failed");
        let session_manager = SessionManager::new(&root).expect("create session manager failed");
        session_manager
            .create_named_session("s1", "system")
            .expect("create session failed");

        let captured_args: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let mut plugins: HashMap<String, Box<dyn ToolPlugin>> = HashMap::new();
        plugins.insert(
            "session_resource_manager".to_string(),
            Box::new(CapturePlugin {
                captured_args: captured_args.clone(),
            }),
        );

        let manager = ToolManager {
            plugins,
            session_manager,
            shutdown_called: AtomicBool::new(false),
        };

        let calls = vec![ToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: ToolFunctionCall {
                name: "session_resource_manager".to_string(),
                arguments: json!({"action": "view", "category": "all"}).to_string(),
            },
        }];

        let _ = manager
            .run_tool_calls_in_loop(&calls, Some(1), Some("s1"))
            .await
            .expect("tool call failed");

        let captured = captured_args.lock().expect("capture lock poisoned");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].get("session").and_then(|v| v.as_str()), Some("s1"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_memory_and_skills_loaded_after_tool_call() {
        let root = temp_workspace_root();
        fs::create_dir_all(&root).expect("create temp workspace failed");
        let session_manager = SessionManager::new(&root).expect("create session manager failed");
        session_manager
            .create_named_session("s1", "system")
            .expect("create session failed");

        let manager = ToolManager {
            plugins: HashMap::new(),
            session_manager: session_manager.clone(),
            shutdown_called: AtomicBool::new(false),
        };

        manager.sync_session_loaded_after_tool_call(
            "s1",
            "memory_rw",
            &json!({"action": "read", "key": "facts.md"}),
            &json!({"ok": true, "key": "facts.md"}),
        );

        manager.sync_session_loaded_after_tool_call(
            "s1",
            "skills_manage",
            &json!({"action": "load", "skill": "planner"}),
            &json!({"ok": true, "skill": "planner"}),
        );

        manager.sync_session_loaded_after_tool_call(
            "s1",
            "skills_manage",
            &json!({"action": "delete", "skill": "planner"}),
            &json!({"ok": true, "deleted": "planner"}),
        );

        let saved = session_manager.load_session("s1").expect("load session failed");
        assert!(saved.memory_loaded.entries.iter().any(|v| v == "facts.md"));
        assert!(!saved.skills_loaded.entries.iter().any(|v| v == "planner"));

        let _ = fs::remove_dir_all(&root);
    }
}

fn build_tool_message(tool_call_id: String, tool_name: String, payload: Value) -> Message {
    Message {
        role: "tool".to_string(),
        content: Some(payload.to_string()),
        tool_calls: None,
        tool_call_id: Some(tool_call_id),
        name: Some(tool_name),
    }
}

fn build_tool_error_message(tool_call_id: String, tool_name: String, error: String) -> Message {
    build_tool_message(
        tool_call_id,
        tool_name,
        json!({
            "ok": false,
            "error": error,
        }),
    )
}

fn preview_text(input: &str, max: usize) -> String {
    let cleaned = input.replace('\n', "\\n");
    truncate_utf8(&cleaned, max)
}

pub(crate) fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }

    let end = s
        .char_indices()
        .take_while(|(idx, _)| *idx <= max)
        .map(|(idx, _)| idx)
        .last()
        .unwrap_or(0);
    format!("{}...(truncated)", &s[..end])
}
