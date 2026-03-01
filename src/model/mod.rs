use anyhow::{Result, anyhow};
use async_trait::async_trait;
use std::sync::Arc;

use crate::config::{DEFAULT_MODEL, ModelConfig, resolve_base_url};
use crate::types::{AssistantReply, Message, StreamResult, ToolDefinition};

pub mod deepseek;
pub mod openai;

const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-mini";

#[async_trait]
pub trait ChatModel: Send + Sync {
    async fn chat_once(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<AssistantReply>;

    async fn stream_chat_collect(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<StreamResult>;
}

pub fn create_model_provider(config: &ModelConfig) -> Result<Arc<dyn ChatModel>> {
    let backend = config.backend.trim().to_lowercase();
    let base_url = resolve_base_url(config)?;
    let model_name = if config.name.trim().is_empty() {
        match backend.as_str() {
            "deepseek" => DEFAULT_MODEL.to_string(),
            "openai" => OPENAI_DEFAULT_MODEL.to_string(),
            _ => DEFAULT_MODEL.to_string(),
        }
    } else {
        config.name.clone()
    };

    match backend.as_str() {
        "deepseek" => Ok(Arc::new(deepseek::DeepSeekModel::new(
            config.api_key.clone(),
            base_url,
            model_name,
        ))),
        "openai" => Ok(Arc::new(openai::OpenAIModel::new(
            config.api_key.clone(),
            base_url,
            model_name,
        ))),
        _ => Err(anyhow!("不支持的模型后端: {}", config.backend)),
    }
}
