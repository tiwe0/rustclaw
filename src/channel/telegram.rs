use anyhow::{Context, Result};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatId, MessageId};
use teloxide::update_listeners;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

use crate::app;
use crate::config::TelegramChannelConfig;

const TELEGRAM_TEXT_LIMIT: usize = 3900;
const STREAM_EDIT_INTERVAL_MS: u64 = 700;
const TELEGRAM_DEFAULT_API_BASE_URL: &str = "https://api.telegram.org";
const INPUT_PREVIEW_CHARS: usize = 80;

pub async fn run(cfg: TelegramChannelConfig) -> Result<()> {
    if cfg.bot_token.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "telegram bot_token 为空，请在 [channel.telegram] 中配置"
        ));
    }

    let cfg = Arc::new(cfg);

    println!(
        "[channel/telegram] started with teloxide: poll={}ms timeout={}s api_base_url={}",
        cfg.poll_interval_ms, cfg.long_poll_timeout_secs
        , cfg.api_base_url
    );
    println!(
        "[channel/telegram] chat_id filter: {}",
        cfg.chat_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "<disabled>".to_string())
    );

    let bot = build_bot(&cfg)?;
    let listener = build_polling_listener(bot.clone(), &cfg).await;
    teloxide::repl_with_listener(bot, move |bot: Bot, msg: teloxide::types::Message| {
        let cfg = cfg.clone();
        async move {
            if let Err(err) = handle_message(bot, msg, cfg).await {
                eprintln!("[channel/telegram] handle_message error: {}", err);
            }
            respond(())
        }
    }, listener)
    .await;

    Ok(())
}

fn build_bot(cfg: &TelegramChannelConfig) -> Result<Bot> {
    let mut bot = Bot::new(cfg.bot_token.clone());
    let raw = cfg.api_base_url.trim();
    if !raw.is_empty() && raw.trim_end_matches('/') != TELEGRAM_DEFAULT_API_BASE_URL {
        let parsed = reqwest::Url::parse(raw)
            .with_context(|| format!("非法 channel.telegram.api_base_url: {}", raw))?;
        println!("[channel/telegram] use custom api_base_url: {}", raw);
        bot = bot.set_api_url(parsed);
    }
    Ok(bot)
}

async fn build_polling_listener(bot: Bot, cfg: &TelegramChannelConfig) -> update_listeners::Polling<Bot> {
    let timeout_secs = normalized_long_poll_timeout_secs(cfg.long_poll_timeout_secs);
    let retry_interval = normalized_poll_interval(cfg.poll_interval_ms);

    update_listeners::Polling::builder(bot)
        .timeout(Duration::from_secs(timeout_secs))
        .backoff_strategy(move |_| retry_interval)
        .delete_webhook()
        .await
        .build()
}

fn normalized_long_poll_timeout_secs(raw: u64) -> u64 {
    raw.clamp(1, 120)
}

fn normalized_poll_interval(raw_ms: u64) -> Duration {
    Duration::from_millis(raw_ms.clamp(100, 10_000))
}

async fn handle_message(bot: Bot, msg: teloxide::types::Message, cfg: Arc<TelegramChannelConfig>) -> Result<()> {
    let chat_id = msg.chat.id;
    let message_id = msg.id;

    if let Some(limit_chat_id) = cfg.chat_id
        && chat_id.0 != limit_chat_id
    {
        println!(
            "[channel/telegram] skip message: chat_id={} not allowed",
            chat_id.0
        );
        return Ok(());
    }

    let Some(text) = msg.text() else {
        println!(
            "[channel/telegram] skip non-text message: chat_id={} message_id={}",
            chat_id.0, message_id.0
        );
        return Ok(());
    };

    let user_input = text.trim();
    if user_input.is_empty() {
        println!(
            "[channel/telegram] skip empty text: chat_id={} message_id={}",
            chat_id.0, message_id.0
        );
        return Ok(());
    }

    println!(
        "[channel/telegram] incoming text: chat_id={} message_id={} preview={}",
        chat_id.0,
        message_id.0,
        preview_input(user_input)
    );

    let sent = bot
        .send_message(chat_id, "思考中...")
        .await
        .context("发送占位消息失败")?;

    let placeholder_id = sent.id;
    println!(
        "[channel/telegram] placeholder sent: chat_id={} message_id={}",
        chat_id.0, placeholder_id.0
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let input_owned = user_input.to_string();
    let generation_task = tokio::spawn(async move {
        app::call_once_stream_with_session(&input_owned, None, move |token| {
            let _ = tx.send(token.to_string());
        })
        .await
    });

    let mut streamed = String::new();
    let mut last_edit = Instant::now();
    let mut edit_count = 0usize;
    let mut streamed_tokens = 0usize;

    while let Some(token) = rx.recv().await {
        streamed_tokens += 1;
        streamed.push_str(&token);
        if last_edit.elapsed() >= Duration::from_millis(STREAM_EDIT_INTERVAL_MS) {
            let preview = preview_for_telegram(&streamed);
            if try_edit_message(&bot, chat_id, placeholder_id, &preview).await? {
                edit_count += 1;
            }
            last_edit = Instant::now();
        }
    }

    println!(
        "[channel/telegram] stream done: chat_id={} placeholder_id={} tokens={} chars={} edits={}",
        chat_id.0,
        placeholder_id.0,
        streamed_tokens,
        streamed.chars().count(),
        edit_count
    );

    let final_output = match generation_task.await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => format!("处理消息失败: {}", err),
        Err(err) => format!("处理消息失败: 任务异常: {}", err),
    };

    println!(
        "[channel/telegram] finalizing reply: chat_id={} placeholder_id={} chars={}",
        chat_id.0,
        placeholder_id.0,
        final_output.chars().count()
    );

    send_final_reply(&bot, chat_id, placeholder_id, &final_output).await
}

async fn send_final_reply(bot: &Bot, chat_id: ChatId, message_id: MessageId, text: &str) -> Result<()> {
    let normalized = if text.trim().is_empty() {
        "(empty response)".to_string()
    } else {
        text.to_string()
    };

    let chunks = split_by_char_limit(&normalized, TELEGRAM_TEXT_LIMIT);
    if chunks.is_empty() {
        let _ = try_edit_message(bot, chat_id, message_id, "(empty response)").await;
        println!(
            "[channel/telegram] final reply sent as empty response: chat_id={} message_id={}",
            chat_id.0, message_id.0
        );
        return Ok(());
    }

    let _ = try_edit_message(bot, chat_id, message_id, &chunks[0]).await;
    for chunk in chunks.iter().skip(1) {
        bot.send_message(chat_id, chunk.clone())
            .await
            .context("发送续段消息失败")?;
    }

    println!(
        "[channel/telegram] final reply sent: chat_id={} message_id={} chunks={} first_chunk_chars={}",
        chat_id.0,
        message_id.0,
        chunks.len(),
        chunks[0].chars().count()
    );

    Ok(())
}

async fn try_edit_message(bot: &Bot, chat_id: ChatId, message_id: MessageId, text: &str) -> Result<bool> {
    match bot
        .edit_message_text(chat_id, message_id, text.to_string())
        .await
    {
        Ok(_) => Ok(true),
        Err(err) => {
            let err_text = err.to_string();
            if err_text.contains("message is not modified") {
                return Ok(false);
            }
            Err(anyhow::anyhow!("编辑消息失败: {}", err_text))
        }
    }
}

fn preview_input(text: &str) -> String {
    let total = text.chars().count();
    let truncated: String = text.chars().take(INPUT_PREVIEW_CHARS).collect();
    if total > INPUT_PREVIEW_CHARS {
        format!("\"{}...\" (chars={})", truncated, total)
    } else {
        format!("\"{}\" (chars={})", truncated, total)
    }
}

fn preview_for_telegram(streamed: &str) -> String {
    if streamed.trim().is_empty() {
        "思考中...".to_string()
    } else {
        truncate_chars(streamed, TELEGRAM_TEXT_LIMIT)
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn split_by_char_limit(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![text.to_string()];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;

    for ch in text.chars() {
        if len >= max_chars {
            out.push(current);
            current = String::new();
            len = 0;
        }
        current.push(ch);
        len += 1;
    }

    if !current.is_empty() {
        out.push(current);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{normalized_long_poll_timeout_secs, normalized_poll_interval};
    use tokio::time::Duration;

    #[test]
    fn test_normalized_long_poll_timeout() {
        assert_eq!(normalized_long_poll_timeout_secs(0), 1);
        assert_eq!(normalized_long_poll_timeout_secs(20), 20);
        assert_eq!(normalized_long_poll_timeout_secs(999), 120);
    }

    #[test]
    fn test_normalized_poll_interval() {
        assert_eq!(normalized_poll_interval(0), Duration::from_millis(100));
        assert_eq!(normalized_poll_interval(1200), Duration::from_millis(1200));
        assert_eq!(normalized_poll_interval(99_999), Duration::from_millis(10_000));
    }
}
