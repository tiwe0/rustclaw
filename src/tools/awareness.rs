use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{load_config, resolve_app_base_dir, resolve_config_path};
use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

const DEFAULT_AWARENESS_FILE: &str = "awareness.md";

pub struct AwarenessTool;

#[derive(Debug, Deserialize)]
struct AwarenessArgs {
    action: String,
    content: Option<String>,
    mode: Option<String>,
}

#[async_trait]
impl ToolPlugin for AwarenessTool {
    fn name(&self) -> &'static str {
        "awareness"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "用于 LLM agent 获取和同步自我认知：get/sync。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["get", "sync"],
                            "description": "执行动作：get 读取自我认知，sync 同步更新自我认知"
                        },
                        "content": {
                            "type": "string",
                            "description": "sync 时要写入的认知内容"
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["replace", "append"],
                            "description": "sync 写入模式，默认 replace"
                        }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let parsed: AwarenessArgs = serde_json::from_value(args).context("解析 awareness 参数失败")?;
        let action = parsed.action.trim().to_ascii_lowercase();
        let awareness_path = resolve_awareness_path()?;

        match action.as_str() {
            "get" => {
                let content = read_awareness(&awareness_path)?;
                Ok(json!({
                    "ok": true,
                    "action": "get",
                    "path": awareness_path.display().to_string(),
                    "content": content,
                    "empty": content.trim().is_empty()
                }))
            }
            "sync" => {
                let new_content = parsed
                    .content
                    .as_deref()
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("sync 需要非空 content"))?;
                let mode = parsed
                    .mode
                    .as_deref()
                    .map(|v| v.trim().to_ascii_lowercase())
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "replace".to_string());

                let merged = match mode.as_str() {
                    "replace" => new_content.to_string(),
                    "append" => {
                        let old = read_awareness(&awareness_path)?;
                        if old.trim().is_empty() {
                            new_content.to_string()
                        } else {
                            format!("{}\n\n{}", old.trim_end(), new_content)
                        }
                    }
                    _ => {
                        return Err(anyhow::anyhow!(
                            "未知 mode: {}，支持 replace/append",
                            mode
                        ))
                    }
                };

                write_awareness(&awareness_path, &merged)?;

                Ok(json!({
                    "ok": true,
                    "action": "sync",
                    "mode": mode,
                    "path": awareness_path.display().to_string(),
                    "bytes": merged.len(),
                    "content": merged
                }))
            }
            _ => Err(anyhow::anyhow!("未知 action: {}，支持 get/sync", action)),
        }
    }
}

fn resolve_awareness_path() -> Result<PathBuf> {
    let config_path = resolve_config_path();
    let cfg = load_config(&config_path)?;
    let workspace_root = env::current_dir().context("获取当前工作目录失败")?;
    let app_base_dir = resolve_app_base_dir(&workspace_root, &cfg.base);
    Ok(app_base_dir.join(DEFAULT_AWARENESS_FILE))
}

fn read_awareness(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).with_context(|| format!("读取 awareness 失败: {}", path.display()))
}

fn write_awareness(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 awareness 目录失败: {}", parent.display()))?;
    }

    fs::write(path, content).with_context(|| format!("写入 awareness 失败: {}", path.display()))
}
