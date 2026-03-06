use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{load_config, resolve_app_base_dir, resolve_config_path};
use crate::cron;
use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

pub struct CronTool;

#[derive(Debug, Deserialize)]
struct CronToolArgs {
    action: String,
    name: Option<String>,
    session: Option<String>,
    prompt: Option<String>,
    minute: Option<String>,
    hour: Option<String>,
    day: Option<String>,
    month: Option<String>,
    weekday: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
struct CronJobsFile {
    #[serde(default)]
    jobs: Vec<CronJobItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CronJobItem {
    name: String,
    prompt: String,
    session: String,
    #[serde(default = "default_cron_field_any")]
    minute: String,
    #[serde(default = "default_cron_field_any")]
    hour: String,
    #[serde(default = "default_cron_field_any")]
    day: String,
    #[serde(default = "default_cron_field_any")]
    month: String,
    #[serde(default = "default_cron_field_any")]
    weekday: String,
    #[serde(default = "default_job_enabled")]
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct CronJobView {
    name: String,
    session: String,
    prompt: String,
    expression: String,
    enabled: bool,
}

#[async_trait]
impl ToolPlugin for CronTool {
    fn name(&self) -> &'static str {
        "cron_job_manager"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "管理内置 cron 对话 job：list/upsert/delete/enable/disable".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["list", "upsert", "delete", "enable", "disable"],
                            "description": "执行动作"
                        },
                        "name": {
                            "type": "string",
                            "description": "job 名称"
                        },
                        "session": {
                            "type": "string",
                            "description": "会话名，默认 new"
                        },
                        "prompt": {
                            "type": "string",
                            "description": "触发时发送给 agent 的提示词"
                        },
                        "minute": {
                            "type": "string",
                            "description": "cron minute 字段，如 * 或 */30"
                        },
                        "hour": {
                            "type": "string",
                            "description": "cron hour 字段"
                        },
                        "day": {
                            "type": "string",
                            "description": "cron day 字段"
                        },
                        "month": {
                            "type": "string",
                            "description": "cron month 字段"
                        },
                        "weekday": {
                            "type": "string",
                            "description": "cron weekday 字段，支持 mon..sun"
                        },
                        "enabled": {
                            "type": "boolean",
                            "description": "upsert 时可显式设置启用状态"
                        }
                    },
                    "required": ["action"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let parsed: CronToolArgs = serde_json::from_value(args).context("解析 cron_tool 参数失败")?;
        let action = parsed.action.trim().to_ascii_lowercase();

        let (jobs_file, mut data) = load_jobs_file_with_path()?;

        match action.as_str() {
            "list" => {
                let views = build_sorted_views(&data.jobs);
                Ok(json!({
                    "ok": true,
                    "action": "list",
                    "jobs_file": jobs_file.display().to_string(),
                    "count": views.len(),
                    "jobs": views
                }))
            }
            "upsert" => {
                let name = required_name(parsed.name.as_deref())?;

                let existing_idx = data.jobs.iter().position(|job| job.name == name);
                let mut job = if let Some(idx) = existing_idx {
                    data.jobs[idx].clone()
                } else {
                    CronJobItem {
                        name: name.clone(),
                        prompt: String::new(),
                        session: "new".to_string(),
                        minute: "*".to_string(),
                        hour: "*".to_string(),
                        day: "*".to_string(),
                        month: "*".to_string(),
                        weekday: "*".to_string(),
                        enabled: true,
                    }
                };

                if let Some(prompt) = parsed.prompt.as_deref() {
                    let text = prompt.trim();
                    if text.is_empty() {
                        return Err(anyhow::anyhow!("prompt 不能为空"));
                    }
                    job.prompt = text.to_string();
                }

                if let Some(session) = parsed.session.as_deref() {
                    let sid = session.trim();
                    job.session = if sid.is_empty() {
                        "new".to_string()
                    } else {
                        sid.to_string()
                    };
                }

                if let Some(minute) = parsed.minute.as_deref() {
                    job.minute = normalize_field(minute);
                }
                if let Some(hour) = parsed.hour.as_deref() {
                    job.hour = normalize_field(hour);
                }
                if let Some(day) = parsed.day.as_deref() {
                    job.day = normalize_field(day);
                }
                if let Some(month) = parsed.month.as_deref() {
                    job.month = normalize_field(month);
                }
                if let Some(weekday) = parsed.weekday.as_deref() {
                    job.weekday = normalize_field(weekday);
                }
                if let Some(enabled) = parsed.enabled {
                    job.enabled = enabled;
                }

                if job.prompt.trim().is_empty() {
                    return Err(anyhow::anyhow!("upsert 新建 job 时必须提供非空 prompt"));
                }

                validate_job_fields(&job)?;

                let created = existing_idx.is_none();
                if let Some(idx) = existing_idx {
                    data.jobs[idx] = job.clone();
                } else {
                    data.jobs.push(job.clone());
                }
                sort_jobs(&mut data.jobs);
                save_jobs_file(&jobs_file, &data)?;
                let runtime_synced = cron::notify_jobs_updated();

                Ok(json!({
                    "ok": true,
                    "action": "upsert",
                    "created": created,
                    "jobs_file": jobs_file.display().to_string(),
                    "runtime_synced": runtime_synced,
                    "job": to_view(&job)
                }))
            }
            "delete" => {
                let name = required_name(parsed.name.as_deref())?;
                let before = data.jobs.len();
                data.jobs.retain(|job| job.name != name);
                let removed = before.saturating_sub(data.jobs.len());

                if removed > 0 {
                    save_jobs_file(&jobs_file, &data)?;
                }
                let runtime_synced = if removed > 0 {
                    cron::notify_jobs_updated()
                } else {
                    false
                };

                Ok(json!({
                    "ok": true,
                    "action": "delete",
                    "jobs_file": jobs_file.display().to_string(),
                    "name": name,
                    "removed": removed,
                    "runtime_synced": runtime_synced
                }))
            }
            "enable" | "disable" => {
                let name = required_name(parsed.name.as_deref())?;
                let target_enabled = action == "enable";
                let mut found = false;

                for job in &mut data.jobs {
                    if job.name == name {
                        job.enabled = target_enabled;
                        found = true;
                        break;
                    }
                }

                if !found {
                    return Err(anyhow::anyhow!("job 不存在: {}", name));
                }

                save_jobs_file(&jobs_file, &data)?;
                let runtime_synced = cron::notify_jobs_updated();
                let views = build_sorted_views(&data.jobs);
                let current = views.into_iter().find(|job| job.name == name);

                Ok(json!({
                    "ok": true,
                    "action": action,
                    "jobs_file": jobs_file.display().to_string(),
                    "runtime_synced": runtime_synced,
                    "job": current
                }))
            }
            _ => Err(anyhow::anyhow!(
                "未知 action: {}，支持 list/upsert/delete/enable/disable",
                action
            )),
        }
    }
}

fn load_jobs_file_with_path() -> Result<(PathBuf, CronJobsFile)> {
    let config_path = resolve_config_path();
    let cfg = load_config(&config_path)?;
    let workspace_root = env::current_dir().context("获取当前工作目录失败")?;
    let app_base_dir = resolve_app_base_dir(&workspace_root, &cfg.base);
    let jobs_file = resolve_jobs_file(&app_base_dir, &cfg.cron.jobs_file);

    if !jobs_file.exists() {
        return Ok((jobs_file, CronJobsFile { jobs: Vec::new() }));
    }

    let content = fs::read_to_string(&jobs_file)
        .with_context(|| format!("读取 cron jobs 文件失败: {}", jobs_file.display()))?;
    if content.trim().is_empty() {
        return Ok((jobs_file, CronJobsFile { jobs: Vec::new() }));
    }

    let mut data: CronJobsFile = toml::from_str(&content)
        .with_context(|| format!("解析 cron jobs TOML 失败: {}", jobs_file.display()))?;
    sort_jobs(&mut data.jobs);
    Ok((jobs_file, data))
}

fn save_jobs_file(path: &Path, data: &CronJobsFile) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建 cron jobs 目录失败: {}", parent.display()))?;
    }

    let content = toml::to_string_pretty(data).context("序列化 cron jobs 失败")?;
    fs::write(path, content).with_context(|| format!("写入 cron jobs 文件失败: {}", path.display()))
}

fn resolve_jobs_file(base_dir: &Path, raw_path: &str) -> PathBuf {
    let p = PathBuf::from(raw_path);
    if p.is_absolute() {
        p
    } else {
        base_dir.join(p)
    }
}

fn required_name(raw: Option<&str>) -> Result<String> {
    let name = raw
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow::anyhow!("需要提供非空 name"))?;
    Ok(name)
}

fn normalize_field(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "*".to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_sorted_views(jobs: &[CronJobItem]) -> Vec<CronJobView> {
    let mut views = jobs.iter().map(to_view).collect::<Vec<_>>();
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

fn to_view(job: &CronJobItem) -> CronJobView {
    CronJobView {
        name: job.name.clone(),
        session: job.session.clone(),
        prompt: job.prompt.clone(),
        expression: format!(
            "{} {} {} {} {}",
            job.minute, job.hour, job.day, job.month, job.weekday
        ),
        enabled: job.enabled,
    }
}

fn sort_jobs(jobs: &mut Vec<CronJobItem>) {
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
}

fn validate_job_fields(job: &CronJobItem) -> Result<()> {
    validate_field(&job.minute, 0, 59, false).with_context(|| {
        format!("job `{}` 的 minute 字段非法: {}", job.name, job.minute)
    })?;
    validate_field(&job.hour, 0, 23, false)
        .with_context(|| format!("job `{}` 的 hour 字段非法: {}", job.name, job.hour))?;
    validate_field(&job.day, 1, 31, false)
        .with_context(|| format!("job `{}` 的 day 字段非法: {}", job.name, job.day))?;
    validate_field(&job.month, 1, 12, false)
        .with_context(|| format!("job `{}` 的 month 字段非法: {}", job.name, job.month))?;
    validate_field(&job.weekday, 0, 7, true)
        .with_context(|| format!("job `{}` 的 weekday 字段非法: {}", job.name, job.weekday))?;
    Ok(())
}

fn validate_field(raw: &str, min: u32, max: u32, allow_weekday_name: bool) -> Result<()> {
    let text = raw.trim();
    if text.is_empty() || text == "*" {
        return Ok(());
    }

    let mut found_any = false;
    for token in text.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }

        if let Some(step_part) = t.strip_prefix("*/") {
            let step: u32 = step_part.parse().context("step 不是整数")?;
            if step == 0 {
                return Err(anyhow::anyhow!("step 不能为 0"));
            }
            found_any = true;
            continue;
        }

        if let Some((start_s, end_s)) = t.split_once('-') {
            let start = parse_single_value(start_s, allow_weekday_name)?;
            let end = parse_single_value(end_s, allow_weekday_name)?;
            let start = normalize_weekday(start);
            let end = normalize_weekday(end);
            if start > end {
                return Err(anyhow::anyhow!("范围起点不能大于终点"));
            }
            if start < min || end > max {
                return Err(anyhow::anyhow!("范围超出允许区间"));
            }
            found_any = true;
            continue;
        }

        let v = normalize_weekday(parse_single_value(t, allow_weekday_name)?);
        if v < min || v > max {
            return Err(anyhow::anyhow!("值超出允许区间"));
        }
        found_any = true;
    }

    if !found_any {
        return Err(anyhow::anyhow!("字段没有有效值"));
    }

    Ok(())
}

fn parse_single_value(raw: &str, allow_weekday_name: bool) -> Result<u32> {
    if allow_weekday_name {
        if let Some(v) = parse_weekday_name(raw) {
            return Ok(v);
        }
    }
    let v: u32 = raw.trim().parse().with_context(|| format!("非法数字: {}", raw))?;
    Ok(v)
}

fn parse_weekday_name(raw: &str) -> Option<u32> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Some(0),
        "mon" | "monday" => Some(1),
        "tue" | "tuesday" => Some(2),
        "wed" | "wednesday" => Some(3),
        "thu" | "thursday" => Some(4),
        "fri" | "friday" => Some(5),
        "sat" | "saturday" => Some(6),
        _ => None,
    }
}

fn normalize_weekday(v: u32) -> u32 {
    if v == 7 { 0 } else { v }
}

fn default_cron_field_any() -> String {
    "*".to_string()
}

fn default_job_enabled() -> bool {
    true
}
