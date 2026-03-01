use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, Timelike, Weekday};
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::Mutex;

use std::sync::Arc;

use crate::app;
use crate::config::{load_config, resolve_app_base_dir, resolve_config_path};
use crate::log;
use crate::session::{session_db_path, session_dir_path};

#[derive(Debug, Clone)]
pub struct CronJob {
    pub name: String,
    pub prompt: String,
    pub session: String,
    pub schedule: CronSchedule,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct CronJobStatus {
    pub name: String,
    pub session: String,
    pub expression: String,
    pub enabled: bool,
    pub running: bool,
}

#[derive(Debug, Clone)]
struct CronJobState {
    job: CronJob,
    last_trigger_slot: Option<i64>,
    running: bool,
}

#[derive(Debug, Clone)]
pub struct CronSchedule {
    minute: CronField,
    hour: CronField,
    day: CronField,
    month: CronField,
    weekday: CronField,
}

#[derive(Debug, Clone)]
enum CronField {
    Any,
    Values(Vec<u32>),
}

#[derive(Debug, Deserialize)]
struct CronJobsFile {
    #[serde(default)]
    jobs: Vec<CronJobItem>,
}

#[derive(Debug, Deserialize)]
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

pub struct CronJobManager {
    jobs: HashMap<String, CronJobState>,
}

impl CronJobManager {
    pub fn from_jobs(jobs: Vec<CronJob>) -> Self {
        let mut manager = Self {
            jobs: HashMap::new(),
        };

        for job in jobs {
            if job.name.is_empty() {
                continue;
            }
            manager.register(job);
        }

        manager
    }

    pub fn register(&mut self, job: CronJob) {
        self.jobs.insert(
            job.name.clone(),
            CronJobState {
                job,
                last_trigger_slot: None,
                running: false,
            },
        );
    }

    pub fn list(&self) -> Vec<CronJobStatus> {
        let mut items = self
            .jobs
            .values()
            .map(|state| CronJobStatus {
                name: state.job.name.clone(),
                session: state.job.session.clone(),
                expression: state.job.schedule.to_expression_string(),
                enabled: state.job.enabled,
                running: state.running,
            })
            .collect::<Vec<_>>();

        items.sort_by(|a, b| a.name.cmp(&b.name));
        items
    }

    pub fn collect_due_jobs(&mut self, now: DateTime<Local>) -> Vec<CronJob> {
        let slot = now.timestamp() / 60;
        let mut due = Vec::new();
        for state in self.jobs.values_mut() {
            if !state.job.enabled || state.running {
                continue;
            }
            if state.job.schedule.matches(now)
                && state.last_trigger_slot.map(|last| last != slot).unwrap_or(true)
            {
                state.running = true;
                state.last_trigger_slot = Some(slot);
                due.push(state.job.clone());
            }
        }
        due
    }

    pub fn mark_finished(&mut self, name: &str) {
        if let Some(state) = self.jobs.get_mut(name) {
            state.running = false;
        }
    }
}

pub async fn run() -> Result<()> {
    let config_path = resolve_config_path();
    let cfg = load_config(&config_path)?;

    let workspace_root = env::current_dir().context("获取当前工作目录失败")?;
    let app_base_dir = resolve_app_base_dir(&workspace_root, &cfg.base);
    log::init(&cfg.log, &app_base_dir)?;

    if !cfg.cron.enabled {
        log::warn("cron 已禁用，请在 config.toml 设置 [cron].enabled = true");
        return Err(anyhow::anyhow!(
            "cron 已禁用，请在 config.toml 设置 [cron].enabled = true"
        ));
    }

    log::info(format!("cron started with base dir {}", app_base_dir.display()));
    let jobs_file = resolve_jobs_file(&app_base_dir, &cfg.cron.jobs_file);
    log::info("loaded directories:");
    log::info(format!("  - base_dir: {}", app_base_dir.display()));
    log::info(format!("  - cron.jobs_file: {}", jobs_file.display()));
    log::info(format!("  - session.dir: {}", session_dir_path(&app_base_dir).display()));
    log::info(format!("  - session.db: {}", session_db_path(&app_base_dir).display()));
    if cfg.log.file_enabled {
        log::info(format!("  - log.file: {}", app_base_dir.join(&cfg.log.file_name).display()));
    } else {
        log::info("  - log.file: <disabled>");
    }
    let jobs = load_jobs_from_file(&jobs_file)?;

    let manager = Arc::new(Mutex::new(CronJobManager::from_jobs(jobs)));

    {
        let snapshot = manager.lock().await.list();
        log::info(format!(
            "[cron] started: tick={}ms jobs={} (enabled={})",
            cfg.cron.tick_ms,
            snapshot.len(),
            snapshot.iter().filter(|j| j.enabled).count()
        ));
        log::info(format!("[cron] jobs_file={}", jobs_file.display()));
        for item in snapshot {
            log::info(format!(
                "[cron] job={} session={} expr='{}' enabled={} running={}",
                item.name, item.session, item.expression, item.enabled, item.running
            ));
        }
    }

    loop {
        let due_jobs = {
            let mut locked = manager.lock().await;
            locked.collect_due_jobs(Local::now())
        };

        for job in due_jobs {
            let manager_ref = Arc::clone(&manager);
            tokio::spawn(async move {
                log::info(format!(
                    "[cron] trigger job={} session={}",
                    job.name, job.session
                ));
                let result = app::call_once_with_session(&job.prompt, Some(&job.session)).await;

                match result {
                    Ok(output) => {
                        let preview = preview_text(&output, 240);
                        log::info(format!("[cron] job={} done: {}", job.name, preview));
                    }
                    Err(err) => {
                        log::error(format!("[cron] job={} failed: {}", job.name, err));
                    }
                }

                manager_ref.lock().await.mark_finished(&job.name);
            });
        }

        tokio::time::sleep(Duration::from_millis(cfg.cron.tick_ms.max(200))).await;
    }
}

fn resolve_jobs_file(workspace_root: &Path, raw_path: &str) -> PathBuf {
    let p = PathBuf::from(raw_path);
    if p.is_absolute() {
        p
    } else {
        workspace_root.join(p)
    }
}

fn load_jobs_from_file(path: &Path) -> Result<Vec<CronJob>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取 cron jobs 文件失败: {}", path.display()))?;
    let parsed = toml::from_str::<CronJobsFile>(&content)
        .with_context(|| format!("解析 cron jobs TOML 失败: {}", path.display()))?;

    let mut jobs = Vec::new();
    for item in parsed.jobs {
        let name = item.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if item.prompt.trim().is_empty() {
            return Err(anyhow::anyhow!("cron job `{}` 的 prompt 不能为空", name));
        }

        let session = if item.session.trim().is_empty() {
            "new".to_string()
        } else {
            item.session.trim().to_string()
        };

        let schedule = CronSchedule {
            minute: parse_field(&item.minute, 0, 59, false).with_context(|| {
                format!("job `{}` 的 minute 字段非法: {}", name, item.minute)
            })?,
            hour: parse_field(&item.hour, 0, 23, false)
                .with_context(|| format!("job `{}` 的 hour 字段非法: {}", name, item.hour))?,
            day: parse_field(&item.day, 1, 31, false)
                .with_context(|| format!("job `{}` 的 day 字段非法: {}", name, item.day))?,
            month: parse_field(&item.month, 1, 12, false)
                .with_context(|| format!("job `{}` 的 month 字段非法: {}", name, item.month))?,
            weekday: parse_field(&item.weekday, 0, 7, true).with_context(|| {
                format!("job `{}` 的 weekday 字段非法: {}", name, item.weekday)
            })?,
        };

        jobs.push(CronJob {
            name,
            prompt: item.prompt,
            session,
            schedule,
            enabled: item.enabled,
        });
    }

    Ok(jobs)
}

impl CronSchedule {
    fn matches(&self, now: DateTime<Local>) -> bool {
        let minute_match = self.minute.matches(now.minute());
        let hour_match = self.hour.matches(now.hour());
        let day_match = self.day.matches(now.day());
        let month_match = self.month.matches(now.month());
        let weekday_raw = weekday_to_number(now.weekday());
        let weekday_match = self.weekday.matches(weekday_raw);

        if !(minute_match && hour_match && month_match) {
            return false;
        }

        if !self.day.is_any() && !self.weekday.is_any() {
            day_match || weekday_match
        } else {
            day_match && weekday_match
        }
    }

    fn to_expression_string(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.minute.to_expr(),
            self.hour.to_expr(),
            self.day.to_expr(),
            self.month.to_expr(),
            self.weekday.to_expr()
        )
    }
}

impl CronField {
    fn matches(&self, value: u32) -> bool {
        match self {
            CronField::Any => true,
            CronField::Values(values) => values.binary_search(&value).is_ok(),
        }
    }

    fn is_any(&self) -> bool {
        matches!(self, CronField::Any)
    }

    fn to_expr(&self) -> String {
        match self {
            CronField::Any => "*".to_string(),
            CronField::Values(values) => values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

fn parse_field(raw: &str, min: u32, max: u32, allow_weekday_name: bool) -> Result<CronField> {
    let text = raw.trim();
    if text.is_empty() || text == "*" {
        return Ok(CronField::Any);
    }

    let mut values = Vec::<u32>::new();
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
            let mut v = min;
            while v <= max {
                values.push(v);
                match v.checked_add(step) {
                    Some(next) => v = next,
                    None => break,
                }
            }
            continue;
        }

        if let Some((start_s, end_s)) = t.split_once('-') {
            let start = parse_single_value(start_s, allow_weekday_name)?;
            let end = parse_single_value(end_s, allow_weekday_name)?;
            if start > end {
                return Err(anyhow::anyhow!("范围起点不能大于终点"));
            }
            for v in start..=end {
                values.push(normalize_weekday(v));
            }
            continue;
        }

        let v = parse_single_value(t, allow_weekday_name)?;
        values.push(normalize_weekday(v));
    }

    values.retain(|v| *v >= min && *v <= max);
    values.sort_unstable();
    values.dedup();

    if values.is_empty() {
        return Err(anyhow::anyhow!("字段没有有效值"));
    }

    Ok(CronField::Values(values))
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
    match raw.trim().to_lowercase().as_str() {
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

fn weekday_to_number(wd: Weekday) -> u32 {
    match wd {
        Weekday::Sun => 0,
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
    }
}

fn default_cron_field_any() -> String {
    "*".to_string()
}

fn default_job_enabled() -> bool {
    true
}

fn preview_text(s: &str, max_chars: usize) -> String {
    let normalized = s.replace('\n', " ").trim().to_string();
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let cut = normalized.chars().take(max_chars).collect::<String>();
        format!("{}...", cut)
    }
}
