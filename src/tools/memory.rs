use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::memory::{resolve_memory_key, MemoryBackend, MemoryWriteMode};
use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

pub struct MemoryTool {
    backend: Arc<dyn MemoryBackend>,
    default_key: String,
}

impl MemoryTool {
    pub fn new(backend: Arc<dyn MemoryBackend>, default_key: String) -> Self {
        Self {
            backend,
            default_key,
        }
    }
}

#[async_trait]
impl ToolPlugin for MemoryTool {
    fn name(&self) -> &'static str {
        "memory_rw"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "读写记忆（可扩展后端，当前为 markdown 存储）。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "read/write/append" },
                        "key": { "type": "string", "description": "记忆键（文件名），默认来自配置" },
                        "content": { "type": "string", "description": "写入内容，write/append 必填" }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase())
            .context("memory_rw 缺少 action")?;

        let requested_key = args.get("key").and_then(|v| v.as_str());
        let key = resolve_memory_key(requested_key, &self.default_key)?;

        match action.as_str() {
            "read" => {
                let content = self.backend.read(&key).await?;
                Ok(json!({
                    "ok": true,
                    "provider": self.backend.provider_name(),
                    "key": key,
                    "content": content
                }))
            }
            "write" | "append" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .context("write/append 需要 content")?;
                let mode = if action == "append" {
                    MemoryWriteMode::Append
                } else {
                    MemoryWriteMode::Overwrite
                };
                self.backend.write(&key, content, mode).await?;
                Ok(json!({
                    "ok": true,
                    "provider": self.backend.provider_name(),
                    "action": action,
                    "key": key,
                    "bytes": content.len()
                }))
            }
            _ => Ok(json!({
                "ok": false,
                "error": "action 仅支持 read/write/append"
            })),
        }
    }
}
