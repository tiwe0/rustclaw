use anyhow::Result;
use async_trait::async_trait;

use crate::app;
use crate::conversation::{ConversationInterface, ConversationMode};

pub struct TuiConversation;

#[async_trait]
impl ConversationInterface for TuiConversation {
    fn mode(&self) -> ConversationMode {
        ConversationMode::Tui
    }

    async fn run(&self) -> Result<()> {
        app::run().await
    }
}
