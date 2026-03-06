use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::MemoryConfig;

pub mod markdown;

#[derive(Debug, Clone, Copy)]
pub enum MemoryWriteMode {
    Overwrite,
    Append,
}

#[async_trait]
pub trait MemoryBackend: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn list(&self) -> Result<Vec<String>>;
    async fn read(&self, key: &str) -> Result<String>;
    async fn write(&self, key: &str, content: &str, mode: MemoryWriteMode) -> Result<()>;
}

pub async fn create_memory_backend(
    cfg: &MemoryConfig,
    app_base_dir: &Path,
) -> Result<Option<Arc<dyn MemoryBackend>>> {
    if !cfg.enabled {
        return Ok(None);
    }

    let base_dir = normalize_base_dir(app_base_dir, &cfg.base_dir)?;
    let provider = cfg.provider.to_lowercase();

    let backend: Arc<dyn MemoryBackend> = match provider.as_str() {
        "markdown" | "md" => Arc::new(markdown::MarkdownMemoryBackend::new(base_dir).await?),
        _ => {
            return Err(anyhow::anyhow!(
                "不支持的 memory provider: {}，当前仅支持 markdown",
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
            "memory.base_dir 必须是相对路径（将基于 [base].base_dir 解析）"
        ));
    }
    Ok(app_base_dir.join(path))
}

pub fn sanitize_key(key: &str) -> Result<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("memory key 不能为空"));
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
        return Err(anyhow::anyhow!("memory key 非法"));
    }
    if output.contains("..") {
        return Err(anyhow::anyhow!("memory key 不能包含 .."));
    }

    Ok(output.to_string())
}

pub fn ensure_md_extension(key: &str) -> String {
    if key.ends_with(".md") {
        key.to_string()
    } else {
        format!("{}.md", key)
    }
}

pub fn resolve_memory_key(requested: Option<&str>, default_key: &str) -> Result<String> {
    let raw = requested.unwrap_or(default_key);
    let sanitized = sanitize_key(raw).context("校验 memory key 失败")?;
    Ok(ensure_md_extension(&sanitized))
}
