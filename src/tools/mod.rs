use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::future::join_all;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::memory::MemoryBackend;
use crate::skills::SkillsBackend;
use crate::types::{Message, ToolCall, ToolDefinition};

pub mod http;
pub mod exec;
pub mod memory;
pub mod skills;
pub mod time;

#[async_trait]
pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &'static str;
    fn definition(&self) -> ToolDefinition;
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
        self.plugins.insert(plugin.name().to_string(), plugin);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.plugins.values().map(|p| p.definition()).collect()
    }

    pub async fn run_tool_calls(&self, tool_calls: &[ToolCall]) -> Result<Vec<Message>> {
        let tasks = tool_calls.iter().map(|call| async move {
            let plugin = self
                .plugins
                .get(&call.function.name)
                .with_context(|| format!("未找到工具插件: {}", call.function.name))?;
            let args = parse_arguments(&call.function.arguments);
            let result = plugin.execute(args).await?;
            Ok::<Message, anyhow::Error>(Message {
                role: "tool".to_string(),
                content: Some(result.to_string()),
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
                name: Some(call.function.name.clone()),
            })
        });

        let mut tool_messages = Vec::with_capacity(tool_calls.len());
        for result in join_all(tasks).await {
            tool_messages.push(result?);
        }
        Ok(tool_messages)
    }
}

fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}
