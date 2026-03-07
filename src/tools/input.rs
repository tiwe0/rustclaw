use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

const DEFAULT_TAP_HOLD_MS: u64 = 40;
const DEFAULT_LONG_PRESS_MS: u64 = 900;
const DEFAULT_SWIPE_DURATION_MS: u64 = 320;
const DEFAULT_TIMEOUT_SECS: u64 = 10;

pub struct InputTool;

#[async_trait]
impl ToolPlugin for InputTool {
    fn name(&self) -> &'static str {
        "input"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "Android 输入模拟（evdev）：支持点击 tap、长按 long_press、滑动 swipe。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["tap", "long_press", "swipe"],
                            "description": "输入动作"
                        },
                        "x": { "type": "integer", "description": "tap/long_press 的 X 坐标" },
                        "y": { "type": "integer", "description": "tap/long_press 的 Y 坐标" },
                        "x1": { "type": "integer", "description": "swipe 起点 X" },
                        "y1": { "type": "integer", "description": "swipe 起点 Y" },
                        "x2": { "type": "integer", "description": "swipe 终点 X" },
                        "y2": { "type": "integer", "description": "swipe 终点 Y" },
                        "duration_ms": { "type": "integer", "description": "动作持续时长（毫秒）" },
                        "steps": { "type": "integer", "description": "swipe 轨迹插值步数，默认 16" },
                        "max_x": { "type": "integer", "description": "触摸坐标最大 X（默认 1080）" },
                        "max_y": { "type": "integer", "description": "触摸坐标最大 Y（默认 2400）" }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_lowercase())
            .filter(|v| !v.is_empty())
            .context("input 缺少 action")?;

        let timeout_secs = read_u64(&args, "timeout_seconds")
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, 60);

        match action.as_str() {
            "tap" => {
                let x = require_i32(&args, "x")?;
                let y = require_i32(&args, "y")?;
                let hold_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_TAP_HOLD_MS);

                if hold_ms <= 80 {
                    run_android_input(&format!("input tap {} {}", x, y), timeout_secs).await?;
                } else {
                    run_android_input(
                        &format!("input swipe {} {} {} {} {}", x, y, x, y, hold_ms),
                        timeout_secs,
                    )
                    .await?;
                }

                Ok(json!({
                    "ok": true,
                    "action": "tap",
                    "x": x,
                    "y": y,
                    "duration_ms": hold_ms,
                    "backend": "android-input-cmd"
                }))
            }
            "long_press" => {
                let x = require_i32(&args, "x")?;
                let y = require_i32(&args, "y")?;
                let hold_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_LONG_PRESS_MS).max(200);

                run_android_input(
                    &format!("input swipe {} {} {} {} {}", x, y, x, y, hold_ms),
                    timeout_secs,
                )
                .await?;

                Ok(json!({
                    "ok": true,
                    "action": "long_press",
                    "x": x,
                    "y": y,
                    "duration_ms": hold_ms,
                    "backend": "android-input-cmd"
                }))
            }
            "swipe" => {
                let x1 = require_i32(&args, "x1")?;
                let y1 = require_i32(&args, "y1")?;
                let x2 = require_i32(&args, "x2")?;
                let y2 = require_i32(&args, "y2")?;
                let duration_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_SWIPE_DURATION_MS).max(16);

                run_android_input(
                    &format!("input swipe {} {} {} {} {}", x1, y1, x2, y2, duration_ms),
                    timeout_secs,
                )
                .await?;

                Ok(json!({
                    "ok": true,
                    "action": "swipe",
                    "x1": x1,
                    "y1": y1,
                    "x2": x2,
                    "y2": y2,
                    "duration_ms": duration_ms,
                    "backend": "android-input-cmd"
                }))
            }
            _ => Err(anyhow::anyhow!("未知 action: {}，支持 tap/long_press/swipe", action)),
        }
    }
}

fn require_i32(args: &Value, key: &str) -> Result<i32> {
    read_i32(args, key).with_context(|| format!("input 缺少或非法参数: {}", key))
}

fn read_i32(args: &Value, key: &str) -> Option<i32> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
}

fn read_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

async fn run_android_input(command: &str, timeout_secs: u64) -> Result<()> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());

    let output = match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(result) => result.context("执行 input 命令失败")?,
        Err(_) => {
            return Err(anyhow::anyhow!(
                "input 命令超时（{}s）：{}",
                timeout_secs,
                command
            ));
        }
    };

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(anyhow::anyhow!(
        "input 命令执行失败: command={} stderr={}",
        command,
        stderr
    ))
}
