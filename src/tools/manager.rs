use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::memory::MemoryBackend;
use crate::interrupt;
use crate::session::SessionManager;
use crate::skills::SkillsBackend;
use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

pub struct SessionResourceManagerTool {
    session_manager: SessionManager,
    memory_backend: Option<Arc<dyn MemoryBackend>>,
    skills_backend: Option<Arc<dyn SkillsBackend>>,
    available_tools: Vec<String>,
}

impl SessionResourceManagerTool {
    pub fn new(
        session_manager: SessionManager,
        memory_backend: Option<Arc<dyn MemoryBackend>>,
        skills_backend: Option<Arc<dyn SkillsBackend>>,
        available_tools: Vec<String>,
    ) -> Self {
        Self {
            session_manager,
            memory_backend,
            skills_backend,
            available_tools,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ManagerArgs {
    action: String,
    category: Option<String>,
    session: Option<String>,
    item: Option<String>,
    items: Option<Vec<String>>,
    keyword: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum ResourceCategory {
    Memory,
    Skills,
    Tools,
}

#[async_trait]
impl ToolPlugin for SessionResourceManagerTool {
    fn name(&self) -> &'static str {
        "session_resource_manager"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "动态管理当前 session 的 memory/skills/tools 加载状态：load/remove/view/search。"
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["load", "remove", "view", "search", "interrupt", "close"],
                            "description": "执行动作"
                        },
                        "category": {
                            "type": "string",
                            "enum": ["memory", "skills", "tools", "all"],
                            "description": "资源类型，view/search 可用 all"
                        },
                        "session": {
                            "type": "string",
                            "description": "会话 ID，不传则使用当前 active session"
                        },
                        "item": {
                            "type": "string",
                            "description": "单个资源名"
                        },
                        "items": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "多个资源名"
                        },
                        "keyword": {
                            "type": "string",
                            "description": "search 时的关键字"
                        }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let parsed: ManagerArgs = serde_json::from_value(args).context("解析 manager 参数失败")?;
        let action = parsed.action.trim().to_lowercase();

        let session_id = resolve_session_id(&self.session_manager, parsed.session.as_deref())?;
        let mut session = self.session_manager.load_session(&session_id)?;

        match action.as_str() {
            "interrupt" | "close" => {
                interrupt::cancel_session(&session_id);
                Ok(json!({
                    "ok": true,
                    "action": action,
                    "session": session_id,
                    "interrupted": true
                }))
            }
            "load" => {
                let category = parse_category(parsed.category.as_deref())?;
                let category = category.ok_or_else(|| anyhow::anyhow!("load 需要 category"))?;
                if matches!(category, ResourceCategory::Memory | ResourceCategory::Skills | ResourceCategory::Tools)
                {
                    let names = collect_items(parsed.item.as_deref(), parsed.items.as_deref())?;
                    let target = loaded_entries_mut(&mut session, category);
                    let before = target.len();
                    merge_unique(target, &names);
                    let loaded = target.clone();
                    let added = loaded.len().saturating_sub(before);
                    self.session_manager.save_session(&session)?;
                    Ok(json!({
                        "ok": true,
                        "action": "load",
                        "session": session_id,
                        "category": category_name(category),
                        "loaded": loaded,
                        "added": added
                    }))
                } else {
                    Err(anyhow::anyhow!("load 不支持 category=all"))
                }
            }
            "remove" => {
                let category = parse_category(parsed.category.as_deref())?;
                let category = category.ok_or_else(|| anyhow::anyhow!("remove 需要 category"))?;
                if matches!(category, ResourceCategory::Memory | ResourceCategory::Skills | ResourceCategory::Tools)
                {
                    let names = collect_items(parsed.item.as_deref(), parsed.items.as_deref())?;
                    let target = loaded_entries_mut(&mut session, category);
                    let before = target.len();
                    target.retain(|v| !names.iter().any(|n| n == v));
                    let loaded = target.clone();
                    let removed = before.saturating_sub(loaded.len());
                    self.session_manager.save_session(&session)?;
                    Ok(json!({
                        "ok": true,
                        "action": "remove",
                        "session": session_id,
                        "category": category_name(category),
                        "loaded": loaded,
                        "removed": removed
                    }))
                } else {
                    Err(anyhow::anyhow!("remove 不支持 category=all"))
                }
            }
            "view" => {
                let category = parse_category(parsed.category.as_deref())?;
                let data = self.build_view_payload(&session, category).await?;
                Ok(json!({
                    "ok": true,
                    "action": "view",
                    "session": session_id,
                    "data": data
                }))
            }
            "search" => {
                let keyword = parsed
                    .keyword
                    .as_deref()
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("search 需要 keyword"))?;
                let category = parse_category(parsed.category.as_deref())?;
                let data = self.build_view_payload(&session, category).await?;
                Ok(json!({
                    "ok": true,
                    "action": "search",
                    "session": session_id,
                    "keyword": keyword,
                    "data": filter_payload_by_keyword(data, &keyword)
                }))
            }
            _ => Err(anyhow::anyhow!(
                "未知 action: {}，支持 load/remove/view/search/interrupt/close",
                action
            )),
        }
    }
}

impl SessionResourceManagerTool {
    async fn build_view_payload(
        &self,
        session: &crate::session::ChatSession,
        category: Option<ResourceCategory>,
    ) -> Result<Value> {
        let mut map = serde_json::Map::new();

        if category.is_none() || matches!(category, Some(ResourceCategory::Memory)) {
            let loaded = session.memory_loaded.entries.clone();
            let available = match &self.memory_backend {
                Some(backend) => backend.list().await?,
                None => Vec::new(),
            };
            map.insert(
                "memory".to_string(),
                json!(build_loaded_unloaded(loaded, available)),
            );
        }

        if category.is_none() || matches!(category, Some(ResourceCategory::Skills)) {
            let loaded = session.skills_loaded.entries.clone();
            let available = match &self.skills_backend {
                Some(backend) => backend.list().await?,
                None => Vec::new(),
            };
            map.insert(
                "skills".to_string(),
                json!(build_loaded_unloaded(loaded, available)),
            );
        }

        if category.is_none() || matches!(category, Some(ResourceCategory::Tools)) {
            let loaded = session.tools_loaded.entries.clone();
            let available = self.available_tools.clone();
            map.insert(
                "tools".to_string(),
                json!(build_loaded_unloaded(loaded, available)),
            );
        }

        Ok(Value::Object(map))
    }
}

fn resolve_session_id(session_manager: &SessionManager, requested: Option<&str>) -> Result<String> {
    if let Some(raw) = requested {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    session_manager
        .active_session_id()?
        .ok_or_else(|| anyhow::anyhow!("无 active session，请显式传 session"))
}

fn parse_category(raw: Option<&str>) -> Result<Option<ResourceCategory>> {
    let Some(raw) = raw else {
        return Ok(None);
    };

    match raw.trim().to_lowercase().as_str() {
        "memory" => Ok(Some(ResourceCategory::Memory)),
        "skills" => Ok(Some(ResourceCategory::Skills)),
        "tools" => Ok(Some(ResourceCategory::Tools)),
        "all" => Ok(None),
        _ => Err(anyhow::anyhow!("未知 category: {}，支持 memory/skills/tools/all", raw)),
    }
}

fn category_name(category: ResourceCategory) -> &'static str {
    match category {
        ResourceCategory::Memory => "memory",
        ResourceCategory::Skills => "skills",
        ResourceCategory::Tools => "tools",
    }
}

fn collect_items(item: Option<&str>, items: Option<&[String]>) -> Result<Vec<String>> {
    let mut out = Vec::new();
    if let Some(item) = item {
        let trimmed = item.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }

    if let Some(items) = items {
        for item in items {
            let trimmed = item.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
    }

    if out.is_empty() {
        return Err(anyhow::anyhow!("至少提供 item 或 items"));
    }

    out.sort();
    out.dedup();
    Ok(out)
}

fn loaded_entries_mut(
    session: &mut crate::session::ChatSession,
    category: ResourceCategory,
) -> &mut Vec<String> {
    match category {
        ResourceCategory::Memory => &mut session.memory_loaded.entries,
        ResourceCategory::Skills => &mut session.skills_loaded.entries,
        ResourceCategory::Tools => &mut session.tools_loaded.entries,
    }
}

#[derive(serde::Serialize)]
struct LoadedUnloaded {
    loaded: Vec<String>,
    unloaded: Vec<String>,
    available_total: usize,
}

fn build_loaded_unloaded(mut loaded: Vec<String>, mut available: Vec<String>) -> LoadedUnloaded {
    loaded.sort();
    loaded.dedup();
    available.sort();
    available.dedup();

    let unloaded = available
        .iter()
        .filter(|name| !loaded.iter().any(|loaded_name| loaded_name == *name))
        .cloned()
        .collect::<Vec<_>>();

    LoadedUnloaded {
        loaded,
        unloaded,
        available_total: available.len(),
    }
}

fn merge_unique(target: &mut Vec<String>, source: &[String]) {
    for name in source {
        if !target.iter().any(|v| v == name) {
            target.push(name.clone());
        }
    }
    target.sort();
}

fn filter_payload_by_keyword(payload: Value, keyword: &str) -> Value {
    let Value::Object(mut obj) = payload else {
        return payload;
    };

    for value in obj.values_mut() {
        if let Value::Object(category_map) = value {
            for key in ["loaded", "unloaded"] {
                if let Some(Value::Array(items)) = category_map.get_mut(key) {
                    let filtered = items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .filter(|name| name.to_lowercase().contains(keyword))
                        .map(|name| Value::String(name.to_string()))
                        .collect::<Vec<_>>();
                    *items = filtered;
                }
            }
        }
    }

    Value::Object(obj)
}
