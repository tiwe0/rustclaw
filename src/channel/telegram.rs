use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, Duration};

use crate::app;
use crate::config::TelegramChannelConfig;

#[derive(Debug, Deserialize)]
struct TelegramGetUpdatesResponse {
    ok: bool,
    result: Vec<TelegramUpdate>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Serialize)]
struct GetUpdatesRequest {
    timeout: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    allowed_updates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    chat_id: i64,
    text: String,
}

pub async fn run(cfg: TelegramChannelConfig) -> Result<()> {
    if cfg.bot_token.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "telegram bot_token 为空，请在 [channel.telegram] 中配置"
        ));
    }

    let client = Client::new();
    let base_url = cfg.api_base_url.trim_end_matches('/').to_string();
    let mut offset: Option<i64> = None;

    println!(
        "[channel/telegram] started: poll={}ms timeout={}s",
        cfg.poll_interval_ms, cfg.long_poll_timeout_secs
    );

    loop {
        let updates = get_updates(
            &client,
            &base_url,
            &cfg.bot_token,
            offset,
            cfg.long_poll_timeout_secs,
        )
        .await;

        match updates {
            Ok(list) => {
                for update in list {
                    offset = Some(update.update_id + 1);

                    let Some(message) = update.message else {
                        continue;
                    };

                    if let Some(limit_chat_id) = cfg.chat_id {
                        if message.chat.id != limit_chat_id {
                            continue;
                        }
                    }

                    let Some(text) = message.text else {
                        continue;
                    };

                    let user_input = text.trim();
                    if user_input.is_empty() {
                        continue;
                    }

                    let reply = match app::call_once(user_input).await {
                        Ok(output) if !output.trim().is_empty() => output,
                        Ok(_) => "(empty response)".to_string(),
                        Err(err) => format!("处理消息失败: {}", err),
                    };

                    if let Err(err) = send_message(
                        &client,
                        &base_url,
                        &cfg.bot_token,
                        message.chat.id,
                        &reply,
                    )
                    .await
                    {
                        eprintln!("[channel/telegram] send_message error: {}", err);
                    }
                }
            }
            Err(err) => {
                eprintln!("[channel/telegram] get_updates error: {}", err);
            }
        }

        sleep(Duration::from_millis(cfg.poll_interval_ms)).await;
    }
}

async fn get_updates(
    client: &Client,
    base_url: &str,
    bot_token: &str,
    offset: Option<i64>,
    timeout_secs: u64,
) -> Result<Vec<TelegramUpdate>> {
    let url = format!("{}/bot{}/getUpdates", base_url, bot_token);
    let body = GetUpdatesRequest {
        timeout: timeout_secs,
        offset,
        allowed_updates: vec!["message".to_string()],
    };

    let response = client
        .post(url)
        .json(&body)
        .send()
        .await
        .context("调用 getUpdates 失败")?
        .error_for_status()
        .context("getUpdates 返回错误状态")?
        .json::<TelegramGetUpdatesResponse>()
        .await
        .context("解析 getUpdates 响应失败")?;

    if !response.ok {
        return Err(anyhow::anyhow!("getUpdates 响应 ok=false"));
    }

    Ok(response.result)
}

async fn send_message(
    client: &Client,
    base_url: &str,
    bot_token: &str,
    chat_id: i64,
    text: &str,
) -> Result<()> {
    let url = format!("{}/bot{}/sendMessage", base_url, bot_token);
    let body = SendMessageRequest {
        chat_id,
        text: text.to_string(),
    };

    client
        .post(url)
        .json(&body)
        .send()
        .await
        .context("调用 sendMessage 失败")?
        .error_for_status()
        .context("sendMessage 返回错误状态")?;

    Ok(())
}
