use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::skills::{resolve_skill_name, SkillWriteMode, SkillsBackend};
use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

pub struct SkillsTool {
    backend: Arc<dyn SkillsBackend>,
    default_skill: String,
}

impl SkillsTool {
    pub fn new(backend: Arc<dyn SkillsBackend>, default_skill: String) -> Self {
        Self {
            backend,
            default_skill,
        }
    }
}

#[derive(Debug, Deserialize)]
struct SkillsArgs {
    action: String,
    skill: Option<String>,
    content: Option<String>,
    mode: Option<String>,
}

#[async_trait]
impl ToolPlugin for SkillsTool {
    fn name(&self) -> &'static str {
        "skills_manage"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: "skills_manage".to_string(),
                description: "管理技能片段：list/load/save/delete，持久化为 markdown。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["list", "load", "save", "delete"],
                            "description": "执行动作"
                        },
                        "skill": {
                            "type": "string",
                            "description": "技能名称（可省略，使用默认 skill）"
                        },
                        "content": {
                            "type": "string",
                            "description": "保存技能时的内容"
                        },
                        "mode": {
                            "type": "string",
                            "enum": ["overwrite", "append"],
                            "description": "保存模式，默认 overwrite"
                        }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let parsed: SkillsArgs = serde_json::from_value(args).context("解析 skills 参数失败")?;
        let action = parsed.action.to_lowercase();

        match action.as_str() {
            "list" => {
                let skills = self.backend.list().await?;
                Ok(json!({
                    "ok": true,
                    "provider": self.backend.provider_name(),
                    "count": skills.len(),
                    "skills": skills
                }))
            }
            "load" => {
                let skill = resolve_skill_name(parsed.skill.as_deref(), &self.default_skill)?;
                let content = self.backend.load(&skill).await?;
                Ok(json!({
                    "ok": true,
                    "skill": skill,
                    "content": content
                }))
            }
            "save" => {
                let skill = resolve_skill_name(parsed.skill.as_deref(), &self.default_skill)?;
                let content = parsed
                    .content
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("save 动作缺少 content 字段"))?;

                let mode = match parsed
                    .mode
                    .as_deref()
                    .unwrap_or("overwrite")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "append" => SkillWriteMode::Append,
                    _ => SkillWriteMode::Overwrite,
                };

                self.backend.save(&skill, content, mode).await?;
                Ok(json!({
                    "ok": true,
                    "skill": skill,
                    "mode": match mode {
                        SkillWriteMode::Overwrite => "overwrite",
                        SkillWriteMode::Append => "append",
                    }
                }))
            }
            "delete" => {
                let skill = resolve_skill_name(parsed.skill.as_deref(), &self.default_skill)?;
                self.backend.delete(&skill).await?;
                Ok(json!({
                    "ok": true,
                    "deleted": skill
                }))
            }
            _ => Err(anyhow::anyhow!(
                "未知 action: {}，支持 list/load/save/delete",
                action
            )),
        }
    }
}
