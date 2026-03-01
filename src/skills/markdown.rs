use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;

use crate::skills::{SkillWriteMode, SkillsBackend};

pub struct MarkdownSkillsBackend {
    base_dir: PathBuf,
}

impl MarkdownSkillsBackend {
    pub async fn new(base_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&base_dir)
            .await
            .with_context(|| format!("创建 skills 目录失败: {}", base_dir.display()))?;
        Ok(Self { base_dir })
    }

    fn file_path(&self, skill: &str) -> PathBuf {
        self.base_dir.join(skill)
    }
}

#[async_trait]
impl SkillsBackend for MarkdownSkillsBackend {
    fn provider_name(&self) -> &'static str {
        "markdown"
    }

    async fn list(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let mut dir = tokio::fs::read_dir(&self.base_dir)
            .await
            .with_context(|| format!("读取 skills 目录失败: {}", self.base_dir.display()))?;

        while let Some(entry) = dir.next_entry().await.context("遍历 skills 目录失败")? {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) == Some("md") {
                if let Some(file_name) = path.file_name().and_then(|v| v.to_str()) {
                    names.push(file_name.to_string());
                }
            }
        }

        names.sort();
        Ok(names)
    }

    async fn load(&self, skill: &str) -> Result<String> {
        let path = self.file_path(skill);
        if !path.exists() {
            return Ok(String::new());
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("读取 skill 失败: {}", path.display()))?;
        Ok(content)
    }

    async fn save(&self, skill: &str, content: &str, mode: SkillWriteMode) -> Result<()> {
        let path = self.file_path(skill);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("创建 skill 子目录失败: {}", parent.display()))?;
        }

        match mode {
            SkillWriteMode::Overwrite => {
                tokio::fs::write(&path, content)
                    .await
                    .with_context(|| format!("写入 skill 失败: {}", path.display()))?;
            }
            SkillWriteMode::Append => {
                use tokio::io::AsyncWriteExt;
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await
                    .with_context(|| format!("追加打开 skill 失败: {}", path.display()))?;
                file.write_all(content.as_bytes())
                    .await
                    .with_context(|| format!("追加写入 skill 失败: {}", path.display()))?;
            }
        }

        Ok(())
    }

    async fn delete(&self, skill: &str) -> Result<()> {
        let path = self.file_path(skill);
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .with_context(|| format!("删除 skill 失败: {}", path.display()))?;
        }
        Ok(())
    }
}
