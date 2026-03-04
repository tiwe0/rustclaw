use anyhow::{Context, Result};
use chrono::Local;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::config::LogConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

struct Logger {
    enabled: bool,
    min_level: LogLevel,
    file: Option<Mutex<File>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init(cfg: &LogConfig, app_base_dir: &Path) -> Result<()> {
    if LOGGER.get().is_some() {
        return Ok(());
    }

    let min_level = parse_level(&cfg.level).with_context(|| {
        format!(
            "log.level 配置非法: {}（支持 debug/info/warn/error）",
            cfg.level
        )
    })?;

    let file = if cfg.enabled && cfg.file_enabled {
        fs::create_dir_all(app_base_dir)
            .with_context(|| format!("创建日志目录失败: {}", app_base_dir.display()))?;
        let file_path = app_base_dir.join(&cfg.file_name);
        let opened = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .with_context(|| format!("打开日志文件失败: {}", file_path.display()))?;
        Some(Mutex::new(opened))
    } else {
        None
    };

    let _ = LOGGER.set(Logger {
        enabled: cfg.enabled,
        min_level,
        file,
    });

    Ok(())
}

pub fn debug(message: impl AsRef<str>) {
    write_log(LogLevel::Debug, message.as_ref());
}

pub fn info(message: impl AsRef<str>) {
    write_log(LogLevel::Info, message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write_log(LogLevel::Warn, message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write_log(LogLevel::Error, message.as_ref());
}

fn parse_level(raw: &str) -> Result<LogLevel> {
    let level = match raw.trim().to_lowercase().as_str() {
        "debug" => LogLevel::Debug,
        "info" => LogLevel::Info,
        "warn" | "warning" => LogLevel::Warn,
        "error" => LogLevel::Error,
        _ => return Err(anyhow::anyhow!("unknown log level")),
    };
    Ok(level)
}

fn write_log(level: LogLevel, message: &str) {
    let Some(logger) = LOGGER.get() else {
        return;
    };
    if !logger.enabled || level < logger.min_level {
        return;
    }

    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("[{}][{}] {}", timestamp, level.as_str(), message);

    if let Some(file_mutex) = &logger.file {
        if let Ok(mut file) = file_mutex.lock() {
            let _ = writeln!(file, "{}", line);
        }
    }
}
