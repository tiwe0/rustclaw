use anyhow::Result;
use async_trait::async_trait;

use crate::config::{load_config, resolve_config_path};

pub mod telegram;
pub mod tui;

#[derive(Debug, Clone, Copy)]
pub enum ConversationMode {
    Tui,
    Telegram,
}

impl ConversationMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "tui" => Some(Self::Tui),
            "telegram" => Some(Self::Telegram),
            _ => None,
        }
    }
}

#[async_trait]
pub trait ConversationInterface: Send + Sync {
    fn mode(&self) -> ConversationMode;
    async fn run(&self) -> Result<()>;
}

pub async fn run_mode(mode: ConversationMode) -> Result<()> {
    let impl_obj: Box<dyn ConversationInterface> = match mode {
        ConversationMode::Tui => Box::new(tui::TuiConversation),
        ConversationMode::Telegram => Box::new(telegram::TelegramConversation),
    };

    println!("[conversation] running mode: {:?}", impl_obj.mode());
    impl_obj.run().await
}

pub async fn run_configured_channel() -> Result<()> {
    let config_path = resolve_config_path();
    let cfg = load_config(&config_path)?;
    let raw = cfg.channel.provider;
    let mode = ConversationMode::parse(&raw)
        .ok_or_else(|| anyhow::anyhow!("不支持的 channel provider: {}", raw))?;
    run_mode(mode).await
}
