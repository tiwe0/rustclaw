use anyhow::{Context, Result};
use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat};
use chromiumoxide::handler::viewport::Viewport;
use chromiumoxide::Page;
use chromiumoxide::page::ScreenshotParams;
use futures_util::StreamExt;
use minify_html::{minify, Cfg};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, timeout};

use crate::log;
use crate::tools::{ToolPlugin, truncate_utf8};
use crate::types::{ToolDefinition, ToolSchema};

const MAX_TEXT_PREVIEW: usize = 6000;
const WINDOW_WIDTH: u32 = 1440;
const WINDOW_HEIGHT: u32 = 900;
const DEFAULT_OPEN_URL: &str = "https://www.google.com";
const WEB_OPEN_TIMEOUT_SECS: u64 = 30;
const WEB_TITLE_TIMEOUT_SECS: u64 = 5;
const KNOWN_CDP_PARSE_ERR: &str = "data did not match any variant of untagged enum Message";
static WEB_SESSION_SEQ: AtomicU64 = AtomicU64::new(1);

type SessionMap = HashMap<String, Arc<Mutex<WebSession>>>;

struct WebSession {
    browser: Browser,
    page: Page,
    handler_task: tokio::task::JoinHandle<()>,
}

static WEB_SESSIONS: OnceLock<RwLock<SessionMap>> = OnceLock::new();
static CURRENT_WEB_SESSION: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn session_store() -> &'static RwLock<SessionMap> {
    WEB_SESSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn current_session_store() -> &'static RwLock<Option<String>> {
    CURRENT_WEB_SESSION.get_or_init(|| RwLock::new(None))
}

pub struct WebBrowserTool;

#[async_trait]
impl ToolPlugin for WebBrowserTool {
    fn name(&self) -> &'static str {
        "web_browser"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "操作浏览器（Chromiumoxide）：打开网页、点击、输入、截图与提取网页内容。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "description": "支持: open, close, sessions, goto, scroll, html, minihtml, content, click, input, screenshot"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "浏览器会话 ID；open 返回该值，后续动作可复用"
                        },
                        "url": {
                            "type": "string",
                            "description": "页面 URL（open 可省略，默认 https://www.google.com；其他动作在无 session_id 时必填）"
                        },
                        "selector": {
                            "type": "string",
                            "description": "CSS 选择器（click/input 动作必填）"
                        },
                        "text": {
                            "type": "string",
                            "description": "输入文本（input 动作必填）"
                        },
                        "clear": {
                            "type": "boolean",
                            "description": "input 前是否清空，默认 true"
                        },
                        "submit": {
                            "type": "boolean",
                            "description": "input 后是否尝试提交所在表单，默认 false"
                        },
                        "path": {
                            "type": "string",
                            "description": "截图保存路径（screenshot 可选，默认输出到系统临时目录）"
                        },
                        "format": {
                            "type": "string",
                            "description": "截图格式: png/jpeg，默认 png"
                        },
                        "full_page": {
                            "type": "boolean",
                            "description": "截图是否全页，默认 true"
                        },
                        "omit_background": {
                            "type": "boolean",
                            "description": "PNG 截图是否透明背景，默认 false"
                        },
                        "max_chars": {
                            "type": "integer",
                            "description": "content/html 返回预览最大字符数"
                        },
                        "scroll_x": {
                            "type": "integer",
                            "description": "scroll 横向滚动距离（像素），默认 0"
                        },
                        "scroll_y": {
                            "type": "integer",
                            "description": "scroll 纵向滚动距离（像素），默认 800；负值表示向上"
                        },
                        "headless": {
                            "type": "boolean",
                            "description": "是否无头模式，默认 true"
                        }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    fn finit(&self) {
        close_all_sessions_blocking();
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        log::debug(format!(
            "[web_browser][execute] received args={}"
            , preview_json(&args, 600)
        ));

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_lowercase())
            .context("web_browser 缺少 action")?;

        log::debug(format!("[web_browser][execute] action={}", action));

        if !is_supported_action(&action) {
            log::warn(format!("[web_browser][execute] unsupported action={}", action));
            return Ok(json!({
                "ok": false,
                "error": "action 仅支持 open/close/sessions/goto/scroll/html/minihtml/content/click/input/screenshot"
            }));
        }

        if action == "close" {
            log::debug("[web_browser][execute] enter close branch");
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .context("close 动作缺少 session_id")?;
            log::debug(format!("[web_browser][execute] close session_id={}", session_id));
            return run_close_session(session_id).await;
        }

        if action == "sessions" {
            log::debug("[web_browser][execute] enter sessions branch");
            return run_list_sessions().await;
        }

        if action == "open" {
            log::debug("[web_browser][execute] enter open branch");
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .unwrap_or(DEFAULT_OPEN_URL);
            if !is_http_url(url) {
                log::warn(format!("[web_browser][execute] open rejected non-http url={}", url));
                return Ok(json!({
                    "ok": false,
                    "error": "仅允许 http/https URL"
                }));
            }

            let headless = args
                .get("headless")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let requested_session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string());

            log::debug(format!(
                "[web_browser][execute] open url={} headless={} requested_session_id={}"
                , url, headless, requested_session_id.as_deref().unwrap_or("<none>")
            ));

            return run_open_session(url, headless, requested_session_id).await;
        }

        let requested_session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string());

        if let Some(session_id) = requested_session_id {
            log::debug(format!(
                "[web_browser][execute] use explicit session_id={} for action={}"
                , session_id, action
            ));
            return run_action_in_session(&action, &session_id, &args).await;
        }

        if let Some(session_id) = get_default_session_id().await {
            log::debug(format!(
                "[web_browser][execute] use default session_id={} for action={}"
                , session_id, action
            ));
            return run_action_in_session(&action, &session_id, &args).await;
        }

        log::debug("[web_browser][execute] no reusable session, fallback to auto-open");
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .context("当前无可复用会话，请先 open，或提供 url 自动创建会话")?;
        validate_http_url(url)?;

        let headless = args
            .get("headless")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let open_result = run_open_session(url, headless, None).await?;
        let created_session_id = open_result
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .context("open 成功但未返回 session_id")?;

        log::debug(format!(
            "[web_browser][execute] auto-open created session_id={} for action={}"
            , created_session_id, action
        ));

        run_action_in_session(&action, &created_session_id, &args).await
    }
}

async fn close_all_sessions() {
    let handles = {
        let mut store = session_store().write().await;
        store.drain().map(|(_, handle)| handle).collect::<Vec<_>>()
    };

    {
        let mut current = current_session_store().write().await;
        *current = None;
    }

    if handles.is_empty() {
        return;
    }

    log::debug(format!(
        "[web_browser][finit] closing remaining sessions count={}"
        , handles.len()
    ));

    for handle in handles {
        let mut session = handle.lock().await;
        let _ = session.browser.close().await;
        session.handler_task.abort();
    }

    log::debug("[web_browser][finit] all remaining browser sessions closed");
}

fn close_all_sessions_blocking() {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::block_in_place(|| handle.block_on(close_all_sessions()))
        }));

        if result.is_err() {
            log::warn(
                "[web_browser][finit] fallback to async spawn cleanup in current runtime",
            );
            std::mem::drop(handle.spawn(close_all_sessions()));
        }
        return;
    }

    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => {
            rt.block_on(close_all_sessions());
        }
        Err(err) => {
            log::warn(format!(
                "[web_browser][finit] create runtime for cleanup failed: {}",
                err
            ));
        }
    }
}

async fn run_open_session(
    url: &str,
    headless: bool,
    requested_session_id: Option<String>,
) -> Result<Value> {
    log::debug(format!(
        "[web_browser][open] start url={} headless={} requested_session_id={}"
        , url, headless, requested_session_id.as_deref().unwrap_or("<none>")
    ));

    let mut cfg_builder = BrowserConfig::builder();
    if !headless {
        cfg_builder = cfg_builder.with_head();
    }
    let cfg = cfg_builder
        .user_data_dir("/tmp/chrome-session")
        .with_head()
        .window_size(WINDOW_WIDTH, WINDOW_HEIGHT)
        .viewport(None::<Viewport>)
        .build()
        .map_err(|e| anyhow::anyhow!("创建 Chromium 配置失败: {e}"))?;

    log::debug("[web_browser][open] launching browser");

    let (browser, mut handler) = match timeout(
        Duration::from_secs(WEB_OPEN_TIMEOUT_SECS),
        Browser::launch(cfg),
    )
    .await
    {
        Ok(Ok(v)) => {
            log::debug("[web_browser][open] browser launched");
            v
        }
        Ok(Err(err)) => {
            log::warn(format!("[web_browser][open] browser launch failed: {}", err));
            return Err(anyhow::anyhow!(
                "启动 Chromium 失败，请确认本机可用 Chrome/Chromium"
            ));
        }
        Err(_) => {
            log::warn("[web_browser][open] browser launch timeout");
            return Err(anyhow::anyhow!(
                "启动 Chromium 超时，请检查本机 Chrome/Chromium 是否可用"
            ));
        }
    };

    let handler_task = tokio::spawn(async move {
        let mut noisy_notice_printed = false;
        while let Some(evt) = handler.next().await {
            if let Err(err) = evt {
                let msg = err.to_string();
                if msg.contains(KNOWN_CDP_PARSE_ERR) {
                    if !noisy_notice_printed {
                        log::warn(
                            "[web_browser][open] detect noisy CDP parse errors (protocol mismatch), keep running",
                        );
                        noisy_notice_printed = true;
                    }
                    log::debug(format!(
                        "[web_browser][open] ignored noisy cdp parse error: {}",
                        msg
                    ));
                    continue;
                }

                log::warn(format!(
                    "[web_browser][open] browser event handler received error, keep running: {}",
                    msg
                ));
                continue;
            }
        }
        log::debug("[web_browser][open] browser event handler ended");
    });


    log::debug(format!("[web_browser][open] creating new page url={}", url));

    let page = match timeout(
        Duration::from_secs(WEB_OPEN_TIMEOUT_SECS),
        browser.new_page(url)
    ).await {
        Ok(Ok(page)) => {
            log::debug("[web_browser][open] page created");
            page
        }
        Ok(Err(err)) => {
            log::warn(format!("[web_browser][open] create page failed url={} err={}", url, err));
            return Err(anyhow::anyhow!("打开页面失败: {}", url));
        }
        Err(_) => {
            log::warn(format!("[web_browser][open] create page timeout url={}", url));
            return Err(anyhow::anyhow!("打开页面超时: {}", url));
        }
    };

    log::debug("[web_browser][open] reading page title");
    let title = timeout(Duration::from_secs(WEB_TITLE_TIMEOUT_SECS), page.get_title())
        .await
        .ok()
        .and_then(|v| v.ok())
        .unwrap_or_default();
    let session_id = requested_session_id.unwrap_or_else(generate_session_id);

    let title_preview = title.as_deref().unwrap_or_default();
    log::debug(format!(
        "[web_browser][open] resolved session_id={} title={}"
        , session_id, truncate(title_preview, 120)
    ));

    let session = WebSession {
        browser,
        page,
        handler_task,
    };

    let replaced = {
        let mut store = session_store().write().await;
        store.insert(session_id.clone(), Arc::new(Mutex::new(session)))
    };

    if let Some(old) = replaced {
        log::debug(format!("[web_browser][open] replacing existing session={}", session_id));
        let mut old_session = old.lock().await;
        let _ = old_session.browser.close().await;
        old_session.handler_task.abort();
    }

    {
        let mut current = current_session_store().write().await;
        *current = Some(session_id.clone());
    }

    log::debug(format!("[web_browser][open] success session_id={}", session_id));

    Ok(json!({
        "ok": true,
        "action": "open",
        "session_id": session_id,
        "url": url,
        "title": title,
        "keep_alive": true,
        "hint": "后续调用请传 session_id 复用浏览器，结束后调用 action=close 释放资源"
    }))
}

async fn run_close_session(session_id: &str) -> Result<Value> {
    log::debug(format!("[web_browser][close] start session_id={}", session_id));
    let session = {
        let mut store = session_store().write().await;
        store.remove(session_id)
    };

    if let Some(handle) = session {
        let mut session = handle.lock().await;
        let _ = session.browser.close().await;
        session.handler_task.abort();
        log::debug(format!("[web_browser][close] browser closed session_id={}", session_id));

        let mut current = current_session_store().write().await;
        if current.as_deref() == Some(session_id) {
            *current = session_store().read().await.keys().next().cloned();
            log::debug(format!(
                "[web_browser][close] switched current session to {}"
                , current.as_deref().unwrap_or("<none>")
            ));
        }

        return Ok(json!({
            "ok": true,
            "action": "close",
            "session_id": session_id,
            "closed": true,
        }));
    }

    Ok(json!({
        "ok": false,
        "action": "close",
        "session_id": session_id,
        "error": "session 不存在或已关闭"
    }))
}

async fn run_action_in_session(action: &str, session_id: &str, args: &Value) -> Result<Value> {
    log::debug(format!(
        "[web_browser][action] start action={} session_id={} args={}"
        , action, session_id, preview_json(args, 600)
    ));

    let handle = {
        let store = session_store().read().await;
        store.get(session_id).cloned()
    }
    .with_context(|| format!("session 不存在: {session_id}"))?;

    {
        let mut current = current_session_store().write().await;
        *current = Some(session_id.to_string());
    }

    let session = handle.lock().await;

    if let Some(url) = args
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        validate_http_url(url)?;
        log::debug(format!("[web_browser][action] goto start session_id={} url={}", session_id, url));
        session
            .page
            .goto(url)
            .await
            .with_context(|| format!("导航到页面失败: {url}"))?;
        log::debug(format!("[web_browser][action] goto done session_id={} url={}", session_id, url));
    }

    match action {
        "goto" => {
            let requested_url = args
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .context("goto 动作缺少 url")?;
            validate_http_url(requested_url)?;
            log::debug(format!(
                "[web_browser][action][goto] navigated to url={} session_id={}"
                , requested_url, session_id
            ));

            let title = session.page.get_title().await.unwrap_or_default();
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();

            Ok(json!({
                "ok": true,
                "action": "goto",
                "session_id": session_id,
                "requested_url": requested_url,
                "url": current_url,
                "title": title,
            }))
        }
        "scroll" => {
            let scroll_x = args
                .get("scroll_x")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .clamp(-10000, 10000);
            let scroll_y = args
                .get("scroll_y")
                .and_then(|v| v.as_i64())
                .unwrap_or(800)
                .clamp(-10000, 10000);
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string());

            log::debug(format!(
                "[web_browser][action][scroll] scroll_x={} scroll_y={} selector={} session_id={}"
                ,
                scroll_x,
                scroll_y,
                selector.as_deref().unwrap_or("<window>"),
                session_id
            ));

            if let Some(sel) = &selector {
                let element = session
                    .page
                    .find_element(sel)
                    .await
                    .with_context(|| format!("未找到元素: {sel}"))?;
                let script = format!(
                    "function(){{ this.scrollBy({}, {}); return {{ left: this.scrollLeft, top: this.scrollTop }}; }}",
                    scroll_x, scroll_y
                );
                let _ = element.call_js_fn(&script, true).await;
            } else {
                let body = session
                    .page
                    .find_element("body")
                    .await
                    .context("未找到 body 元素，无法执行页面滚动")?;
                let script = format!(
                    "function(){{ window.scrollBy({}, {}); return {{ x: window.scrollX, y: window.scrollY }}; }}",
                    scroll_x, scroll_y
                );
                let _ = body.call_js_fn(&script, true).await;
            }

            tokio::time::sleep(Duration::from_millis(150)).await;

            let title = session.page.get_title().await.unwrap_or_default();
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();

            Ok(json!({
                "ok": true,
                "action": "scroll",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "scroll_x": scroll_x,
                "scroll_y": scroll_y,
                "selector": selector,
            }))
        }
        "html" => {
            log::debug(format!("[web_browser][action][html] collecting html session_id={}", session_id));
            let title = session.page.get_title().await.unwrap_or_default();
            let html = session.page.content().await.unwrap_or_default();
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "action": "html",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "html": html,
            }))
        }
        "minihtml" => {
            log::debug(format!("[web_browser][action][minihtml] collecting minified html session_id={}", session_id));
            let title = session.page.get_title().await.unwrap_or_default();
            let html = session.page.content().await.unwrap_or_default();
            let html = minify_html_text(&html);
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "action": "minihtml",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "html": html,
            }))
        }
        "content" => {
            log::debug(format!("[web_browser][action][content] collecting content session_id={}", session_id));
            let max_chars = args
                .get("max_chars")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(MAX_TEXT_PREVIEW)
                .clamp(256, 20000);
            let title = session.page.get_title().await.unwrap_or_default();
            let html = session.page.content().await.unwrap_or_default();
            let body_text = match session.page.find_element("body").await {
                Ok(el) => el.inner_text().await.ok().flatten().unwrap_or_default(),
                Err(_) => String::new(),
            };
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "action": "content",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "text": truncate(&body_text, max_chars),
                "html": truncate(&html, max_chars),
            }))
        }
        "click" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .context("click 动作缺少 selector")?;
            log::debug(format!("[web_browser][action][click] selector={} session_id={}", selector, session_id));
            let element = session
                .page
                .find_element(selector)
                .await
                .with_context(|| format!("未找到元素: {selector}"))?;
            element
                .click()
                .await
                .with_context(|| format!("点击元素失败: {selector}"))?;
            let title = session.page.get_title().await.unwrap_or_default();
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "action": "click",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "selector": selector,
            }))
        }
        "input" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .context("input 动作缺少 selector")?;
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .context("input 动作缺少 text")?;
            let clear = args.get("clear").and_then(|v| v.as_bool()).unwrap_or(true);
            let submit = args
                .get("submit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            log::debug(format!(
                "[web_browser][action][input] selector={} text_len={} clear={} submit={} session_id={}"
                , selector, text.len(), clear, submit, session_id
            ));

            let element = session
                .page
                .find_element(selector)
                .await
                .with_context(|| format!("未找到元素: {selector}"))?;
            if clear {
                let _ = element
                    .call_js_fn("function(){ this.value = ''; }", true)
                    .await;
            }
            element
                .click()
                .await
                .with_context(|| format!("聚焦元素失败: {selector}"))?;
            element
                .type_str(text)
                .await
                .with_context(|| format!("输入文本失败: {selector}"))?;
            if submit {
                let _ = element
                    .call_js_fn(
                        "function(){ if(this.form){ if(this.form.requestSubmit){ this.form.requestSubmit(); } else { this.form.submit(); } } }",
                        true,
                    )
                    .await;
            }

            let title = session.page.get_title().await.unwrap_or_default();
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "action": "input",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "selector": selector,
                "text_len": text.len(),
                "submit": submit,
            }))
        }
        "screenshot" => {
            log::debug(format!("[web_browser][action][screenshot] start session_id={}", session_id));
            let format = args
                .get("format")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .unwrap_or_else(|| "png".to_string());
            let full_page = args
                .get("full_page")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let omit_background = args
                .get("omit_background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let output_path = args
                .get("path")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from);
            let capture_format = match format.as_str() {
                "jpeg" | "jpg" => CaptureScreenshotFormat::Jpeg,
                _ => CaptureScreenshotFormat::Png,
            };
            let ext = if matches!(capture_format, CaptureScreenshotFormat::Jpeg) {
                "jpg"
            } else {
                "png"
            };
            let params = ScreenshotParams::builder()
                .format(capture_format)
                .full_page(full_page)
                .omit_background(omit_background)
                .build();
            let image = session.page.screenshot(params).await.context("截屏失败")?;
            let output = output_path.unwrap_or_else(|| default_screenshot_path(ext));
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .with_context(|| format!("创建截图目录失败: {}", parent.display()))?;
                }
            }
            tokio::fs::write(&output, &image)
                .await
                .with_context(|| format!("写入截图失败: {}", output.display()))?;
            let title = session.page.get_title().await.unwrap_or_default();
            let current_url = session
                .page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(json!({
                "ok": true,
                "action": "screenshot",
                "session_id": session_id,
                "url": current_url,
                "title": title,
                "path": output.display().to_string(),
                "bytes": image.len(),
                "format": ext,
                "full_page": full_page,
            }))
        }
        _ => Ok(json!({
            "ok": false,
            "error": "该动作需要 session_id，或不在支持范围内"
        })),
    }
}

async fn run_list_sessions() -> Result<Value> {
    log::debug("[web_browser][sessions] start");
    let handles = {
        let store = session_store().read().await;
        store
            .iter()
            .map(|(id, handle)| (id.clone(), handle.clone()))
            .collect::<Vec<_>>()
    };

    let current_id = current_session_store().read().await.clone();
    let mut sessions = Vec::with_capacity(handles.len());

    for (id, handle) in handles {
        let session = handle.lock().await;
        let title = session.page.get_title().await.unwrap_or_default();
        let url = session
            .page
            .url()
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        sessions.push(json!({
            "session_id": id,
            "current": current_id.as_deref() == Some(id.as_str()),
            "url": url,
            "title": title,
        }));
    }

    log::debug(format!(
        "[web_browser][sessions] done count={} current_session_id={}"
        , sessions.len(), current_id.as_deref().unwrap_or("<none>")
    ));

    Ok(json!({
        "ok": true,
        "action": "sessions",
        "current_session_id": current_id,
        "count": sessions.len(),
        "sessions": sessions,
    }))
}

async fn get_default_session_id() -> Option<String> {
    log::debug("[web_browser][session] resolve default session_id");
    let current_id = current_session_store().read().await.clone();
    if let Some(id) = current_id {
        let exists = session_store().read().await.contains_key(&id);
        if exists {
            log::debug(format!("[web_browser][session] default from current={}", id));
            return Some(id);
        }
    }

    let fallback = session_store().read().await.keys().next().cloned();
    if let Some(id) = &fallback {
        let mut current = current_session_store().write().await;
        *current = Some(id.clone());
        log::debug(format!("[web_browser][session] default from fallback={}", id));
    } else {
        log::debug("[web_browser][session] no available session");
    }
    fallback
}

fn preview_json(value: &Value, max: usize) -> String {
    let text = value.to_string();
    truncate(&text, max)
}

fn default_screenshot_path(ext: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::env::temp_dir().join(format!("rustclaw_webshot_{ts}.{ext}"))
}

fn truncate(s: &str, max: usize) -> String {
    truncate_utf8(s, max)
}

fn minify_html_text(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }

    let cfg = Cfg::new();
    let minified = minify(html.as_bytes(), &cfg);
    String::from_utf8(minified).unwrap_or_else(|_| html.to_string())
}

fn is_supported_action(action: &str) -> bool {
    matches!(
        action,
        "open"
            | "close"
            | "sessions"
            | "goto"
            | "scroll"
            | "html"
            | "minihtml"
            | "content"
            | "click"
            | "input"
            | "screenshot"
    )
}

fn generate_session_id() -> String {
    let seq = WEB_SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("web_{}_{}", ts, seq)
}

fn validate_http_url(url: &str) -> Result<()> {
    if is_http_url(url) {
        Ok(())
    } else {
        Err(anyhow::anyhow!("仅允许 http/https URL"))
    }
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolPlugin;

    #[test]
    fn truncate_keeps_short_text() {
        let input = "hello";
        let out = truncate(input, 10);
        assert_eq!(out, "hello");
    }

    #[test]
    fn truncate_cuts_long_text() {
        let input = "abcdefghijklmnopqrstuvwxyz";
        let out = truncate(input, 5);
        assert_eq!(out, "abcde...(truncated)");
    }

    #[test]
    fn truncate_does_not_panic_on_utf8_boundary() {
        let input = "ab，cd";
        let out = truncate(input, 3);
        assert_eq!(out, "ab...(truncated)");
    }

    #[test]
    fn default_screenshot_path_uses_extension() {
        let p = default_screenshot_path("png");
        let text = p.to_string_lossy();
        assert!(text.contains("rustclaw_webshot_"));
        assert!(text.ends_with(".png"));
    }

    #[test]
    fn action_support_check() {
        assert!(is_supported_action("open"));
        assert!(is_supported_action("click"));
        assert!(is_supported_action("sessions"));
        assert!(is_supported_action("goto"));
        assert!(is_supported_action("scroll"));
        assert!(!is_supported_action("unknown"));
    }

    #[test]
    fn tool_definition_contains_required_fields() {
        let tool = WebBrowserTool;
        let def = tool.definition();
        assert_eq!(def.function.name, "web_browser");

        let required = def
            .function
            .parameters
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required should be array");

        let required_values = required
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>();
        assert!(required_values.contains(&"action"));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_url_without_browser() {
        let tool = WebBrowserTool;
        let out = tool
            .execute(json!({
                "action": "open",
                "url": "file:///tmp/a.html"
            }))
            .await
            .expect("execute should return structured error");

        assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            out.get("error").and_then(|v| v.as_str()),
            Some("仅允许 http/https URL")
        );
    }

    #[tokio::test]
    async fn execute_rejects_unsupported_action_before_browser_launch() {
        let tool = WebBrowserTool;
        let out = tool
            .execute(json!({
                "action": "unknown",
                "url": "https://example.com"
            }))
            .await
            .expect("execute should return structured error");

        assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            out.get("error").and_then(|v| v.as_str()),
            Some("action 仅支持 open/close/sessions/goto/scroll/html/minihtml/content/click/input/screenshot")
        );
    }
}
