use anyhow::{Context, Result};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{ChatId, MessageId, ReplyParameters, ThreadId};
use teloxide::update_listeners;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

use crate::app;
use crate::config::TelegramChannelConfig;
use crate::interrupt;
use crate::types::{Message as ChatMessage, ToolCall};

const TELEGRAM_TEXT_LIMIT: usize = 3900;
const STREAM_EDIT_INTERVAL_MS: u64 = 700;
const TELEGRAM_DEFAULT_API_BASE_URL: &str = "https://api.telegram.org";
const INPUT_PREVIEW_CHARS: usize = 80;
const TOOL_ARGS_PREVIEW_CHARS: usize = 200;
const REACT_STOP_MARKER: &str = "[[REACT_STOP]]";

enum TelegramReactEvent {
    AssistantStarted { loop_idx: usize },
    Token(String),
    ToolCallsStarted(Vec<ToolCall>),
    ToolResults(Vec<ChatMessage>),
}

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
    println!(
        "[channel/telegram] verbose_tool_messages: {}",
        cfg.verbose_tool_messages
    );

    let bot = build_bot(&cfg)?;
    let listener = build_polling_listener(bot.clone(), &cfg).await;
    teloxide::repl_with_listener(bot, move |bot: Bot, msg: teloxide::types::Message| {
        let cfg = cfg.clone();
        async move {
            if let Err(err) = handle_message(bot, msg, cfg).await {
                eprintln!("[channel/telegram] handle_message error: {}", err);
            } else {
                println!("[channel/telegram] message handled, waiting for next update...");
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
    let thread_id = msg.thread_id;
    let telegram_session_id = format!("telegram_{}", chat_id.0);

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

    if user_input.eq_ignore_ascii_case("/interrupt")
        || user_input.eq_ignore_ascii_case("/cancel")
        || user_input.eq_ignore_ascii_case("/stop")
    {
        interrupt::cancel_session(&telegram_session_id);
        let _ = send_message_in_context(
            &bot,
            chat_id,
            thread_id,
            Some(message_id),
            "已中断当前对话。".to_string(),
        )
        .await;
        println!(
            "[channel/telegram] interrupt requested: chat_id={} session_id={}",
            chat_id.0, telegram_session_id
        );
        return Ok(());
    }

    println!(
        "[channel/telegram] incoming text: chat_id={} message_id={} session_id={} preview={}",
        chat_id.0,
        message_id.0,
        telegram_session_id,
        preview_input(user_input)
    );

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<TelegramReactEvent>();
    let input_owned = user_input.to_string();
    let generation_task = tokio::spawn(async move {
        let tx_for_start = event_tx.clone();
        let tx_for_token = event_tx.clone();
        let tx_for_tools = event_tx.clone();
        let tx_for_results = event_tx.clone();

        app::call_once_react_with_session(
            &input_owned,
            Some(&telegram_session_id),
            move |loop_idx| {
                let _ = tx_for_start.send(TelegramReactEvent::AssistantStarted { loop_idx });
            },
            move |token| {
                let _ = tx_for_token.send(TelegramReactEvent::Token(token.to_string()));
            },
            move |tool_calls| {
                let _ = tx_for_tools.send(TelegramReactEvent::ToolCallsStarted(tool_calls.to_vec()));
            },
            move |tool_messages| {
                let _ = tx_for_results.send(TelegramReactEvent::ToolResults(tool_messages.to_vec()));
            },
        )
        .await
    });

    let mut current_streamed = String::new();
    let mut current_placeholder_id: Option<MessageId> = None;
    let mut current_loop_idx: usize = 0;
    let mut last_edit = Instant::now();
    let mut edit_count = 0usize;
    let mut streamed_tokens = 0usize;

    while let Some(event) = event_rx.recv().await {
        match event {
            TelegramReactEvent::AssistantStarted { loop_idx } => {
                if let Some(placeholder_id) = current_placeholder_id {
                    finalize_assistant_message(
                        &bot,
                        chat_id,
                        thread_id,
                        Some(message_id),
                        placeholder_id,
                        &current_streamed,
                    )
                    .await?;
                }

                current_loop_idx = loop_idx;
                current_streamed.clear();
                last_edit = Instant::now();
                let sent = send_message_in_context(
                    &bot,
                    chat_id,
                    thread_id,
                    Some(message_id),
                    format!("🤖 第{}轮思考中...", loop_idx),
                )
                .await
                .context("发送 assistant 占位消息失败")?;
                current_placeholder_id = Some(sent.id);
                println!(
                    "[channel/telegram] assistant placeholder sent: loop={} chat_id={} message_id={}",
                    loop_idx,
                    chat_id.0,
                    sent.id.0
                );
            }
            TelegramReactEvent::Token(token) => {
                streamed_tokens += 1;
                current_streamed.push_str(&token);
                if last_edit.elapsed() >= Duration::from_millis(STREAM_EDIT_INTERVAL_MS)
                    && let Some(placeholder_id) = current_placeholder_id
                {
                    let preview = preview_for_telegram(&current_streamed);
                    match try_edit_message(&bot, chat_id, placeholder_id, &preview).await {
                        Ok(true) => {
                            edit_count += 1;
                        }
                        Ok(false) => {}
                        Err(err) => {
                            eprintln!(
                                "[channel/telegram] stream edit failed: loop={} chat_id={} placeholder_id={} err={}",
                                current_loop_idx,
                                chat_id.0,
                                placeholder_id.0,
                                err
                            );
                        }
                    }
                    last_edit = Instant::now();
                }
            }
            TelegramReactEvent::ToolCallsStarted(tool_calls) => {
                println!(
                    "[channel/telegram] tool_calls_started: chat_id={} count={}",
                    chat_id.0,
                    tool_calls.len()
                );
                for call in &tool_calls {
                    println!(
                        "[channel/telegram] tool_call: name={} id={} args_preview={}",
                        call.function.name,
                        call.id,
                        preview_chars(&call.function.arguments, TOOL_ARGS_PREVIEW_CHARS)
                    );
                }
                if cfg.verbose_tool_messages {
                    let tool_text = format_tool_calls_for_telegram(&tool_calls);
                    let _ = send_message_in_context(
                        &bot,
                        chat_id,
                        thread_id,
                        Some(message_id),
                        tool_text,
                    )
                    .await
                    .context("发送 function call 通知失败")?;
                    println!(
                        "[channel/telegram] tool_calls_notice_sent: chat_id={} count={}",
                        chat_id.0,
                        tool_calls.len()
                    );
                } else {
                    println!(
                        "[channel/telegram] tool_calls_notice_skipped: verbose_tool_messages=false"
                    );
                }
            }
            TelegramReactEvent::ToolResults(tool_messages) => {
                println!(
                    "[channel/telegram] tool_results_ready: chat_id={} count={}",
                    chat_id.0,
                    tool_messages.len()
                );
                for tool_msg in &tool_messages {
                    let tool_name = tool_msg.name.as_deref().unwrap_or("unknown_tool");
                    let call_id = tool_msg.tool_call_id.as_deref().unwrap_or("unknown_call");
                    let preview = preview_chars(
                        tool_msg.content.as_deref().unwrap_or(""),
                        180,
                    );
                    println!(
                        "[channel/telegram] tool_result: tool={} call_id={} preview={}",
                        tool_name,
                        call_id,
                        preview
                    );
                }
                if cfg.verbose_tool_messages {
                    for tool_msg in &tool_messages {
                        let text = format_tool_result_for_telegram(tool_msg);
                        let _ = send_message_in_context(
                            &bot,
                            chat_id,
                            thread_id,
                            Some(message_id),
                            text,
                        )
                        .await
                        .context("发送 function call 结果失败")?;
                    }
                    println!(
                        "[channel/telegram] tool_results_messages_sent: chat_id={} count={}",
                        chat_id.0,
                        tool_messages.len()
                    );
                } else {
                    println!(
                        "[channel/telegram] tool_results_messages_skipped: verbose_tool_messages=false"
                    );
                }
            }
        }
    }

    if let Some(placeholder_id) = current_placeholder_id {
        finalize_assistant_message(
            &bot,
            chat_id,
            thread_id,
            Some(message_id),
            placeholder_id,
            &current_streamed,
        )
        .await?;
    }

    println!(
        "[channel/telegram] stream done: chat_id={} loops={} tokens={} chars={} edits={}",
        chat_id.0,
        current_loop_idx,
        streamed_tokens,
        current_streamed.chars().count(),
        edit_count
    );

    match generation_task.await {
        Ok(Ok(_)) => {
            println!(
                "[channel/telegram] complete: chat_id={} message_id={}",
                chat_id.0, message_id.0
            );
            Ok(())
        }
        Ok(Err(err)) => Err(anyhow::anyhow!("处理消息失败: {}", err)),
        Err(err) => Err(anyhow::anyhow!("处理消息失败: 任务异常: {}", err)),
    }
}

async fn finalize_assistant_message(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    reply_to: Option<MessageId>,
    placeholder_id: MessageId,
    content: &str,
) -> Result<()> {
    let visible = sanitize_stream_visible_text(content);
    let normalized = if visible.trim().is_empty() {
        "（本轮无文本回复）".to_string()
    } else {
        visible
    };
    let chunks = split_by_char_limit(&normalized, TELEGRAM_TEXT_LIMIT);
    if chunks.is_empty() {
        return Ok(());
    }

    match try_edit_message(bot, chat_id, placeholder_id, &chunks[0]).await {
        Ok(_) => {}
        Err(err) => {
            eprintln!(
                "[channel/telegram] finalize edit failed, fallback send: chat_id={} placeholder_id={} err={}",
                chat_id.0,
                placeholder_id.0,
                err
            );
            let _ = send_message_in_context(
                bot,
                chat_id,
                thread_id,
                reply_to,
                chunks[0].clone(),
            )
            .await
            .context("发送 fallback assistant 消息失败")?;
        }
    }

    for chunk in chunks.iter().skip(1) {
        let _ = send_message_in_context(bot, chat_id, thread_id, reply_to, chunk.clone())
            .await
            .context("发送 assistant 续段失败")?;
    }

    Ok(())
}

fn format_tool_calls_for_telegram(tool_calls: &[ToolCall]) -> String {
    if tool_calls.is_empty() {
        return "🔧 function call: (empty)".to_string();
    }

    let mut lines = Vec::new();
    lines.push("🔧 function call 发起".to_string());
    for call in tool_calls {
        let args_preview = preview_chars(&call.function.arguments, TOOL_ARGS_PREVIEW_CHARS);
        lines.push(format!(
            "- {}(id={}) args={}",
            call.function.name, call.id, args_preview
        ));
    }
    lines.join("\n")
}

fn format_tool_result_for_telegram(msg: &ChatMessage) -> String {
    let name = msg.name.clone().unwrap_or_else(|| "unknown_tool".to_string());
    let call_id = msg
        .tool_call_id
        .clone()
        .unwrap_or_else(|| "unknown_call".to_string());
    let content = msg.content.clone().unwrap_or_default();
    let preview = preview_chars(&content, 1200);
    format!("🧾 function call 结果\n- tool={}\n- call_id={}\n- result={}", name, call_id, preview)
}

fn preview_chars(s: &str, max_chars: usize) -> String {
    let normalized = s.replace('\n', " ").trim().to_string();
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let cut = normalized.chars().take(max_chars).collect::<String>();
        format!("{}...", cut)
    }
}

async fn send_message_in_context(
    bot: &Bot,
    chat_id: ChatId,
    thread_id: Option<ThreadId>,
    reply_to: Option<MessageId>,
    text: String,
) -> Result<teloxide::types::Message> {
    let mut req = bot.send_message(chat_id, text);
    if let Some(thread_id) = thread_id {
        req = req.message_thread_id(thread_id);
    }
    if let Some(reply_to) = reply_to {
        req = req.reply_parameters(ReplyParameters::new(reply_to));
    }
    let sent = req.await.context("send_message 请求失败")?;
    Ok(sent)
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
    let visible = sanitize_stream_visible_text(streamed);
    if visible.trim().is_empty() {
        "思考中...".to_string()
    } else {
        truncate_chars(&visible, TELEGRAM_TEXT_LIMIT)
    }
}

fn sanitize_stream_visible_text(text: &str) -> String {
    let without_full_marker = text.replace(REACT_STOP_MARKER, "");
    trim_partial_stop_marker_suffix(&without_full_marker)
}

fn trim_partial_stop_marker_suffix(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let marker = REACT_STOP_MARKER;
    let max_check = marker.len().saturating_sub(1).min(text.len());
    for suffix_len in (1..=max_check).rev() {
        if text.ends_with(&marker[..suffix_len]) {
            return text[..text.len() - suffix_len].to_string();
        }
    }
    text.to_string()
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
