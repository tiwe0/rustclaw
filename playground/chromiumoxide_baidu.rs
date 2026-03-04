use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::handler::viewport::Viewport;
use futures_util::StreamExt;
use tokio::time::{sleep, timeout, Duration};

const OPEN_TIMEOUT_SECS: u64 = 30;
const TITLE_TIMEOUT_SECS: u64 = 10;
const PAGE_STABILIZE_WAIT_MS: u64 = 15000;
const KNOWN_CDP_PARSE_ERR: &str = "data did not match any variant of untagged enum Message";
const WINDOW_WIDTH: u32 = 1440;
const WINDOW_HEIGHT: u32 = 900;

#[tokio::main]
async fn main() -> Result<()> {
    let mut cfg_builder = BrowserConfig::builder();
    cfg_builder = cfg_builder.with_head();
    cfg_builder = cfg_builder.window_size(WINDOW_WIDTH, WINDOW_HEIGHT);
    cfg_builder = cfg_builder.viewport(None::<Viewport>);
    let cfg = cfg_builder
        .build()
        .map_err(|e| anyhow::anyhow!("创建 Chromium 配置失败: {e}"))?;

    println!("[playground] launching browser...");
    let (mut browser, mut handler) = timeout(Duration::from_secs(OPEN_TIMEOUT_SECS), Browser::launch(cfg))
        .await
        .context("启动 Chromium 超时")??;
    println!("[playground] browser launched");

    let handler_task = tokio::spawn(async move {
        let mut ignored_parse_errors = 0usize;
        let mut noisy_notice_printed = false;
        while let Some(evt) = handler.next().await {
            if let Err(err) = evt {
                let msg = err.to_string();
                if msg.contains(KNOWN_CDP_PARSE_ERR) {
                    ignored_parse_errors += 1;
                    if !noisy_notice_printed {
                        eprintln!(
                            "[playground] ignore noisy CDP parse errors (protocol mismatch); page ops still work"
                        );
                        noisy_notice_printed = true;
                    }
                    continue;
                }
                eprintln!("[playground] browser event error: {msg}");
                continue;
            }
        }

        if ignored_parse_errors > 0 {
            eprintln!(
                "[playground] ignored {} noisy CDP parse errors",
                ignored_parse_errors
            );
        }
    });

    println!("[playground] opening https://www.baidu.com ...");
    let page = timeout(
        Duration::from_secs(OPEN_TIMEOUT_SECS),
        browser.new_page("https://www.google.com"),
    )
    .await
    .context("打开百度页面超时")??;

    println!(
        "[playground] waiting {}ms for page stabilize...",
        PAGE_STABILIZE_WAIT_MS
    );
    sleep(Duration::from_millis(PAGE_STABILIZE_WAIT_MS)).await;

    let title = timeout(Duration::from_secs(TITLE_TIMEOUT_SECS), page.get_title())
        .await
        .ok()
        .and_then(|v| v.ok())
        .flatten()
        .unwrap_or_else(|| "<empty title>".to_string());

    let url = page
        .url()
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "<unknown url>".to_string());

    println!("[playground] success");
    println!("[playground] title: {title}");
    println!("[playground] url: {url}");

    let _ = browser.close().await;
    handler_task.abort();
    Ok(())
}
