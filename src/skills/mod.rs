use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::SkillsConfig;

pub mod markdown;

#[derive(Debug, Clone, Copy)]
pub enum SkillWriteMode {
    Overwrite,
    Append,
}

#[async_trait]
pub trait SkillsBackend: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn list(&self) -> Result<Vec<String>>;
    async fn load(&self, skill: &str) -> Result<String>;
    async fn save(&self, skill: &str, content: &str, mode: SkillWriteMode) -> Result<()>;
    async fn delete(&self, skill: &str) -> Result<()>;
}

pub async fn create_skills_backend(
    cfg: &SkillsConfig,
    app_base_dir: &Path,
) -> Result<Option<Arc<dyn SkillsBackend>>> {
    if !cfg.enabled {
        return Ok(None);
    }

    let base_dir = normalize_base_dir(app_base_dir, &cfg.base_dir)?;
    let provider = cfg.provider.to_lowercase();

    let backend: Arc<dyn SkillsBackend> = match provider.as_str() {
        "markdown" | "md" => Arc::new(markdown::MarkdownSkillsBackend::new(base_dir).await?),
        _ => {
            return Err(anyhow::anyhow!(
                "不支持的 skills provider: {}，当前仅支持 markdown",
                cfg.provider
            ))
        }
    };

    Ok(Some(backend))
}

fn normalize_base_dir(app_base_dir: &Path, dir: &str) -> Result<PathBuf> {
    let path = PathBuf::from(dir);
    if path.is_absolute() {
        return Err(anyhow::anyhow!(
            "skills.base_dir 必须是相对路径（将基于 [base].base_dir 解析）"
        ));
    }
    Ok(app_base_dir.join(path))
}

pub fn sanitize_skill_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("skill 名称不能为空"));
    }

    let mut output = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '/' {
            output.push(ch);
        } else {
            output.push('_');
        }
    }

    let output = output.trim_matches('/');
    if output.is_empty() {
        return Err(anyhow::anyhow!("skill 名称非法"));
    }
    if output.contains("..") {
        return Err(anyhow::anyhow!("skill 名称不能包含 .."));
    }

    Ok(output.to_string())
}

pub fn ensure_md_extension(skill: &str) -> String {
    if skill.ends_with(".md") {
        skill.to_string()
    } else {
        format!("{}.md", skill)
    }
}

pub fn resolve_skill_name(requested: Option<&str>, default_skill: &str) -> Result<String> {
    let raw = requested.unwrap_or(default_skill);
    let sanitized = sanitize_skill_name(raw).context("校验 skill 名称失败")?;
    Ok(ensure_md_extension(&sanitized))
}
