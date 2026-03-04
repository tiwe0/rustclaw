use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::tools::{ToolPlugin, truncate_utf8};
use crate::types::{ToolDefinition, ToolSchema};

const DEFAULT_TIMEOUT_SECS: u64 = 20;
const MAX_OUTPUT_LEN: usize = 4000;
const DEFAULT_GESTURE_BACKEND: &str = "evdev";
#[cfg(target_os = "linux")]
const DEFAULT_EVDEV_DEVICE: &str = "/dev/input/event0";

pub struct MobileTool;

#[async_trait]
impl ToolPlugin for MobileTool {
    fn name(&self) -> &'static str {
        "mobile_tool"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "手机端自动化工具（基于 adb），支持设备查询、点击、滑动、输入、按键与截图。"
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "description": "支持: devices, shell, tap, swipe, text, keyevent, screenshot"
                        },
                        "serial": {
                            "type": "string",
                            "description": "可选设备序列号（多设备场景建议传）"
                        },
                        "timeout_seconds": {
                            "type": "integer",
                            "description": "执行超时秒数，默认 20"
                        },
                        "command": {
                            "type": "string",
                            "description": "shell 动作命令内容"
                        },
                        "x": { "type": "integer", "description": "tap x 坐标" },
                        "y": { "type": "integer", "description": "tap y 坐标" },
                        "x1": { "type": "integer", "description": "swipe 起点 x" },
                        "y1": { "type": "integer", "description": "swipe 起点 y" },
                        "x2": { "type": "integer", "description": "swipe 终点 x" },
                        "y2": { "type": "integer", "description": "swipe 终点 y" },
                        "duration_ms": { "type": "integer", "description": "swipe 时长毫秒，默认 300" },
                        "text": { "type": "string", "description": "text 动作输入文本" },
                        "keycode": { "type": "string", "description": "keyevent 键值（如 HOME/BACK/66）" },
                        "path": { "type": "string", "description": "screenshot 保存路径（默认临时目录）" }
                        ,"gesture_backend": { "type": "string", "description": "手势后端：evdev 或 adb（仅 tap/swipe 生效，默认 evdev）" }
                        ,"evdev_device": { "type": "string", "description": "evdev 设备路径（默认 /dev/input/event0）" }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> anyhow::Result<Value> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_lowercase())
            .context("mobile_tool 缺少 action")?;

        let serial = args
            .get("serial")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string());

        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, 120);

        match action.as_str() {
            "devices" => run_devices(serial, timeout_secs).await,
            "shell" => run_shell(serial, &args, timeout_secs).await,
            "tap" => run_tap(serial, &args, timeout_secs).await,
            "swipe" => run_swipe(serial, &args, timeout_secs).await,
            "text" => run_text(serial, &args, timeout_secs).await,
            "keyevent" => run_keyevent(serial, &args, timeout_secs).await,
            "screenshot" => run_screenshot(serial, &args, timeout_secs).await,
            _ => Ok(json!({
                "ok": false,
                "error": "action 仅支持 devices/shell/tap/swipe/text/keyevent/screenshot"
            })),
        }
    }
}

async fn run_devices(serial: Option<String>, timeout_secs: u64) -> anyhow::Result<Value> {
    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.push("devices".to_string());
    build_output("devices", &adb_args, timeout_secs).await
}

async fn run_shell(serial: Option<String>, args: &Value, timeout_secs: u64) -> anyhow::Result<Value> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .context("shell 动作缺少 command")?;

    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.push("shell".to_string());
    adb_args.push(command.to_string());
    build_output("shell", &adb_args, timeout_secs).await
}

async fn run_tap(serial: Option<String>, args: &Value, timeout_secs: u64) -> anyhow::Result<Value> {
    let x = args.get("x").and_then(|v| v.as_i64()).context("tap 动作缺少 x")?;
    let y = args.get("y").and_then(|v| v.as_i64()).context("tap 动作缺少 y")?;

    let backend = resolve_gesture_backend(args);
    if backend == "evdev" {
        return run_evdev_tap(args, x, y, timeout_secs).await;
    }

    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.extend([
        "shell".to_string(),
        "input".to_string(),
        "tap".to_string(),
        x.to_string(),
        y.to_string(),
    ]);

    let mut out = build_output("tap", &adb_args, timeout_secs).await?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("x".to_string(), json!(x));
        obj.insert("y".to_string(), json!(y));
        obj.insert("backend".to_string(), json!("adb"));
    }
    Ok(out)
}

async fn run_swipe(serial: Option<String>, args: &Value, timeout_secs: u64) -> anyhow::Result<Value> {
    let x1 = args.get("x1").and_then(|v| v.as_i64()).context("swipe 动作缺少 x1")?;
    let y1 = args.get("y1").and_then(|v| v.as_i64()).context("swipe 动作缺少 y1")?;
    let x2 = args.get("x2").and_then(|v| v.as_i64()).context("swipe 动作缺少 x2")?;
    let y2 = args.get("y2").and_then(|v| v.as_i64()).context("swipe 动作缺少 y2")?;
    let duration_ms = args
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(300)
        .clamp(50, 60_000);

    let backend = resolve_gesture_backend(args);
    if backend == "evdev" {
        return run_evdev_swipe(args, x1, y1, x2, y2, duration_ms, timeout_secs).await;
    }

    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.extend([
        "shell".to_string(),
        "input".to_string(),
        "swipe".to_string(),
        x1.to_string(),
        y1.to_string(),
        x2.to_string(),
        y2.to_string(),
        duration_ms.to_string(),
    ]);

    let mut out = build_output("swipe", &adb_args, timeout_secs).await?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("x1".to_string(), json!(x1));
        obj.insert("y1".to_string(), json!(y1));
        obj.insert("x2".to_string(), json!(x2));
        obj.insert("y2".to_string(), json!(y2));
        obj.insert("duration_ms".to_string(), json!(duration_ms));
        obj.insert("backend".to_string(), json!("adb"));
    }
    Ok(out)
}

fn resolve_gesture_backend(args: &Value) -> String {
    args.get("gesture_backend")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_GESTURE_BACKEND.to_string())
}

#[cfg(target_os = "linux")]
fn resolve_evdev_device(args: &Value) -> String {
    args.get("evdev_device")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_EVDEV_DEVICE)
        .to_string()
}

#[cfg(target_os = "linux")]
async fn run_evdev_tap(args: &Value, x: i64, y: i64, timeout_secs: u64) -> anyhow::Result<Value> {
    let device = resolve_evdev_device(args);
    run_evemu_event(&device, "EV_ABS", "ABS_X", &x.to_string(), timeout_secs).await?;
    run_evemu_event(&device, "EV_ABS", "ABS_Y", &y.to_string(), timeout_secs).await?;
    run_evemu_event(&device, "EV_KEY", "BTN_TOUCH", "1", timeout_secs).await?;
    run_evemu_event(&device, "EV_SYN", "SYN_REPORT", "0", timeout_secs).await?;
    run_evemu_event(&device, "EV_KEY", "BTN_TOUCH", "0", timeout_secs).await?;
    run_evemu_event(&device, "EV_SYN", "SYN_REPORT", "0", timeout_secs).await?;

    Ok(json!({
        "ok": true,
        "action": "tap",
        "backend": "evdev",
        "evdev_device": device,
        "x": x,
        "y": y,
    }))
}

#[cfg(not(target_os = "linux"))]
async fn run_evdev_tap(_args: &Value, _x: i64, _y: i64, _timeout_secs: u64) -> anyhow::Result<Value> {
    Ok(json!({
        "ok": false,
        "action": "tap",
        "backend": "evdev",
        "error": "evdev 手势仅支持 Linux；当前系统请使用 gesture_backend=adb"
    }))
}

#[cfg(target_os = "linux")]
async fn run_evdev_swipe(
    args: &Value,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
    duration_ms: u64,
    timeout_secs: u64,
) -> anyhow::Result<Value> {
    let device = resolve_evdev_device(args);
    let steps = (duration_ms / 16).clamp(2, 240) as i64;

    run_evemu_event(&device, "EV_ABS", "ABS_X", &x1.to_string(), timeout_secs).await?;
    run_evemu_event(&device, "EV_ABS", "ABS_Y", &y1.to_string(), timeout_secs).await?;
    run_evemu_event(&device, "EV_KEY", "BTN_TOUCH", "1", timeout_secs).await?;
    run_evemu_event(&device, "EV_SYN", "SYN_REPORT", "0", timeout_secs).await?;

    for idx in 1..=steps {
        let x = x1 + (x2 - x1) * idx / steps;
        let y = y1 + (y2 - y1) * idx / steps;
        run_evemu_event(&device, "EV_ABS", "ABS_X", &x.to_string(), timeout_secs).await?;
        run_evemu_event(&device, "EV_ABS", "ABS_Y", &y.to_string(), timeout_secs).await?;
        run_evemu_event(&device, "EV_SYN", "SYN_REPORT", "0", timeout_secs).await?;
    }

    run_evemu_event(&device, "EV_KEY", "BTN_TOUCH", "0", timeout_secs).await?;
    run_evemu_event(&device, "EV_SYN", "SYN_REPORT", "0", timeout_secs).await?;

    Ok(json!({
        "ok": true,
        "action": "swipe",
        "backend": "evdev",
        "evdev_device": device,
        "x1": x1,
        "y1": y1,
        "x2": x2,
        "y2": y2,
        "duration_ms": duration_ms,
        "steps": steps,
    }))
}

#[cfg(not(target_os = "linux"))]
async fn run_evdev_swipe(
    _args: &Value,
    _x1: i64,
    _y1: i64,
    _x2: i64,
    _y2: i64,
    _duration_ms: u64,
    _timeout_secs: u64,
) -> anyhow::Result<Value> {
    Ok(json!({
        "ok": false,
        "action": "swipe",
        "backend": "evdev",
        "error": "evdev 手势仅支持 Linux；当前系统请使用 gesture_backend=adb"
    }))
}

#[cfg(target_os = "linux")]
async fn run_evemu_event(
    device: &str,
    event_type: &str,
    event_code: &str,
    value: &str,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let mut cmd = Command::new("evemu-event");
    cmd.arg(device)
        .arg("--type")
        .arg(event_type)
        .arg("--code")
        .arg(event_code)
        .arg("--value")
        .arg(value);

    match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(res) => {
            let out = res.with_context(|| "执行 evemu-event 失败，请确认已安装 evemu-tools")?;
            if out.status.success() {
                Ok(())
            } else {
                let stderr = truncate_utf8(&String::from_utf8_lossy(&out.stderr), MAX_OUTPUT_LEN);
                Err(anyhow::anyhow!(
                    "evemu-event 执行失败: type={} code={} value={} stderr={}",
                    event_type,
                    event_code,
                    value,
                    stderr
                ))
            }
        }
        Err(_) => Err(anyhow::anyhow!("evemu-event 命令超时（{}s）", timeout_secs)),
    }
}

async fn run_text(serial: Option<String>, args: &Value, timeout_secs: u64) -> anyhow::Result<Value> {
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .context("text 动作缺少 text")?;

    let escaped = text.replace(' ', "%s");
    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.extend([
        "shell".to_string(),
        "input".to_string(),
        "text".to_string(),
        escaped,
    ]);

    let mut out = build_output("text", &adb_args, timeout_secs).await?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("text_len".to_string(), json!(text.chars().count()));
    }
    Ok(out)
}

async fn run_keyevent(serial: Option<String>, args: &Value, timeout_secs: u64) -> anyhow::Result<Value> {
    let keycode = args
        .get("keycode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .context("keyevent 动作缺少 keycode")?;

    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.extend([
        "shell".to_string(),
        "input".to_string(),
        "keyevent".to_string(),
        keycode.to_string(),
    ]);

    let mut out = build_output("keyevent", &adb_args, timeout_secs).await?;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("keycode".to_string(), json!(keycode));
    }
    Ok(out)
}

async fn run_screenshot(serial: Option<String>, args: &Value, timeout_secs: u64) -> anyhow::Result<Value> {
    let output_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_screenshot_path);

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("创建截图目录失败: {}", parent.display()))?;
        }
    }

    let mut adb_args = Vec::new();
    push_serial_args(&mut adb_args, serial.as_deref());
    adb_args.extend(["exec-out".to_string(), "screencap".to_string(), "-p".to_string()]);

    let output = run_adb(&adb_args, timeout_secs).await;
    match output {
        Ok(out) => {
            if !out.status.success() {
                return Ok(json!({
                    "ok": false,
                    "action": "screenshot",
                    "exit_code": out.status.code(),
                    "stderr": truncate_utf8(&String::from_utf8_lossy(&out.stderr), MAX_OUTPUT_LEN),
                }));
            }

            tokio::fs::write(&output_path, &out.stdout)
                .await
                .with_context(|| format!("写入截图失败: {}", output_path.display()))?;

            Ok(json!({
                "ok": true,
                "action": "screenshot",
                "path": output_path.display().to_string(),
                "bytes": out.stdout.len(),
            }))
        }
        Err(err) => Ok(json!({
            "ok": false,
            "action": "screenshot",
            "error": err.to_string(),
        })),
    }
}

async fn build_output(action: &str, adb_args: &[String], timeout_secs: u64) -> anyhow::Result<Value> {
    let output = run_adb(adb_args, timeout_secs).await;
    match output {
        Ok(out) => Ok(json!({
            "ok": out.status.success(),
            "action": action,
            "exit_code": out.status.code(),
            "stdout": truncate_utf8(&String::from_utf8_lossy(&out.stdout), MAX_OUTPUT_LEN),
            "stderr": truncate_utf8(&String::from_utf8_lossy(&out.stderr), MAX_OUTPUT_LEN),
        })),
        Err(err) => Ok(json!({
            "ok": false,
            "action": action,
            "error": err.to_string(),
        })),
    }
}

async fn run_adb(adb_args: &[String], timeout_secs: u64) -> anyhow::Result<std::process::Output> {
    let mut cmd = Command::new("adb");
    cmd.args(adb_args);

    match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
        Ok(res) => res.with_context(|| "执行 adb 命令失败，请确认 adb 已安装且在 PATH 中"),
        Err(_) => Err(anyhow::anyhow!("adb 命令超时（{}s）", timeout_secs)),
    }
}

fn push_serial_args(adb_args: &mut Vec<String>, serial: Option<&str>) {
    if let Some(serial) = serial {
        adb_args.push("-s".to_string());
        adb_args.push(serial.to_string());
    }
}

fn default_screenshot_path() -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::env::temp_dir().join(format!("rustclaw_mobile_shot_{ts}.png"))
}
