use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::interrupt;
use crate::types::{Message, MessageContent};

const SESSION_DIR_NAME: &str = ".sessions";
const SESSION_DB_NAME: &str = "sessions.db";
const META_CURRENT_SESSION_KEY: &str = "current_session_id";

#[derive(Debug, Clone)]
pub struct ChatSession {
    pub id: String,
    pub title: String,
    pub messages: Vec<Message>,
    pub skills_loaded: SkillsLoaded,
    pub memory_loaded: MemoryLoaded,
    pub tools_loaded: ToolsLoaded,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsLoaded {
    #[serde(default)]
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryLoaded {
    #[serde(default)]
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsLoaded {
    #[serde(default)]
    pub entries: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Mutex<SessionInner>>,
}

struct SessionInner {
    db_path: PathBuf,
    sessions: HashMap<String, CachedSession>,
    active_id: Option<String>,
    dirty_sessions: HashSet<String>,
    deleted_sessions: HashSet<String>,
    active_dirty: bool,
}

#[derive(Clone)]
struct CachedSession {
    session: ChatSession,
    updated_at: String,
}

#[derive(Serialize)]
struct SessionExportDoc {
    exported_at: String,
    active_session_id: Option<String>,
    sessions: Vec<SessionExportItem>,
}

#[derive(Serialize)]
struct SessionExportItem {
    id: String,
    title: String,
    updated_at: String,
    messages: Vec<Message>,
    skills_loaded: SkillsLoaded,
    memory_loaded: MemoryLoaded,
    tools_loaded: ToolsLoaded,
}

impl SessionManager {
    pub fn new(workspace_root: &Path) -> Result<Self> {
        let base_dir = workspace_root.join(SESSION_DIR_NAME);
        fs::create_dir_all(&base_dir).context("创建会话目录失败")?;
        let db_path = base_dir.join(SESSION_DB_NAME);

        let mut inner = SessionInner {
            db_path,
            sessions: HashMap::new(),
            active_id: None,
            dirty_sessions: HashSet::new(),
            deleted_sessions: HashSet::new(),
            active_dirty: false,
        };

        inner.init_schema()?;
        inner.load_all_from_db()?;

        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    pub fn load_or_create_active(&self, system_prompt: &str) -> Result<ChatSession> {
        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;

        if let Some(active_id) = locked.active_id.clone() {
            if let Some(existing) = locked.sessions.get(&active_id) {
                return Ok(existing.session.clone());
            }
        }

        let mut metas = locked.list_cached_meta();
        if let Some(meta) = metas.drain(..).next() {
            locked.active_id = Some(meta.id.clone());
            locked.active_dirty = true;
            if let Some(existing) = locked.sessions.get(&meta.id) {
                return Ok(existing.session.clone());
            }
        }

        locked.create_new_session(None, system_prompt, true)
    }

    pub fn create_session(&self, title: Option<String>, system_prompt: &str) -> Result<ChatSession> {
        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked.create_new_session(title, system_prompt, true)
    }

    pub fn create_named_session(&self, id: &str, system_prompt: &str) -> Result<ChatSession> {
        let normalized_id = normalize_session_id(id);
        if normalized_id.is_empty() {
            return Err(anyhow::anyhow!("session 名称不能为空"));
        }

        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        let session = ChatSession {
            id: normalized_id.clone(),
            title: normalized_id,
            messages: vec![Message {
                role: "system".to_string(),
                content: Some(MessageContent::text(system_prompt.to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            skills_loaded: SkillsLoaded::default(),
            memory_loaded: MemoryLoaded::default(),
            tools_loaded: ToolsLoaded::default(),
        };
        locked.upsert_cached_session(session.clone());
        Ok(session)
    }

    pub fn load_or_create_named_session(&self, id: &str, system_prompt: &str) -> Result<ChatSession> {
        let normalized_id = normalize_session_id(id);
        if normalized_id.is_empty() {
            return Err(anyhow::anyhow!("session 名称不能为空"));
        }

        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        if let Some(existing) = locked.sessions.get(&normalized_id) {
            return Ok(existing.session.clone());
        }

        let session = ChatSession {
            id: normalized_id.clone(),
            title: normalized_id,
            messages: vec![Message {
                role: "system".to_string(),
                content: Some(MessageContent::text(system_prompt.to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            skills_loaded: SkillsLoaded::default(),
            memory_loaded: MemoryLoaded::default(),
            tools_loaded: ToolsLoaded::default(),
        };
        locked.upsert_cached_session(session.clone());
        Ok(session)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let normalized_id = normalize_session_id(id);
        if normalized_id.is_empty() {
            return Ok(());
        }

        interrupt::cancel_session(&normalized_id);

        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked.sessions.remove(&normalized_id);
        locked.dirty_sessions.remove(&normalized_id);
        locked.deleted_sessions.insert(normalized_id.clone());
        if locked.active_id.as_deref() == Some(normalized_id.as_str()) {
            locked.active_id = None;
            locked.active_dirty = true;
        }
        Ok(())
    }

    pub fn load_session(&self, id: &str) -> Result<ChatSession> {
        let locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked
            .sessions
            .get(id)
            .map(|s| s.session.clone())
            .with_context(|| format!("会话不存在: {}", id))
    }

    pub fn save_session(&self, session: &ChatSession) -> Result<()> {
        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked.upsert_cached_session(session.clone());
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMeta>> {
        let locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        Ok(locked.list_cached_meta())
    }

    pub fn set_active_id(&self, id: &str) -> Result<()> {
        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked.active_id = Some(id.to_string());
        locked.active_dirty = true;
        Ok(())
    }

    pub fn active_session_id(&self) -> Result<Option<String>> {
        let locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        Ok(locked.active_id.clone())
    }

    pub fn clear_session_messages(&self, session: &mut ChatSession, system_prompt: &str) {
        session.messages.clear();
        session.messages.push(Message {
            role: "system".to_string(),
            content: Some(MessageContent::text(system_prompt.to_string())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn save_all(&self) -> Result<()> {
        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked.flush_to_db()
    }

    pub fn export_all_to_json_file(&self, output_path: &Path) -> Result<usize> {
        let mut locked = self.inner.lock().map_err(|_| anyhow::anyhow!("session lock poisoned"))?;
        locked.flush_to_db()?;

        let mut sessions = locked
            .sessions
            .values()
            .map(|cached| SessionExportItem {
                id: cached.session.id.clone(),
                title: cached.session.title.clone(),
                updated_at: cached.updated_at.clone(),
                messages: cached.session.messages.clone(),
                skills_loaded: cached.session.skills_loaded.clone(),
                memory_loaded: cached.session.memory_loaded.clone(),
                tools_loaded: cached.session.tools_loaded.clone(),
            })
            .collect::<Vec<_>>();

        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let count = sessions.len();

        let doc = SessionExportDoc {
            exported_at: Utc::now().to_rfc3339(),
            active_session_id: locked.active_id.clone(),
            sessions,
        };

        let json = serde_json::to_string_pretty(&doc).context("序列化会话导出数据失败")?;
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建导出目录失败: {}", parent.display()))?;
            }
        }
        fs::write(output_path, json)
            .with_context(|| format!("写入会话导出文件失败: {}", output_path.display()))?;

        Ok(count)
    }
}

pub fn session_dir_path(app_base_dir: &Path) -> PathBuf {
    app_base_dir.join(SESSION_DIR_NAME)
}

pub fn session_db_path(app_base_dir: &Path) -> PathBuf {
    session_dir_path(app_base_dir).join(SESSION_DB_NAME)
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            if let Ok(mut locked) = self.inner.lock() {
                let _ = locked.flush_to_db();
            }
        }
    }
}

impl SessionInner {
    fn init_schema(&self) -> Result<()> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("打开 session 数据库失败: {}", self.db_path.display()))?;

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                messages_json TEXT NOT NULL,
                skills_loaded_json TEXT NOT NULL DEFAULT '{"entries":[]}',
                memory_loaded_json TEXT NOT NULL DEFAULT '{"entries":[]}',
                tools_loaded_json TEXT NOT NULL DEFAULT '{"entries":[]}',
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .context("初始化 session 数据表失败")?;

        ensure_sessions_column(
            &conn,
            "skills_loaded_json",
            "TEXT NOT NULL DEFAULT '{\"entries\":[]}'",
        )?;
        ensure_sessions_column(
            &conn,
            "memory_loaded_json",
            "TEXT NOT NULL DEFAULT '{\"entries\":[]}'",
        )?;
        ensure_sessions_column(
            &conn,
            "tools_loaded_json",
            "TEXT NOT NULL DEFAULT '{\"entries\":[]}'",
        )?;

        Ok(())
    }

    fn load_all_from_db(&mut self) -> Result<()> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("打开 session 数据库失败: {}", self.db_path.display()))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, title, messages_json, updated_at, skills_loaded_json, memory_loaded_json, tools_loaded_json FROM sessions",
            )
            .context("查询 sessions 失败")?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let title: String = row.get(1)?;
                let messages_json: String = row.get(2)?;
                let updated_at: String = row.get(3)?;
                let skills_loaded_json: String = row.get(4)?;
                let memory_loaded_json: String = row.get(5)?;
                let tools_loaded_json: String = row.get(6)?;
                Ok((
                    id,
                    title,
                    messages_json,
                    updated_at,
                    skills_loaded_json,
                    memory_loaded_json,
                    tools_loaded_json,
                ))
            })
            .context("读取 sessions 行失败")?;

        self.sessions.clear();
        for row in rows {
            let (
                id,
                title,
                messages_json,
                updated_at,
                skills_loaded_json,
                memory_loaded_json,
                tools_loaded_json,
            ) = row.context("解析 sessions 行失败")?;
            let messages: Vec<Message> = serde_json::from_str(&messages_json)
                .with_context(|| format!("解析 session messages 失败: {}", id))?;
            let skills_loaded: SkillsLoaded =
                parse_loaded_json(&skills_loaded_json, &id, "skills_loaded_json")?;
            let memory_loaded: MemoryLoaded =
                parse_loaded_json(&memory_loaded_json, &id, "memory_loaded_json")?;
            let tools_loaded: ToolsLoaded =
                parse_loaded_json(&tools_loaded_json, &id, "tools_loaded_json")?;
            self.sessions.insert(
                id.clone(),
                CachedSession {
                    session: ChatSession {
                        id,
                        title,
                        messages,
                        skills_loaded,
                        memory_loaded,
                        tools_loaded,
                    },
                    updated_at,
                },
            );
        }

        self.active_id = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![META_CURRENT_SESSION_KEY],
                |row| row.get::<_, String>(0),
            )
            .ok();

        self.dirty_sessions.clear();
        self.deleted_sessions.clear();
        self.active_dirty = false;

        Ok(())
    }

    fn create_new_session(
        &mut self,
        title: Option<String>,
        system_prompt: &str,
        set_active: bool,
    ) -> Result<ChatSession> {
        let id = Utc::now().format("%Y%m%d%H%M%S").to_string();
        let title = title.unwrap_or_else(|| format!("session-{}", &id));
        let session = ChatSession {
            id: id.clone(),
            title,
            messages: vec![Message {
                role: "system".to_string(),
                content: Some(MessageContent::text(system_prompt.to_string())),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            skills_loaded: SkillsLoaded::default(),
            memory_loaded: MemoryLoaded::default(),
            tools_loaded: ToolsLoaded::default(),
        };

        self.upsert_cached_session(session.clone());
        if set_active {
            self.active_id = Some(id);
            self.active_dirty = true;
        }

        Ok(session)
    }

    fn upsert_cached_session(&mut self, session: ChatSession) {
        let updated_at = Utc::now().to_rfc3339();
        let id = session.id.clone();
        self.sessions.insert(
            id.clone(),
            CachedSession {
                session,
                updated_at,
            },
        );
        self.dirty_sessions.insert(id.clone());
        self.deleted_sessions.remove(&id);
    }

    fn list_cached_meta(&self) -> Vec<SessionMeta> {
        let mut metas = self
            .sessions
            .values()
            .map(|cached| SessionMeta {
                id: cached.session.id.clone(),
                title: cached.session.title.clone(),
                updated_at: cached.updated_at.clone(),
            })
            .collect::<Vec<_>>();

        metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        metas
    }

    fn flush_to_db(&mut self) -> Result<()> {
        if self.dirty_sessions.is_empty() && self.deleted_sessions.is_empty() && !self.active_dirty {
            return Ok(());
        }

        let mut conn = Connection::open(&self.db_path)
            .with_context(|| format!("打开 session 数据库失败: {}", self.db_path.display()))?;
        let tx = conn.transaction().context("开启 session 事务失败")?;

        for id in self.deleted_sessions.clone() {
            tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])
                .context("删除 session 失败")?;
        }

        for id in self.dirty_sessions.clone() {
            if let Some(cached) = self.sessions.get(&id) {
                let messages_json = serde_json::to_string(&cached.session.messages)
                    .with_context(|| format!("序列化 session messages 失败: {}", id))?;
                let skills_loaded_json = serde_json::to_string(&cached.session.skills_loaded)
                    .with_context(|| format!("序列化 skills_loaded 失败: {}", id))?;
                let memory_loaded_json = serde_json::to_string(&cached.session.memory_loaded)
                    .with_context(|| format!("序列化 memory_loaded 失败: {}", id))?;
                let tools_loaded_json = serde_json::to_string(&cached.session.tools_loaded)
                    .with_context(|| format!("序列化 tools_loaded 失败: {}", id))?;
                tx.execute(
                    "
                    INSERT INTO sessions(
                        id,
                        title,
                        messages_json,
                        skills_loaded_json,
                        memory_loaded_json,
                        tools_loaded_json,
                        updated_at
                    )
                    VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ON CONFLICT(id) DO UPDATE SET
                        title = excluded.title,
                        messages_json = excluded.messages_json,
                        skills_loaded_json = excluded.skills_loaded_json,
                        memory_loaded_json = excluded.memory_loaded_json,
                        tools_loaded_json = excluded.tools_loaded_json,
                        updated_at = excluded.updated_at
                    ",
                    params![
                        cached.session.id,
                        cached.session.title,
                        messages_json,
                        skills_loaded_json,
                        memory_loaded_json,
                        tools_loaded_json,
                        cached.updated_at
                    ],
                )
                .with_context(|| format!("写入 session 失败: {}", id))?;
            }
        }

        if self.active_dirty {
            if let Some(active) = &self.active_id {
                tx.execute(
                    "
                    INSERT INTO meta(key, value)
                    VALUES(?1, ?2)
                    ON CONFLICT(key) DO UPDATE SET value = excluded.value
                    ",
                    params![META_CURRENT_SESSION_KEY, active],
                )
                .context("写入 active session 失败")?;
            } else {
                tx.execute(
                    "DELETE FROM meta WHERE key = ?1",
                    params![META_CURRENT_SESSION_KEY],
                )
                .context("删除 active session 失败")?;
            }
        }

        tx.commit().context("提交 session 事务失败")?;
        self.dirty_sessions.clear();
        self.deleted_sessions.clear();
        self.active_dirty = false;
        Ok(())
    }
}

fn normalize_session_id(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

fn ensure_sessions_column(conn: &Connection, column: &str, definition: &str) -> Result<()> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(sessions)")
        .context("读取 sessions 表结构失败")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .context("解析 sessions 表结构失败")?;

    let mut exists = false;
    for row in rows {
        if row.context("读取 sessions 字段失败")? == column {
            exists = true;
            break;
        }
    }

    if !exists {
        conn.execute(
            &format!("ALTER TABLE sessions ADD COLUMN {} {}", column, definition),
            [],
        )
        .with_context(|| format!("为 sessions 添加字段失败: {}", column))?;
    }

    Ok(())
}

fn parse_loaded_json<T>(raw: &str, session_id: &str, field: &str) -> Result<T>
where
    T: DeserializeOwned + Default,
{
    if raw.trim().is_empty() {
        return Ok(T::default());
    }
    serde_json::from_str(raw)
        .with_context(|| format!("解析 session {} 字段失败: {}", session_id, field))
}
