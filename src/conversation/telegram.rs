use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::channel;
use crate::config::{load_config, resolve_config_path};
use crate::conversation::ConversationInterface;

pub struct TelegramConversation;

#[async_trait]
impl ConversationInterface for TelegramConversation {
    async fn run(&self) -> Result<()> {
        let config_path = resolve_config_path();
        let cfg = load_config(&config_path)?;
        if !cfg.channel.enabled {
            return Err(anyhow::anyhow!(
                "channel 已禁用，请在 config.toml 中设置 [channel].enabled = true"
            ));
        }

        channel::telegram::run(cfg.channel.telegram)
            .await
            .context("启动 telegram 对话实现失败")
    }
}
