use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

const DEFAULT_TAP_HOLD_MS: u64 = 40;
const DEFAULT_LONG_PRESS_MS: u64 = 900;
const DEFAULT_SWIPE_DURATION_MS: u64 = 320;
const DEFAULT_SWIPE_STEPS: usize = 18;
const MIN_SWIPE_STEPS: usize = 6;
const MAX_SWIPE_STEPS: usize = 64;
const DEFAULT_MAX_X: i32 = 1080;
const DEFAULT_MAX_Y: i32 = 2400;
const MIN_SEGMENT_DURATION_MS: u64 = 8;
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
                description: "Android 输入模拟：支持 tap/long_press/swipe/back/home/recent_apps/notifications/quick_settings/power_short/power_long/volume_up/volume_down。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": [
                                "tap",
                                "long_press",
                                "swipe",
                                "back",
                                "home",
                                "recent_apps",
                                "notifications",
                                "quick_settings",
                                "power_short",
                                "power_long",
                                "volume_up",
                                "volume_down"
                            ],
                            "description": "输入动作"
                        },
                        "x": { "type": "integer", "description": "tap/long_press 的 X 坐标" },
                        "y": { "type": "integer", "description": "tap/long_press 的 Y 坐标" },
                        "x1": { "type": "integer", "description": "swipe 起点 X" },
                        "y1": { "type": "integer", "description": "swipe 起点 Y" },
                        "x2": { "type": "integer", "description": "swipe 终点 X" },
                        "y2": { "type": "integer", "description": "swipe 终点 Y" },
                        "duration_ms": { "type": "integer", "description": "动作持续时长（毫秒）" },
                        "steps": { "type": "integer", "description": "swipe 轨迹点数量（非均匀分布，默认 18）" },
                        "swipe_profile": {
                            "type": "string",
                            "enum": ["human_fast", "human_natural", "human_slow"],
                            "description": "swipe 轨迹预设（会自动调 duration/steps/noise）"
                        },
                        "noise_px": { "type": "integer", "description": "轨迹随机噪音像素半径（默认按滑动距离自动计算）" },
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
                let profile = read_string(&args, "swipe_profile");

                let mut duration_ms = read_u64(&args, "duration_ms").unwrap_or(DEFAULT_SWIPE_DURATION_MS).max(16);
                let mut steps = read_u64(&args, "steps")
                    .map(|v| v as usize)
                    .unwrap_or(DEFAULT_SWIPE_STEPS)
                    .clamp(MIN_SWIPE_STEPS, MAX_SWIPE_STEPS);
                let max_x = read_i32(&args, "max_x").unwrap_or(DEFAULT_MAX_X).max(1);
                let max_y = read_i32(&args, "max_y").unwrap_or(DEFAULT_MAX_Y).max(1);

                let distance = euclidean_distance((x1, y1), (x2, y2));
                let auto_noise = ((distance / 36.0).round() as i32).clamp(2, 18);
                let mut noise_px = read_i32(&args, "noise_px").unwrap_or(auto_noise).clamp(0, 48);

                if let Some(profile_name) = profile.as_deref() {
                    let tuned = tune_swipe_profile(profile_name, duration_ms, steps, noise_px);
                    duration_ms = tuned.duration_ms;
                    steps = tuned.steps;
                    noise_px = tuned.noise_px;
                }

                let seed = build_swipe_seed(x1, y1, x2, y2, duration_ms);
                let mut rng = SimpleRng::new(seed);
                let points = build_swipe_trajectory(
                    (x1, y1),
                    (x2, y2),
                    steps,
                    max_x,
                    max_y,
                    noise_px,
                    &mut rng,
                );

                run_android_swipe_path(&points, duration_ms, timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "swipe",
                    "x1": x1,
                    "y1": y1,
                    "x2": x2,
                    "y2": y2,
                    "duration_ms": duration_ms,
                    "steps": points.len(),
                    "noise_px": noise_px,
                    "swipe_profile": profile,
                    "backend": "android-input-cmd"
                }))
            }
            "back" => {
                run_android_input("input keyevent 4", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "back",
                    "keyevent": 4,
                    "backend": "android-input-cmd"
                }))
            }
            "home" => {
                run_android_input("input keyevent 3", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "home",
                    "keyevent": 3,
                    "backend": "android-input-cmd"
                }))
            }
            "recent_apps" => {
                run_android_input("input keyevent 187", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "recent_apps",
                    "keyevent": 187,
                    "backend": "android-input-cmd"
                }))
            }
            "notifications" => {
                run_android_input("input keyevent 83", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "notifications",
                    "keyevent": 83,
                    "backend": "android-input-cmd"
                }))
            }
            "quick_settings" => {
                run_android_input("input keyevent 280", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "quick_settings",
                    "keyevent": 280,
                    "backend": "android-input-cmd"
                }))
            }
            "power_short" => {
                run_android_input("input keyevent 26", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "power_short",
                    "keyevent": 26,
                    "backend": "android-input-cmd"
                }))
            }
            "power_long" => {
                run_android_input("input keyevent --longpress 26", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "power_long",
                    "keyevent": 26,
                    "long_press": true,
                    "backend": "android-input-cmd"
                }))
            }
            "volume_up" => {
                run_android_input("input keyevent 24", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "volume_up",
                    "keyevent": 24,
                    "backend": "android-input-cmd"
                }))
            }
            "volume_down" => {
                run_android_input("input keyevent 25", timeout_secs).await?;

                Ok(json!({
                    "ok": true,
                    "action": "volume_down",
                    "keyevent": 25,
                    "backend": "android-input-cmd"
                }))
            }
            _ => Err(anyhow::anyhow!(
                "未知 action: {}，支持 tap/long_press/swipe/back/home/recent_apps/notifications/quick_settings/power_short/power_long/volume_up/volume_down",
                action
            )),
        }
    }
}

struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        let init = if seed == 0 { 0x9e3779b97f4a7c15 } else { seed };
        Self { state: init }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    fn next_f64(&mut self) -> f64 {
        let value = self.next_u64() >> 11;
        (value as f64) / ((1u64 << 53) as f64)
    }

    fn range_f64(&mut self, min: f64, max: f64) -> f64 {
        min + self.next_f64() * (max - min)
    }
}

fn build_swipe_seed(x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u64) -> u64 {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    now_nanos
        ^ ((x1 as i64 as u64).wrapping_mul(0x9E3779B185EBCA87))
        ^ ((y1 as i64 as u64).wrapping_mul(0xC2B2AE3D27D4EB4F))
        ^ ((x2 as i64 as u64).wrapping_mul(0x165667B19E3779F9))
        ^ ((y2 as i64 as u64).wrapping_mul(0x85EBCA77C2B2AE63))
        ^ duration_ms.wrapping_mul(0x27D4EB2F165667C5)
}

fn build_swipe_trajectory(
    start: (i32, i32),
    end: (i32, i32),
    steps: usize,
    max_x: i32,
    max_y: i32,
    noise_px: i32,
    rng: &mut SimpleRng,
) -> Vec<(i32, i32)> {
    let p0 = (start.0 as f64, start.1 as f64);
    let p3 = (end.0 as f64, end.1 as f64);

    let dx = p3.0 - p0.0;
    let dy = p3.1 - p0.1;
    let dist = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / dist;
    let uy = dy / dist;
    let px = -uy;
    let py = ux;

    let bend = dist * rng.range_f64(0.08, 0.22);
    let cp1 = (
        p0.0 + ux * dist * rng.range_f64(0.20, 0.38) + px * rng.range_f64(-bend, bend),
        p0.1 + uy * dist * rng.range_f64(0.20, 0.38) + py * rng.range_f64(-bend, bend),
    );
    let cp2 = (
        p0.0 + ux * dist * rng.range_f64(0.62, 0.84) + px * rng.range_f64(-bend, bend),
        p0.1 + uy * dist * rng.range_f64(0.62, 0.84) + py * rng.range_f64(-bend, bend),
    );

    let ts = non_uniform_ts(steps, rng);

    let mut points: Vec<(i32, i32)> = Vec::with_capacity(steps);
    for t in ts {
        let omt = 1.0 - t;
        let x = omt * omt * omt * p0.0
            + 3.0 * omt * omt * t * cp1.0
            + 3.0 * omt * t * t * cp2.0
            + t * t * t * p3.0;
        let y = omt * omt * omt * p0.1
            + 3.0 * omt * omt * t * cp1.1
            + 3.0 * omt * t * t * cp2.1
            + t * t * t * p3.1;

        let center_weight = (1.0 - (2.0 * t - 1.0).abs()).max(0.0);
        let noise = noise_px as f64 * center_weight;
        let jitter_x = px * rng.range_f64(-noise, noise) + ux * rng.range_f64(-noise * 0.25, noise * 0.25);
        let jitter_y = py * rng.range_f64(-noise, noise) + uy * rng.range_f64(-noise * 0.25, noise * 0.25);

        let ix = clamp_i32((x + jitter_x).round() as i32, 0, max_x);
        let iy = clamp_i32((y + jitter_y).round() as i32, 0, max_y);

        if points.last().copied() != Some((ix, iy)) {
            points.push((ix, iy));
        }
    }

    if points.is_empty() {
        points.push((clamp_i32(start.0, 0, max_x), clamp_i32(start.1, 0, max_y)));
    }

    if points.first().copied() != Some((start.0, start.1)) {
        points.insert(0, (clamp_i32(start.0, 0, max_x), clamp_i32(start.1, 0, max_y)));
    }

    if points.last().copied() != Some((end.0, end.1)) {
        points.push((clamp_i32(end.0, 0, max_x), clamp_i32(end.1, 0, max_y)));
    }

    if points.len() < 2 {
        points.push((clamp_i32(end.0, 0, max_x), clamp_i32(end.1, 0, max_y)));
    }

    points
}

fn non_uniform_ts(steps: usize, rng: &mut SimpleRng) -> Vec<f64> {
    if steps <= 2 {
        return vec![0.0, 1.0];
    }

    let mut weights = Vec::with_capacity(steps - 1);
    for idx in 0..(steps - 1) {
        let ratio = idx as f64 / ((steps - 2) as f64);
        let phase_bias = if ratio < 0.4 {
            rng.range_f64(0.35, 0.90)
        } else if ratio < 0.75 {
            rng.range_f64(1.00, 1.85)
        } else {
            rng.range_f64(0.55, 1.30)
        };
        let random_weight = rng.range_f64(0.50, 1.75);
        weights.push((phase_bias * random_weight).max(0.05));
    }

    let total: f64 = weights.iter().sum::<f64>().max(1e-6);
    let mut ts = Vec::with_capacity(steps);
    ts.push(0.0);

    let mut acc = 0.0;
    for w in weights {
        acc += w;
        let t = (acc / total).clamp(0.0, 1.0);
        ts.push(t);
    }

    if let Some(last) = ts.last_mut() {
        *last = 1.0;
    }

    ts
}

async fn run_android_swipe_path(
    points: &[(i32, i32)],
    duration_ms: u64,
    timeout_secs: u64,
) -> Result<()> {
    if points.len() < 2 {
        return Ok(());
    }

    let mut segment_lengths = Vec::with_capacity(points.len() - 1);
    let mut sum_len = 0.0_f64;
    for idx in 0..(points.len() - 1) {
        let length = euclidean_distance(points[idx], points[idx + 1]).max(1.0);
        segment_lengths.push(length);
        sum_len += length;
    }

    let mut used_ms = 0_u64;
    for idx in 0..(points.len() - 1) {
        let (sx, sy) = points[idx];
        let (ex, ey) = points[idx + 1];

        let segment_ms = if idx == points.len() - 2 {
            duration_ms.saturating_sub(used_ms).max(MIN_SEGMENT_DURATION_MS)
        } else {
            let portion = (segment_lengths[idx] / sum_len).clamp(0.0, 1.0);
            let allocated = ((duration_ms as f64) * portion).round() as u64;
            let seg_ms = allocated.max(MIN_SEGMENT_DURATION_MS);
            used_ms = used_ms.saturating_add(seg_ms);
            seg_ms
        };

        run_android_input(
            &format!("input swipe {} {} {} {} {}", sx, sy, ex, ey, segment_ms),
            timeout_secs,
        )
        .await?;
    }

    Ok(())
}

fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
    value.clamp(min, max)
}

fn euclidean_distance(a: (i32, i32), b: (i32, i32)) -> f64 {
    let dx = (b.0 - a.0) as f64;
    let dy = (b.1 - a.1) as f64;
    (dx * dx + dy * dy).sqrt()
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

fn read_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
}

struct SwipeTune {
    duration_ms: u64,
    steps: usize,
    noise_px: i32,
}

fn tune_swipe_profile(profile: &str, duration_ms: u64, steps: usize, noise_px: i32) -> SwipeTune {
    match profile {
        "human_fast" => SwipeTune {
            duration_ms: (duration_ms.saturating_mul(70) / 100).clamp(120, 520),
            steps: ((steps.saturating_mul(80)) / 100).clamp(MIN_SWIPE_STEPS, 28),
            noise_px: (noise_px.saturating_mul(75) / 100).clamp(1, 28),
        },
        "human_natural" => SwipeTune {
            duration_ms: duration_ms.clamp(180, 980),
            steps: steps.clamp(10, 36),
            noise_px: noise_px.clamp(2, 24),
        },
        "human_slow" => SwipeTune {
            duration_ms: (duration_ms.saturating_mul(145) / 100).clamp(260, 1800),
            steps: ((steps.saturating_mul(140)) / 100).clamp(10, MAX_SWIPE_STEPS),
            noise_px: (noise_px.saturating_mul(130) / 100).clamp(2, 48),
        },
        _ => SwipeTune {
            duration_ms,
            steps,
            noise_px,
        },
    }
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
