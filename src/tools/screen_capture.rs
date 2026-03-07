use anyhow::Result;
#[cfg(any(target_os = "linux", target_os = "android"))]
use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
#[cfg(any(target_os = "linux", target_os = "android"))]
use base64::Engine;
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::process::Stdio;
#[cfg(any(target_os = "linux", target_os = "android"))]
use tokio::process::Command;
#[cfg(any(target_os = "linux", target_os = "android"))]
use tokio::time::{timeout, Duration, Instant};

use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

#[cfg(any(target_os = "linux", target_os = "android"))]
const DEFAULT_TIMEOUT_SECS: u64 = 15;

pub struct ScreenCaptureTool;

#[async_trait]
impl ToolPlugin for ScreenCaptureTool {
    fn name(&self) -> &'static str {
        "screen_capture"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "使用 Android 命令行 screencap 截图，并返回 PNG 的 base64。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "timeout_seconds": {
                            "type": "integer",
                            "description": "命令超时秒数，默认 15"
                        },
                        "with_data_url": {
                            "type": "boolean",
                            "description": "是否附带 data URL（data:image/png;base64,...），默认 true"
                        }
                    },
                    "required": []
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let timeout_secs = args
                .get("timeout_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, 120);

            let with_data_url = args
                .get("with_data_url")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let start = Instant::now();
            let capture_result = capture_with_screencap(timeout_secs).await?;
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let (width, height) = read_screen_size(timeout_secs).await.unwrap_or((None, None));

            if !capture_result.ok {
                return Ok(json!({
                    "ok": false,
                    "bytes": 0,
                    "width": width,
                    "height": height,
                    "elapsed_ms": elapsed_ms,
                    "stderr": capture_result.stderr,
                    "backend": "screencap"
                }));
            }

            let image_base64 = base64::engine::general_purpose::STANDARD.encode(&capture_result.image_bytes);
            let data_url = if with_data_url {
                Some(format!("data:image/png;base64,{}", image_base64))
            } else {
                None
            };

            return Ok(json!({
                "ok": true,
                "bytes": capture_result.image_bytes.len(),
                "width": width,
                "height": height,
                "mime_type": "image/png",
                "image_base64": image_base64,
                "data_url": data_url,
                "elapsed_ms": elapsed_ms,
                "stderr": capture_result.stderr,
                "backend": "screencap"
            }));
        }

        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let _ = args;
            Ok(json!({
                "ok": false,
                "error": "screen_capture 仅支持 Linux/Android（需要系统 screencap 命令）",
                "backend": "screencap"
            }))
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
struct CaptureResult {
    ok: bool,
    image_bytes: Vec<u8>,
    stderr: String,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn capture_with_screencap(timeout_secs: u64) -> Result<CaptureResult> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("screencap -p");
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let output = match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(result) => result.context("执行 screencap 失败")?,
        Err(_) => {
            return Ok(CaptureResult {
                ok: false,
                image_bytes: Vec::new(),
                stderr: format!("screencap timeout after {}s", timeout_secs),
            });
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    Ok(CaptureResult {
        ok: output.status.success(),
        image_bytes: output.stdout,
        stderr,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
async fn read_screen_size(timeout_secs: u64) -> Result<(Option<u32>, Option<u32>)> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg("wm size");
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());

    let output = match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(result) => result.context("执行 wm size 失败")?,
        Err(_) => return Ok((None, None)),
    };

    if !output.status.success() {
        return Ok((None, None));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_wm_size(&text))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn parse_wm_size(text: &str) -> (Option<u32>, Option<u32>) {
    for line in text.lines() {
        let candidate = line.split(':').nth(1).unwrap_or(line).trim();
        if let Some((w, h)) = candidate.split_once('x') {
            let width = w.trim().parse::<u32>().ok();
            let height = h.trim().parse::<u32>().ok();
            if width.is_some() && height.is_some() {
                return (width, height);
            }
        }
    }
    (None, None)
}
