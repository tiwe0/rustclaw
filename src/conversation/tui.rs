use anyhow::Result;
use async_trait::async_trait;

use crate::app;
use crate::conversation::ConversationInterface;

pub struct TuiConversation;

#[async_trait]
impl ConversationInterface for TuiConversation {
    async fn run(&self) -> Result<()> {
        app::run().await
    }
}
