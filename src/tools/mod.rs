use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::log;
use crate::memory::MemoryBackend;
use crate::skills::SkillsBackend;
use crate::types::{Message, ToolCall, ToolDefinition};

pub mod http;
pub mod web;
pub mod exec;
pub mod memory;
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
}

impl ToolManager {
    pub fn with_builtin_plugins(
        memory_backend: Option<Arc<dyn MemoryBackend>>,
        memory_default_key: String,
        skills_backend: Option<Arc<dyn SkillsBackend>>,
        skills_default_name: String,
    ) -> Self {
        let mut manager = Self {
            plugins: HashMap::new(),
        };
        manager.register(Box::new(time::TimeTool));
        manager.register(Box::new(http::HttpTool));
        manager.register(Box::new(web::WebBrowserTool));
        manager.register(Box::new(exec::ExecTool));
        if let Some(backend) = memory_backend {
            manager.register(Box::new(memory::MemoryTool::new(backend, memory_default_key)));
        }
        if let Some(backend) = skills_backend {
            manager.register(Box::new(skills::SkillsTool::new(backend, skills_default_name)));
        }
        manager
    }

    pub fn register(&mut self, plugin: Box<dyn ToolPlugin>) {
        plugin.init();
        self.plugins.insert(plugin.name().to_string(), plugin);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.plugins.values().map(|p| p.definition()).collect()
    }

    pub async fn run_tool_calls_in_loop(
        &self,
        tool_calls: &[ToolCall],
        loop_no: Option<usize>,
    ) -> Result<Vec<Message>> {
        let tasks = tool_calls.iter().map(|call| async move {
            let tool_name = call.function.name.clone();
            let tool_call_id = call.id.clone();
            let args_preview = preview_text(&call.function.arguments, 600);
            log::info(format!(
                "[tool_call][start] loop={} id={} name={} args={}",
                loop_label(loop_no),
                tool_call_id,
                tool_name,
                args_preview
            ));

            let message = match self.plugins.get(&tool_name) {
                Some(plugin) => {
                    let parsed_args = parse_arguments(&call.function.arguments);
                    match parsed_args {
                        Ok(args) => match plugin.execute(args).await {
                            Ok(result) => {
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
        });

        let mut tool_messages = Vec::with_capacity(tool_calls.len());
        for result in join_all(tasks).await {
            tool_messages.push(result);
        }
        Ok(tool_messages)
    }
}

impl Drop for ToolManager {
    fn drop(&mut self) {
        for plugin in self.plugins.values() {
            plugin.finit();
        }
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
