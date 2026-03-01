use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;

use crate::memory::{MemoryBackend, MemoryWriteMode};

pub struct MarkdownMemoryBackend {
    base_dir: PathBuf,
}

impl MarkdownMemoryBackend {
    pub async fn new(base_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&base_dir)
            .await
            .with_context(|| format!("创建 memory 目录失败: {}", base_dir.display()))?;
        Ok(Self { base_dir })
    }

    fn file_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(key)
    }
}

#[async_trait]
impl MemoryBackend for MarkdownMemoryBackend {
    fn provider_name(&self) -> &'static str {
        "markdown"
    }

    async fn read(&self, key: &str) -> Result<String> {
        let path = self.file_path(key);
        if !path.exists() {
            return Ok(String::new());
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("读取 memory 失败: {}", path.display()))?;
        Ok(content)
    }

    async fn write(&self, key: &str, content: &str, mode: MemoryWriteMode) -> Result<()> {
        let path = self.file_path(key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("创建 memory 子目录失败: {}", parent.display()))?;
        }

        match mode {
            MemoryWriteMode::Overwrite => {
                tokio::fs::write(&path, content)
                    .await
                    .with_context(|| format!("写入 memory 失败: {}", path.display()))?;
            }
            MemoryWriteMode::Append => {
                use tokio::io::AsyncWriteExt;
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                    .with_context(|| format!("追加打开 memory 失败: {}", path.display()))?;
                file.write_all(content.as_bytes())
                    .await
                    .with_context(|| format!("追加写入 memory 失败: {}", path.display()))?;
            }
        }

        Ok(())
    }
}
