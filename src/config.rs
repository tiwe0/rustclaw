use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com";
pub const DEFAULT_MODEL: &str = "deepseek-chat";
pub const DEFAULT_CONFIG_PATH: &str = "~/.rustclaw/config.toml";
const DEFAULT_CONFIG_DIR_NAME: &str = ".rustclaw";
const DEFAULT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_CONFIG_TEMPLATE: &str = include_str!("../example.config.toml");

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub model: ModelConfig,
    #[serde(default)]
    pub base: BaseConfig,
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub channel: ChannelConfig,
    #[serde(default)]
    pub cron: CronConfig,
    #[serde(default)]
    pub tui: TuiConfig,
}

#[derive(Debug, Deserialize)]
pub struct ModelConfig {
    pub backend: String,
    pub stream: bool,
    pub name: String,
    pub api_key: String,
    pub base_url: Option<String>,
    #[serde(default = "default_model_max_token", alias = "maxToken")]
    pub max_token: usize,
    #[serde(default = "default_model_window_size", alias = "windowSize")]
    pub window_size: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BaseConfig {
    #[serde(default = "default_base_base_dir")]
    pub base_dir: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LogConfig {
    #[serde(default = "default_log_enabled")]
    pub enabled: bool,
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_file_enabled")]
    pub file_enabled: bool,
    #[serde(default = "default_log_file_name")]
    pub file_name: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_enabled")]
    pub enabled: bool,
    #[serde(default = "default_memory_provider")]
    pub provider: String,
    #[serde(default = "default_memory_base_dir")]
    pub base_dir: String,
    #[serde(default = "default_memory_default_key")]
    pub default_key: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SkillsConfig {
    #[serde(default = "default_skills_enabled")]
    pub enabled: bool,
    #[serde(default = "default_skills_provider")]
    pub provider: String,
    #[serde(default = "default_skills_base_dir")]
    pub base_dir: String,
    #[serde(default = "default_skills_default_skill")]
    pub default_skill: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    #[serde(default = "default_react_max_loops")]
    pub react_max_loops: usize,
    #[serde(default = "default_react_stop_marker")]
    pub react_stop_marker: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChannelConfig {
    #[serde(default = "default_channel_enabled")]
    pub enabled: bool,
    #[serde(default = "default_channel_provider")]
    pub provider: String,
    #[serde(default)]
    pub telegram: TelegramChannelConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TelegramChannelConfig {
    #[serde(default = "default_telegram_bot_token")]
    pub bot_token: String,
    #[serde(default)]
    pub chat_id: Option<i64>,
    #[serde(default = "default_telegram_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_telegram_long_poll_timeout_secs")]
    pub long_poll_timeout_secs: u64,
    #[serde(default = "default_telegram_api_base_url")]
    pub api_base_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CronConfig {
    #[serde(default = "default_cron_enabled")]
    pub enabled: bool,
    #[serde(default = "default_cron_tick_ms")]
    pub tick_ms: u64,
    #[serde(default = "default_cron_jobs_file")]
    pub jobs_file: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TuiConfig {
    #[serde(default = "default_tui_stream_flush_ms")]
    pub stream_flush_ms: u64,
    #[serde(default = "default_tui_assistant_msg_color")]
    pub assistant_msg_color: String,
    #[serde(default = "default_tui_user_msg_color")]
    pub user_msg_color: String,
    #[serde(default = "default_tui_system_msg_color")]
    pub system_msg_color: String,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: default_memory_enabled(),
            provider: default_memory_provider(),
            base_dir: default_memory_base_dir(),
            default_key: default_memory_default_key(),
        }
    }
}

impl Default for BaseConfig {
    fn default() -> Self {
        Self {
            base_dir: default_base_base_dir(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: default_log_enabled(),
            level: default_log_level(),
            file_enabled: default_log_file_enabled(),
            file_name: default_log_file_name(),
        }
    }
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: default_skills_enabled(),
            provider: default_skills_provider(),
            base_dir: default_skills_base_dir(),
            default_skill: default_skills_default_skill(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            react_max_loops: default_react_max_loops(),
            react_stop_marker: default_react_stop_marker(),
        }
    }
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            enabled: default_channel_enabled(),
            provider: default_channel_provider(),
            telegram: TelegramChannelConfig::default(),
        }
    }
}

impl Default for TelegramChannelConfig {
    fn default() -> Self {
        Self {
            bot_token: default_telegram_bot_token(),
            chat_id: None,
            poll_interval_ms: default_telegram_poll_interval_ms(),
            long_poll_timeout_secs: default_telegram_long_poll_timeout_secs(),
            api_base_url: default_telegram_api_base_url(),
        }
    }
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            enabled: default_cron_enabled(),
            tick_ms: default_cron_tick_ms(),
            jobs_file: default_cron_jobs_file(),
        }
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            stream_flush_ms: default_tui_stream_flush_ms(),
            assistant_msg_color: default_tui_assistant_msg_color(),
            user_msg_color: default_tui_user_msg_color(),
            system_msg_color: default_tui_system_msg_color(),
        }
    }
}

fn default_memory_enabled() -> bool {
    false
}

fn default_model_max_token() -> usize {
    0
}

fn default_model_window_size() -> usize {
    0
}

fn default_base_base_dir() -> String {
    "~/.rustclaw".to_string()
}

fn default_log_enabled() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_file_enabled() -> bool {
    true
}

fn default_log_file_name() -> String {
    "rustclaw.log".to_string()
}

fn default_memory_provider() -> String {
    "markdown".to_string()
}

fn default_memory_base_dir() -> String {
    ".memory".to_string()
}

fn default_memory_default_key() -> String {
    "main".to_string()
}

fn default_skills_enabled() -> bool {
    false
}

fn default_skills_provider() -> String {
    "markdown".to_string()
}

fn default_skills_base_dir() -> String {
    ".skills".to_string()
}

fn default_skills_default_skill() -> String {
    "default".to_string()
}

fn default_react_max_loops() -> usize {
    8
}

fn default_react_stop_marker() -> String {
    "[[REACT_STOP]]".to_string()
}

fn default_channel_enabled() -> bool {
    false
}

fn default_channel_provider() -> String {
    "telegram".to_string()
}

fn default_telegram_bot_token() -> String {
    "".to_string()
}

fn default_telegram_poll_interval_ms() -> u64 {
    1200
}

fn default_telegram_long_poll_timeout_secs() -> u64 {
    20
}

fn default_telegram_api_base_url() -> String {
    "https://api.telegram.org".to_string()
}

fn default_cron_enabled() -> bool {
    false
}

fn default_cron_tick_ms() -> u64 {
    1000
}

fn default_cron_jobs_file() -> String {
    "cron_jobs.toml".to_string()
}

fn default_tui_stream_flush_ms() -> u64 {
    30
}

fn default_tui_assistant_msg_color() -> String {
    "cyan".to_string()
}

fn default_tui_user_msg_color() -> String {
    "green".to_string()
}

fn default_tui_system_msg_color() -> String {
    "yellow".to_string()
}

fn default_base_url_for_backend(backend: &str) -> Option<&'static str> {
    match backend {
        "deepseek" => Some(DEFAULT_BASE_URL),
        "openai" => Some(OPENAI_DEFAULT_BASE_URL),
        _ => None,
    }
}

pub fn resolve_base_url(config: &ModelConfig) -> Result<String> {
    if let Some(url) = &config.base_url {
        return Ok(url.trim_end_matches('/').to_string());
    }
    default_base_url_for_backend(&config.backend)
        .map(|s| s.to_string())
        .with_context(|| {
            format!(
                "未知 backend `{}`，请在 config.toml 设置 model.base_url",
                config.backend
            )
        })
}

pub fn resolve_app_base_dir(workspace_root: &Path, base: &BaseConfig) -> PathBuf {
    let path = expand_tilde_path(&base.base_dir);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

pub fn load_config(path: &str) -> Result<AppConfig> {
    let resolved_path = expand_tilde_path(path);
    let content = fs::read_to_string(&resolved_path)
        .with_context(|| format!("读取配置文件失败: {}", resolved_path.display()))?;
    let mut cfg = toml::from_str::<AppConfig>(&content).context("解析 TOML 配置失败")?;
    normalize_legacy_base_dir(&mut cfg.base);
    Ok(cfg)
}

pub fn resolve_config_path() -> String {
    if let Err(err) = ensure_default_user_config() {
        eprintln!("[config] 初始化默认配置失败: {err}");
    }

    env::var("RUSTCLAW_CONFIG").unwrap_or_else(|_| {
        default_user_config_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string())
    })
}

fn ensure_default_user_config() -> Result<()> {
    let Some(config_dir) = default_user_config_dir() else {
        return Ok(());
    };
    let config_path = config_dir.join(DEFAULT_CONFIG_FILE_NAME);

    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)
            .with_context(|| format!("创建配置目录失败: {}", config_dir.display()))?;
    }

    if !config_path.exists() {
        fs::write(&config_path, DEFAULT_CONFIG_TEMPLATE)
            .with_context(|| format!("写入默认配置失败: {}", config_path.display()))?;
    }

    Ok(())
}

fn default_user_config_path() -> Option<PathBuf> {
    default_user_config_dir().map(|dir| dir.join(DEFAULT_CONFIG_FILE_NAME))
}

fn default_user_config_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(DEFAULT_CONFIG_DIR_NAME))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

fn expand_tilde_path(path: &str) -> PathBuf {
    if let Some(suffix) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(suffix);
        }
    }
    PathBuf::from(path)
}

fn normalize_legacy_base_dir(base: &mut BaseConfig) {
    let normalized = base.base_dir.trim();
    if normalized == ".rustclaw" || normalized == "./.rustclaw" {
        base.base_dir = format!("~/{DEFAULT_CONFIG_DIR_NAME}");
    }
}
