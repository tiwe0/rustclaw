use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
#[cfg(any(target_os = "linux", target_os = "android"))]
use tokio::time::{sleep, Duration};

use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

#[cfg(any(target_os = "linux", target_os = "android"))]
const DEFAULT_TAP_HOLD_MS: u64 = 40;
#[cfg(any(target_os = "linux", target_os = "android"))]
const DEFAULT_LONG_PRESS_MS: u64 = 900;
#[cfg(any(target_os = "linux", target_os = "android"))]
const DEFAULT_SWIPE_DURATION_MS: u64 = 320;
#[cfg(any(target_os = "linux", target_os = "android"))]
const DEFAULT_SWIPE_STEPS: u32 = 16;
const DEFAULT_MAX_X: i32 = 1080;
const DEFAULT_MAX_Y: i32 = 2400;

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

        let max_x = read_i32(&args, "max_x").unwrap_or(DEFAULT_MAX_X).max(1);
        let max_y = read_i32(&args, "max_y").unwrap_or(DEFAULT_MAX_Y).max(1);

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let injector = EvdevInjector::new(max_x, max_y)?;

            match action.as_str() {
                "tap" => {
                    let x = require_i32(&args, "x")?;
                    let y = require_i32(&args, "y")?;
                    let hold_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_TAP_HOLD_MS);
                    injector.tap(x, y, hold_ms).await?;
                    Ok(json!({
                        "ok": true,
                        "action": "tap",
                        "x": x,
                        "y": y,
                        "duration_ms": hold_ms,
                        "backend": "evdev-uinput"
                    }))
                }
                "long_press" => {
                    let x = require_i32(&args, "x")?;
                    let y = require_i32(&args, "y")?;
                    let hold_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_LONG_PRESS_MS).max(200);
                    injector.tap(x, y, hold_ms).await?;
                    Ok(json!({
                        "ok": true,
                        "action": "long_press",
                        "x": x,
                        "y": y,
                        "duration_ms": hold_ms,
                        "backend": "evdev-uinput"
                    }))
                }
                "swipe" => {
                    let x1 = require_i32(&args, "x1")?;
                    let y1 = require_i32(&args, "y1")?;
                    let x2 = require_i32(&args, "x2")?;
                    let y2 = require_i32(&args, "y2")?;
                    let duration_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_SWIPE_DURATION_MS).max(16);
                    let steps = read_u32(&args, "steps").unwrap_or(DEFAULT_SWIPE_STEPS).clamp(2, 240);
                    injector.swipe(x1, y1, x2, y2, duration_ms, steps).await?;
                    Ok(json!({
                        "ok": true,
                        "action": "swipe",
                        "x1": x1,
                        "y1": y1,
                        "x2": x2,
                        "y2": y2,
                        "duration_ms": duration_ms,
                        "steps": steps,
                        "backend": "evdev-uinput"
                    }))
                }
                _ => Err(anyhow::anyhow!("未知 action: {}，支持 tap/long_press/swipe", action)),
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let _ = max_x;
            let _ = max_y;
            Ok(json!({
                "ok": false,
                "action": action,
                "error": "input 工具仅支持 Linux/Android（evdev/uinput）。当前平台不可用。",
                "backend": "evdev-uinput"
            }))
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn require_i32(args: &Value, key: &str) -> Result<i32> {
    read_i32(args, key).with_context(|| format!("input 缺少或非法参数: {}", key))
}

fn read_i32(args: &Value, key: &str) -> Option<i32> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_u32(args: &Value, key: &str) -> Option<u32> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
struct EvdevInjector {
    device: std::sync::Mutex<evdev::uinput::VirtualDevice>,
    max_x: i32,
    max_y: i32,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
impl EvdevInjector {
    fn new(max_x: i32, max_y: i32) -> Result<Self> {
        use evdev::uinput::VirtualDeviceBuilder;
        use evdev::{AbsoluteAxisCode, AbsoluteAxisInfo, InputId, KeyCode};

        let x_axis = AbsoluteAxisInfo::new(0, 0, max_x, 0, 0, 0);
        let y_axis = AbsoluteAxisInfo::new(0, 0, max_y, 0, 0, 0);

        let mut builder = VirtualDeviceBuilder::new().context("创建虚拟输入设备失败")?;
        builder = builder
            .name("rustclaw-input")
            .input_id(InputId::new(0x03, 0x18D1, 0x4EE7, 0x0001))
            .with_keys(&[KeyCode::BTN_TOUCH])
            .context("配置触摸按键失败")?
            .with_absolute_axis(&AbsoluteAxisCode::ABS_X, x_axis)
            .context("配置 ABS_X 失败")?
            .with_absolute_axis(&AbsoluteAxisCode::ABS_Y, y_axis)
            .context("配置 ABS_Y 失败")?;

        let device = builder.build().context("创建 evdev uinput 设备失败（可能缺少权限）")?;
        Ok(Self {
            device: std::sync::Mutex::new(device),
            max_x,
            max_y,
        })
    }

    async fn tap(&self, x: i32, y: i32, hold_ms: u64) -> Result<()> {
        self.press(x, y)?;
        sleep(Duration::from_millis(hold_ms.max(1))).await;
        self.release()?;
        Ok(())
    }

    async fn swipe(&self, x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u64, steps: u32) -> Result<()> {
        let start_x = self.clamp_x(x1);
        let start_y = self.clamp_y(y1);
        let end_x = self.clamp_x(x2);
        let end_y = self.clamp_y(y2);

        self.press(start_x, start_y)?;

        let step_count = steps.max(2);
        let step_sleep = Duration::from_millis((duration_ms / u64::from(step_count)).max(1));
        for idx in 1..=step_count {
            let t = idx as f32 / step_count as f32;
            let x = lerp_i32(start_x, end_x, t);
            let y = lerp_i32(start_y, end_y, t);
            self.move_to(x, y)?;
            sleep(step_sleep).await;
        }

        self.release()?;
        Ok(())
    }

    fn press(&self, x: i32, y: i32) -> Result<()> {
        use evdev::{AbsoluteAxisCode, EventType, InputEvent, KeyCode, SynchronizationCode};

        let x = self.clamp_x(x);
        let y = self.clamp_y(y);

        let mut dev = self.device.lock().map_err(|_| anyhow::anyhow!("evdev 设备锁失败"))?;
        dev.emit(&[
            InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, x),
            InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0, y),
            InputEvent::new(EventType::KEY, KeyCode::BTN_TOUCH.code(), 1),
            InputEvent::new(EventType::SYNCHRONIZATION, SynchronizationCode::SYN_REPORT.0, 0),
        ])
        .context("发送按下事件失败")
    }

    fn move_to(&self, x: i32, y: i32) -> Result<()> {
        use evdev::{AbsoluteAxisCode, EventType, InputEvent, SynchronizationCode};

        let x = self.clamp_x(x);
        let y = self.clamp_y(y);
        let mut dev = self.device.lock().map_err(|_| anyhow::anyhow!("evdev 设备锁失败"))?;
        dev.emit(&[
            InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, x),
            InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0, y),
            InputEvent::new(EventType::SYNCHRONIZATION, SynchronizationCode::SYN_REPORT.0, 0),
        ])
        .context("发送移动事件失败")
    }

    fn release(&self) -> Result<()> {
        use evdev::{EventType, InputEvent, KeyCode, SynchronizationCode};

        let mut dev = self.device.lock().map_err(|_| anyhow::anyhow!("evdev 设备锁失败"))?;
        dev.emit(&[
            InputEvent::new(EventType::KEY, KeyCode::BTN_TOUCH.code(), 0),
            InputEvent::new(EventType::SYNCHRONIZATION, SynchronizationCode::SYN_REPORT.0, 0),
        ])
        .context("发送抬起事件失败")
    }

    fn clamp_x(&self, x: i32) -> i32 {
        x.clamp(0, self.max_x)
    }

    fn clamp_y(&self, y: i32) -> i32 {
        y.clamp(0, self.max_y)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn lerp_i32(start: i32, end: i32, t: f32) -> i32 {
    let value = start as f32 + (end - start) as f32 * t;
    value.round() as i32
}
