use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use std::collections::{HashMap, HashSet};
use std::env;
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

use crate::config::{load_config, resolve_app_base_dir, resolve_config_path};
use crate::log;
use crate::memory::create_memory_backend;
use crate::model::{ChatModel, create_model_provider};
use crate::react_agent::{run_react_loop, ReActOptions, ReActStopReason, SessionLoadedState};
use crate::session::{ChatSession, SessionManager, SessionMeta, session_db_path, session_dir_path};
use crate::skills::create_skills_backend;
use crate::tools::ToolManager;
use crate::types::{Message, ToolCall};

const MAX_LINES: usize = 1000;
const SCROLL_STEP: usize = 8;
const UI_POLL_MS: u64 = 16;
const STREAM_FORCE_FLUSH_BYTES: usize = 96;
const TOOL_ARGS_PREVIEW_CHARS: usize = 180;

fn resolve_child_dir_for_log(app_base_dir: &std::path::Path, raw: &str) -> String {
    let path = std::path::PathBuf::from(raw);
    if path.is_absolute() {
        format!("<invalid-absolute:{}>", path.display())
    } else {
        app_base_dir.join(path).display().to_string()
    }
}

fn log_loaded_dirs(
    app_base_dir: &std::path::Path,
    memory_base_dir_raw: &str,
    skills_base_dir_raw: &str,
    log_file_enabled: bool,
    log_file_name: &str,
) {
    let memory_dir = resolve_child_dir_for_log(app_base_dir, memory_base_dir_raw);
    let skills_dir = resolve_child_dir_for_log(app_base_dir, skills_base_dir_raw);
    let session_dir = session_dir_path(app_base_dir);
    let session_db = session_db_path(app_base_dir);
    let log_file = if log_file_enabled {
        app_base_dir.join(log_file_name).display().to_string()
    } else {
        "<disabled>".to_string()
    };

    log::info("loaded directories:");
    log::info(format!("  - base_dir: {}", app_base_dir.display()));
    log::info(format!("  - memory.base_dir: {}", memory_dir));
    log::info(format!("  - skills.base_dir: {}", skills_dir));
    log::info(format!("  - session.dir: {}", session_dir.display()));
    log::info(format!("  - session.db: {}", session_db.display()));
    log::info(format!("  - log.file: {}", log_file));
}

fn build_react_system_prompt(stop_marker: &str) -> String {
    format!(
        "你是一个具备 ReAct 工作流的助手。

目标：高质量完成用户请求。

推理与行动循环：
- 当你需要外部信息或执行动作时，优先调用工具。
- 每次拿到工具结果后，继续推进任务，可再次调用工具。
- 当你已经可以给出最终答案时，直接给出结论，不再调用工具。

停止规则：
- 当你决定结束 ReAct 循环时，在最终回复末尾输出停止标记：{marker}
- 输出该标记时，必须同时给出对用户可读的最终答案。

输出要求：
- 回答简洁、准确、可执行。
- 不暴露内部思维链路，只给必要结论和步骤。
- 若工具失败，说明失败原因并给出可行替代方案。

web_browser 工具使用规则（必须遵守）：
- 该工具支持 action：open / close / sessions / goto / scroll / html / content / click / input / screenshot。
- 优先采用会话模式：先 open 获取 session_id；后续动作尽量复用 session_id，不要每一步都重新 open。
- 若未显式传 session_id，工具会默认复用当前会话；可通过 sessions 查看可用会话。
- 需要页面跳转时优先使用 goto，而不是在其他动作里反复传不同 url。
- 任务结束后主动调用 close 释放浏览器资源。
- close 必须提供 session_id；click/input 通常需要 selector；input 需要 text。
- 若返回 ok=false 的工具结果，先阅读 error 再修正参数或更换策略，不要重复同一错误调用。",
        marker = stop_marker
    )
}

fn session_loaded_state_from_session(session: &ChatSession) -> SessionLoadedState {
    SessionLoadedState {
        skills_loaded: session.skills_loaded.clone(),
        memory_loaded: session.memory_loaded.clone(),
        tools_loaded: session.tools_loaded.clone(),
    }
}

pub async fn run() -> Result<()> {
    let config_path = resolve_config_path();
    let config = load_config(&config_path)?;
    let model_config = config.model;
    let memory_config = config.memory;
    let skills_config = config.skills;
    let agent_config = config.agent;
    let tui_config = config.tui;
    let system_prompt = build_react_system_prompt(&agent_config.react_stop_marker);
    let client = create_model_provider(&model_config)?;
    let workspace_root = env::current_dir().context("获取当前工作目录失败")?;
    let app_base_dir = resolve_app_base_dir(&workspace_root, &config.base);
    log::init(&config.log, &app_base_dir)?;
    log::info(format!("app started with session store at {}", app_base_dir.display()));
    log::debug(format!("model backend: {}", model_config.backend));
    log_loaded_dirs(
        &app_base_dir,
        &memory_config.base_dir,
        &skills_config.base_dir,
        config.log.file_enabled,
        &config.log.file_name,
    );
    let memory_backend = create_memory_backend(&memory_config, &app_base_dir).await?;
    let skills_backend = create_skills_backend(&skills_config, &app_base_dir).await?;
    let session_manager = SessionManager::new(&app_base_dir)?;
    let tool_manager = Arc::new(ToolManager::with_builtin_plugins(
        session_manager.clone(),
        memory_backend,
        memory_config.default_key,
        skills_backend,
        skills_config.default_skill,
    ));

    let assistant_msg_color = parse_tui_color(&tui_config.assistant_msg_color, Color::Cyan);
    let user_msg_color = parse_tui_color(&tui_config.user_msg_color, Color::Green);
    let system_msg_color = parse_tui_color(&tui_config.system_msg_color, Color::Yellow);

    let active_session = session_manager.load_or_create_active(&system_prompt)?;
    let shared_session = Arc::new(Mutex::new(active_session));

    let mut terminal = init_terminal()?;
    let tool_manager_for_shutdown = tool_manager.clone();
    let result = run_tui_loop(
        &mut terminal,
        client,
        tool_manager,
        session_manager.clone(),
        shared_session,
        model_config.stream,
        model_config.max_token,
        model_config.window_size,
        agent_config.react_max_loops,
        agent_config.react_stop_marker,
        tui_config.stream_flush_ms,
        assistant_msg_color,
        user_msg_color,
        system_msg_color,
        system_prompt,
    )
    .await;
    tool_manager_for_shutdown.shutdown();
    let _ = session_manager.save_all();
    restore_terminal(&mut terminal)?;
    result
}

pub async fn call_once(user_input: &str) -> Result<String> {
    call_once_with_session(user_input, None).await
}

pub async fn call_once_with_session(user_input: &str, session: Option<&str>) -> Result<String> {
    let config_path = resolve_config_path();
    let config = load_config(&config_path)?;
    let model_config = config.model;
    let memory_config = config.memory;
    let skills_config = config.skills;
    let agent_config = config.agent;
    let system_prompt = build_react_system_prompt(&agent_config.react_stop_marker);
    let client = create_model_provider(&model_config)?;
    let workspace_root = env::current_dir().context("获取当前工作目录失败")?;
    let app_base_dir = resolve_app_base_dir(&workspace_root, &config.base);
    log::init(&config.log, &app_base_dir)?;
    log_loaded_dirs(
        &app_base_dir,
        &memory_config.base_dir,
        &skills_config.base_dir,
        config.log.file_enabled,
        &config.log.file_name,
    );
    let session_manager = SessionManager::new(&app_base_dir)?;
    let memory_backend = create_memory_backend(&memory_config, &app_base_dir).await?;
    let skills_backend = create_skills_backend(&skills_config, &app_base_dir).await?;
    let tool_manager = ToolManager::with_builtin_plugins(
        session_manager.clone(),
        memory_backend,
        memory_config.default_key,
        skills_backend,
        skills_config.default_skill,
    );

    let session_key = session
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let mut transient_session_id: Option<String> = None;

    let mut working_messages = if let Some(ref key) = session_key {
        if key.eq_ignore_ascii_case("new") {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_else(|_| Duration::from_secs(0))
                .as_millis();
            let sid = format!("cron_new_{}", ts);
            transient_session_id = Some(sid.clone());
            let session_obj = session_manager.create_named_session(&sid, &system_prompt)?;
            session_obj.messages
        } else {
            let session_obj = session_manager.load_or_create_named_session(key, &system_prompt)?;
            session_obj.messages
        }
    } else {
        vec![Message {
            role: "system".to_string(),
            content: Some(system_prompt.clone()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }]
    };

    working_messages.push(Message {
        role: "user".to_string(),
        content: Some(user_input.to_string()),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });

    let summary_result = run_react_loop(
        client.as_ref(),
        &tool_manager,
        &mut working_messages,
        || {
            session_key
                .as_ref()
                .and_then(|sid| session_manager.load_session(sid).ok())
                .map(|s| session_loaded_state_from_session(&s))
        },
        session_key.as_deref(),
        ReActOptions {
            stream_enabled: model_config.stream,
            max_loops: agent_config.react_max_loops,
            stop_marker: agent_config.react_stop_marker,
            max_message_chars: model_config.max_token,
            window_size_chars: model_config.window_size,
        },
        |_| {},
        |_| {},
        |_| {},
    )
    .await;
    tool_manager.shutdown();
    let summary = summary_result?;

    if let Some(ref key) = session_key {
        if !key.eq_ignore_ascii_case("new") {
            let mut session_obj = session_manager.load_or_create_named_session(key, &system_prompt)?;
            session_obj.messages = working_messages.clone();
            session_manager.save_session(&session_obj)?;
        }
    }
    if let Some(sid) = transient_session_id {
        session_manager.delete_session(&sid)?;
    }
    session_manager.save_all()?;

    let final_content = working_messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.content.as_ref().map(|s| !s.is_empty()).unwrap_or(false))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

    let mut output = final_content;
    if matches!(summary.stop_reason, ReActStopReason::MaxLoopsReached) {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!(
            "[ReAct] 达到最大循环次数 {}，已强制停止。",
            summary.loops_used
        ));
    }

    Ok(output)
}

async fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: Arc<dyn ChatModel>,
    tool_manager: Arc<ToolManager>,
    session_manager: SessionManager,
    session: Arc<Mutex<ChatSession>>,
    stream_enabled: bool,
    max_message_chars: usize,
    window_size_chars: usize,
    react_max_loops: usize,
    react_stop_marker: String,
    stream_flush_ms: u64,
    assistant_msg_color: Color,
    user_msg_color: Color,
    system_msg_color: Color,
    system_prompt: String,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<BackendEvent>();
    let mut running_tasks: HashMap<u64, JoinHandle<()>> = HashMap::new();
    let locked = session.lock().await;
    let mut app = TuiApp::from_session(
        &locked,
        stream_enabled,
        stream_flush_ms,
        assistant_msg_color,
        user_msg_color,
        system_msg_color,
    );
    drop(locked);

    loop {
        let visible_chat_lines = {
            let (width, height) = crossterm::terminal::size()?;
            let area = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(area);
            let lines = layout[0].height.saturating_sub(2) as usize;
            let cols = layout[0].width.saturating_sub(2) as usize;
            (lines, cols)
        };
        app.set_chat_viewport(visible_chat_lines.0, visible_chat_lines.1);

        while let Ok(event) = rx.try_recv() {
            match &event {
                BackendEvent::TurnFinished { task_id, .. }
                | BackendEvent::Error { task_id, .. }
                | BackendEvent::TaskCanceled { task_id, .. } => {
                    running_tasks.remove(task_id);
                }
                _ => {}
            }
            app.handle_backend_event(event);
        }

        app.flush_stream_buffers(false);

        terminal.draw(|frame| draw_ui(frame, &app))?;

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(UI_POLL_MS))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.show_session_popup {
                    handle_popup_key(key.code, &session_manager, &session, &mut app).await?;
                    continue;
                }

                if app.show_loaded_popup {
                    handle_loaded_popup_key(key.code, &mut app);
                    continue;
                }

                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        abort_all_tasks(&mut app, &mut running_tasks, "用户退出中断");
                        app.should_quit = true;
                    }
                    KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        interrupt_current_session(&mut app, &mut running_tasks, "手动打断");
                    }
                    KeyCode::F(2) => {
                        open_session_popup(&session_manager, &session, &mut app).await?;
                    }
                    KeyCode::F(3) => {
                        let locked = session.lock().await;
                        app.open_loaded_popup(&locked);
                    }
                    KeyCode::PageUp => {
                        app.scroll_up(SCROLL_STEP);
                    }
                    KeyCode::PageDown => {
                        app.page_down(SCROLL_STEP);
                    }
                    KeyCode::End => {
                        app.enable_auto_scroll();
                    }
                    KeyCode::Up => {
                        app.history_prev();
                    }
                    KeyCode::Down => {
                        app.history_next();
                    }
                    KeyCode::Char(ch) => {
                        app.input.push(ch);
                        app.reset_history_nav_on_edit();
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                        app.reset_history_nav_on_edit();
                    }
                    KeyCode::Enter => {
                        let line = app.input.trim().to_string();
                        app.input.clear();
                        app.reset_history_nav_on_send();
                        if line.is_empty() {
                            continue;
                        }

                        app.remember_history(line.clone());

                        if line.starts_with('/') {
                            execute_command(
                                &line,
                                &session_manager,
                                &session,
                                &mut app,
                                &mut running_tasks,
                                tx.clone(),
                                &system_prompt,
                            )
                            .await?;
                        } else {
                            app.enable_auto_scroll();
                            let (session_id, session_title) = {
                                let mut locked = session.lock().await;
                                if let Ok(latest) = session_manager.load_session(&locked.id) {
                                    *locked = latest;
                                }
                                locked.messages.push(Message {
                                    role: "user".to_string(),
                                    content: Some(line.clone()),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    name: None,
                                });
                                session_manager.save_session(&locked)?;
                                (locked.id.clone(), locked.title.clone())
                            };

                            if session_id == app.session_id {
                                app.push_line(format!("user> {}", line));
                            } else {
                                app.push_line(format!(
                                    "system> 后台会话 {} ({}) 发送: {}",
                                    session_id, session_title, line
                                ));
                            }

                            let task_id = app.next_task_id();
                            app.mark_task_started(&session_id, task_id);
                            let handle = spawn_turn_task(
                                task_id,
                                session_id,
                                client.clone(),
                                tool_manager.clone(),
                                session_manager.clone(),
                                stream_enabled,
                                max_message_chars,
                                window_size_chars,
                                react_max_loops,
                                react_stop_marker.clone(),
                                tx.clone(),
                            );
                            running_tasks.insert(task_id, handle);
                        }
                    }
                    KeyCode::Esc => {
                        abort_all_tasks(&mut app, &mut running_tasks, "Esc退出中断");
                        app.should_quit = true;
                    }
                    _ => {}
                }
            }
        }
    }

    abort_all_tasks(&mut app, &mut running_tasks, "程序结束");

    Ok(())
}

async fn handle_popup_key(
    key: KeyCode,
    session_manager: &SessionManager,
    session: &Arc<Mutex<ChatSession>>,
    app: &mut TuiApp,
) -> Result<()> {
    match key {
        KeyCode::Esc => {
            app.close_session_popup();
        }
        KeyCode::Up => {
            app.popup_prev();
        }
        KeyCode::Down => {
            app.popup_next();
        }
        KeyCode::Enter => {
            if let Some(selected) = app.selected_session().cloned() {
                let loaded = session_manager.load_session(&selected.id)?;
                session_manager.set_active_id(&loaded.id)?;
                {
                    let mut locked = session.lock().await;
                    *locked = loaded;
                }
                let locked = session.lock().await;
                app.reset_from_session(&locked);
                app.push_line(format!("system> 已切换会话: {} ({})", locked.id, locked.title));
                app.close_session_popup();
            }
        }
        _ => {}
    }

    Ok(())
}

async fn open_session_popup(
    session_manager: &SessionManager,
    session: &Arc<Mutex<ChatSession>>,
    app: &mut TuiApp,
) -> Result<()> {
    let current_id = { session.lock().await.id.clone() };
    let sessions = session_manager.list_sessions()?;
    if sessions.is_empty() {
        app.push_line("system> 暂无会话".to_string());
        return Ok(());
    }
    app.open_session_popup(sessions, &current_id);
    Ok(())
}

fn handle_loaded_popup_key(key: KeyCode, app: &mut TuiApp) {
    match key {
        KeyCode::Esc | KeyCode::Enter | KeyCode::F(3) => app.close_loaded_popup(),
        _ => {}
    }
}

async fn execute_command(
    line: &str,
    session_manager: &SessionManager,
    session: &Arc<Mutex<ChatSession>>,
    app: &mut TuiApp,
    running_tasks: &mut HashMap<u64, JoinHandle<()>>,
    tx: mpsc::UnboundedSender<BackendEvent>,
    system_prompt: &str,
) -> Result<()> {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or_default();
    let arg = parts.next().map(str::trim).unwrap_or("");

    let format_loaded = |entries: &[String]| {
        if entries.is_empty() {
            "(none)".to_string()
        } else {
            entries.join(", ")
        }
    };

    match cmd {
        "/help" => {
            app.push_line("system> 命令: /help /new [title] /list /use <id> /history /loaded /clear /tasks /interrupt /cancel [task_id|all] /exit".to_string());
            app.push_line("system> 键位: PgUp/PgDn滚动, ↑/↓输入历史, F2会话弹窗, F3查看loaded, Ctrl+K打断当前会话".to_string());
        }
        "/new" => {
            let title = if arg.is_empty() {
                None
            } else {
                Some(arg.to_string())
            };
            let new_session = session_manager.create_session(title, system_prompt)?;
            {
                let mut locked = session.lock().await;
                *locked = new_session;
            }
            let locked = session.lock().await;
            app.reset_from_session(&locked);
            app.push_line(format!("system> 已切换会话: {} ({})", locked.id, locked.title));
        }
        "/list" => {
            open_session_popup(session_manager, session, app).await?;
        }
        "/use" => {
            if arg.is_empty() {
                app.push_line("system> 用法: /use <session_id>".to_string());
            } else {
                let loaded = session_manager.load_session(arg)?;
                session_manager.set_active_id(&loaded.id)?;
                {
                    let mut locked = session.lock().await;
                    *locked = loaded;
                }
                let locked = session.lock().await;
                app.reset_from_session(&locked);
                app.push_line(format!("system> 已切换会话: {} ({})", locked.id, locked.title));
            }
        }
        "/history" => {
            let locked = session.lock().await;
            app.push_line(format!("system> history: {} ({})", locked.id, locked.title));
            for message in &locked.messages {
                if message.role == "system" {
                    continue;
                }
                let content = message.content.as_deref().unwrap_or("");
                app.push_line(format!("{}> {}", message.role, content));
            }
        }
        "/loaded" | "/resources" => {
            let locked = session.lock().await;
            app.push_line(format!(
                "system> loaded @ {} ({})",
                locked.id, locked.title
            ));
            app.push_line(format!(
                "system> memory_loaded: {}",
                format_loaded(&locked.memory_loaded.entries)
            ));
            app.push_line(format!(
                "system> skills_loaded: {}",
                format_loaded(&locked.skills_loaded.entries)
            ));
            app.push_line(format!(
                "system> tools_loaded: {}",
                format_loaded(&locked.tools_loaded.entries)
            ));
        }
        "/clear" => {
            {
                let mut locked = session.lock().await;
                session_manager.clear_session_messages(&mut locked, system_prompt);
                session_manager.save_session(&locked)?;
                app.reset_from_session(&locked);
            }
            app.push_line("system> 当前会话已清空（保留 system）".to_string());
        }
        "/tasks" => {
            let mut task_ids = app.all_task_ids();
            task_ids.sort_unstable();
            if task_ids.is_empty() {
                app.push_line("system> 当前无运行中任务".to_string());
            } else {
                for task_id in task_ids {
                    let session_id = app
                        .active_task_sessions
                        .get(&task_id)
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    app.push_line(format!("system> task {} @ session {}", task_id, session_id));
                }
            }
        }
        "/interrupt" => {
            interrupt_current_session(app, running_tasks, "命令打断");
        }
        "/cancel" => {
            if arg.is_empty() {
                interrupt_current_session(app, running_tasks, "命令取消");
            } else if arg.eq_ignore_ascii_case("all") {
                abort_all_tasks(app, running_tasks, "命令取消全部任务");
            } else if let Ok(task_id) = arg.parse::<u64>() {
                cancel_task_by_id(app, running_tasks, task_id, "命令取消任务");
            } else {
                app.push_line("system> 用法: /cancel [task_id|all]".to_string());
            }
        }
        "/exit" | "/quit" => {
            abort_all_tasks(app, running_tasks, "用户退出中断");
            let _ = tx.send(BackendEvent::QuitRequested);
        }
        _ => app.push_line("system> 未知命令，输入 /help 查看可用命令".to_string()),
    }

    Ok(())
}

fn spawn_turn_task(
    task_id: u64,
    session_id: String,
    client: Arc<dyn ChatModel>,
    tool_manager: Arc<ToolManager>,
    session_manager: SessionManager,
    stream_enabled: bool,
    max_message_chars: usize,
    window_size_chars: usize,
    react_max_loops: usize,
    react_stop_marker: String,
    tx: mpsc::UnboundedSender<BackendEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = process_user_turn_async(
            task_id,
            &session_id,
            client.as_ref(),
            &tool_manager,
            &session_manager,
            stream_enabled,
            max_message_chars,
            window_size_chars,
            react_max_loops,
            react_stop_marker,
            tx.clone(),
        )
        .await;

        if let Err(err) = result {
            log::error(format!("turn task failed: session={} task={} err={}", session_id, task_id, err));
            let _ = tx.send(BackendEvent::Error {
                session_id: session_id.clone(),
                task_id,
                error: format!("请求失败: {err}"),
            });
        }
    })
}

async fn process_user_turn_async(
    task_id: u64,
    session_id: &str,
    client: &dyn ChatModel,
    tool_manager: &ToolManager,
    session_manager: &SessionManager,
    stream_enabled: bool,
    max_message_chars: usize,
    window_size_chars: usize,
    react_max_loops: usize,
    react_stop_marker: String,
    tx: mpsc::UnboundedSender<BackendEvent>,
) -> Result<()> {
    let mut working_session = session_manager.load_session(session_id)?;
    let summary = run_react_loop(
        client,
        tool_manager,
        &mut working_session.messages,
        || {
            session_manager
                .load_session(session_id)
                .ok()
                .map(|s| session_loaded_state_from_session(&s))
        },
        Some(session_id),
        ReActOptions {
            stream_enabled,
            max_loops: react_max_loops,
            stop_marker: react_stop_marker,
            max_message_chars,
            window_size_chars,
        },
        |loop_idx| {
            let _ = tx.send(if loop_idx == 1 {
                BackendEvent::AssistantStarted {
                    session_id: session_id.to_string(),
                    task_id,
                }
            } else {
                BackendEvent::AssistantAfterToolStarted {
                    session_id: session_id.to_string(),
                    task_id,
                }
            });
        },
        |token| {
            let _ = tx.send(BackendEvent::Token {
                session_id: session_id.to_string(),
                task_id,
                token: token.to_string(),
            });
        },
        |tool_calls| {
            let tool_details = tool_calls
                .iter()
                .map(|call| format_tool_call_for_tui(call, TOOL_ARGS_PREVIEW_CHARS))
                .collect::<Vec<_>>();
            let _ = tx.send(BackendEvent::CallingTools {
                session_id: session_id.to_string(),
                task_id,
                tool_count: tool_calls.len(),
                tool_details,
            });
        },
    )
    .await?;

    match summary.stop_reason {
        ReActStopReason::ModelRequestedStop => {
            let _ = tx.send(BackendEvent::Info {
                session_id: session_id.to_string(),
                message: format!(
                    "ReAct 已收到模型停止信号，循环结束（{} 轮）",
                    summary.loops_used
                ),
            });
        }
        ReActStopReason::MaxLoopsReached => {
            let _ = tx.send(BackendEvent::Info {
                session_id: session_id.to_string(),
                message: format!(
                    "ReAct 达到最大循环次数 {}，已强制停止",
                    summary.loops_used
                ),
            });
        }
        ReActStopReason::AssistantFinished => {}
    }

    let mut latest = session_manager.load_session(session_id)?;
    latest.messages = working_session.messages;
    session_manager.save_session(&latest)?;
    let _ = tx.send(BackendEvent::SessionChanged {
        id: latest.id.clone(),
        title: latest.title.clone(),
    });
    let _ = tx.send(BackendEvent::TurnFinished {
        session_id: session_id.to_string(),
        task_id,
    });
    Ok(())
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[derive(Debug)]
enum BackendEvent {
    AssistantStarted { session_id: String, task_id: u64 },
    AssistantAfterToolStarted { session_id: String, task_id: u64 },
    CallingTools {
        session_id: String,
        task_id: u64,
        tool_count: usize,
        tool_details: Vec<String>,
    },
    Token { session_id: String, task_id: u64, token: String },
    SessionChanged { id: String, title: String },
    TurnFinished { session_id: String, task_id: u64 },
    Error { session_id: String, task_id: u64, error: String },
    TaskCanceled {
        session_id: String,
        task_id: u64,
        reason: String,
    },
    Info { session_id: String, message: String },
    QuitRequested,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssistantPhase {
    Idle,
    Thinking,
    Answering,
    CallingTools,
}

struct TuiApp {
    input: String,
    lines: Vec<String>,
    session_id: String,
    session_title: String,
    stream_enabled: bool,
    should_quit: bool,
    auto_scroll_enabled: bool,
    chat_scroll: usize,
    input_history: Vec<String>,
    history_cursor: Option<usize>,
    history_draft: String,
    show_session_popup: bool,
    show_loaded_popup: bool,
    popup_sessions: Vec<SessionMeta>,
    popup_selected: usize,
    loaded_popup_lines: Vec<String>,
    next_task_id: u64,
    active_task_lines: HashMap<u64, usize>,
    active_task_sessions: HashMap<u64, String>,
    pending_by_session: HashMap<String, usize>,
    assistant_phase_by_session: HashMap<String, AssistantPhase>,
    stream_token_buffers: HashMap<u64, String>,
    stream_tasks_dirty: HashSet<u64>,
    last_stream_flush: Instant,
    stream_flush_interval: Duration,
    chat_view_lines: usize,
    chat_view_cols: usize,
    assistant_msg_color: Color,
    user_msg_color: Color,
    system_msg_color: Color,
}

impl TuiApp {
    fn from_session(
        session: &ChatSession,
        stream_enabled: bool,
        stream_flush_ms: u64,
        assistant_msg_color: Color,
        user_msg_color: Color,
        system_msg_color: Color,
    ) -> Self {
        let flush_ms = stream_flush_ms.clamp(10, 500);
        let mut app = Self {
            input: String::new(),
            lines: Vec::new(),
            session_id: session.id.clone(),
            session_title: session.title.clone(),
            stream_enabled,
            should_quit: false,
            auto_scroll_enabled: true,
            chat_scroll: 0,
            chat_view_lines: 1,
            chat_view_cols: 80,
            input_history: Vec::new(),
            history_cursor: None,
            history_draft: String::new(),
            show_session_popup: false,
            show_loaded_popup: false,
            popup_sessions: Vec::new(),
            popup_selected: 0,
            loaded_popup_lines: Vec::new(),
            next_task_id: 1,
            active_task_lines: HashMap::new(),
            active_task_sessions: HashMap::new(),
            pending_by_session: HashMap::new(),
            assistant_phase_by_session: HashMap::new(),
            stream_token_buffers: HashMap::new(),
            stream_tasks_dirty: HashSet::new(),
            last_stream_flush: Instant::now(),
            stream_flush_interval: Duration::from_millis(flush_ms),
            assistant_msg_color,
            user_msg_color,
            system_msg_color,
        };
        app.rebuild_lines_from_messages(session);
        app.push_line("system> 输入 /help 查看命令，Esc/Ctrl+C 退出".to_string());
        app
    }

    fn rebuild_lines_from_messages(&mut self, session: &ChatSession) {
        self.lines.clear();
        self.session_id = session.id.clone();
        self.session_title = session.title.clone();
        for message in &session.messages {
            if message.role == "system" {
                continue;
            }
            let content = message.content.as_deref().unwrap_or("");
            self.lines.push(format!("{}> {}", message.role, content));
        }
        self.chat_scroll = 0;
    }

    fn reset_from_session(&mut self, session: &ChatSession) {
        self.rebuild_lines_from_messages(session);
        self.active_task_lines.clear();
        self.stream_token_buffers.clear();
        self.stream_tasks_dirty.clear();
        self.auto_scroll_enabled = true;
    }

    fn next_task_id(&mut self) -> u64 {
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.saturating_add(1);
        id
    }

    fn mark_task_started(&mut self, session_id: &str, task_id: u64) {
        *self
            .pending_by_session
            .entry(session_id.to_string())
            .or_insert(0) += 1;
        self.active_task_sessions.insert(task_id, session_id.to_string());
    }

    fn mark_task_finished(&mut self, session_id: &str, task_id: u64) {
        if let Some(count) = self.pending_by_session.get_mut(session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.pending_by_session.remove(session_id);
            }
        }
        self.active_task_lines.remove(&task_id);
        self.active_task_sessions.remove(&task_id);
        self.stream_token_buffers.remove(&task_id);
        self.stream_tasks_dirty.remove(&task_id);
        if !self.pending_by_session.contains_key(session_id) {
            self.assistant_phase_by_session
                .insert(session_id.to_string(), AssistantPhase::Idle);
        }
    }

    fn pending_total(&self) -> usize {
        self.pending_by_session.values().sum()
    }

    fn push_line(&mut self, line: String) {
        let prev_max_scroll = self.max_scroll_offset();
        self.lines.push(line);
        if self.lines.len() > MAX_LINES {
            let overflow = self.lines.len() - MAX_LINES;
            self.lines.drain(0..overflow);

            for line_idx in self.active_task_lines.values_mut() {
                *line_idx = line_idx.saturating_sub(overflow);
            }

            let invalid_task_ids = self
                .active_task_lines
                .iter()
                .filter_map(|(task_id, line_idx)| {
                    if *line_idx >= self.lines.len() {
                        Some(*task_id)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            for task_id in invalid_task_ids {
                self.active_task_lines.remove(&task_id);
                self.stream_token_buffers.remove(&task_id);
                self.stream_tasks_dirty.remove(&task_id);
            }
        }

        let next_max_scroll = self.max_scroll_offset();
        if self.chat_scroll > 0 {
            self.chat_scroll = self
                .chat_scroll
                .saturating_add(next_max_scroll.saturating_sub(prev_max_scroll))
                .min(next_max_scroll);
        }
        if self.auto_scroll_enabled {
            self.chat_scroll = 0;
        } else {
            self.chat_scroll = self.chat_scroll.min(next_max_scroll);
        }
    }

    fn max_scroll_offset(&self) -> usize {
        self.visual_line_count()
            .saturating_sub(self.chat_view_lines.max(1))
    }

    fn visual_line_count(&self) -> usize {
        self.visual_lines().len()
    }

    fn visual_lines(&self) -> Vec<String> {
        let max_cols = self.chat_view_cols.max(8);
        let mut out = Vec::new();

        for line in &self.lines {
            let mut current = String::new();
            let mut col_count = 0usize;

            for ch in line.chars() {
                if ch == '\n' {
                    out.push(current);
                    current = String::new();
                    col_count = 0;
                    continue;
                }

                if col_count >= max_cols {
                    out.push(current);
                    current = String::new();
                    col_count = 0;
                }

                current.push(ch);
                col_count += 1;
            }

            out.push(current);
        }

        out
    }

    fn set_chat_viewport(&mut self, lines: usize, cols: usize) {
        self.chat_view_lines = lines.max(1);
        self.chat_view_cols = cols.max(8);
        self.chat_scroll = self.chat_scroll.min(self.max_scroll_offset());
        if self.auto_scroll_enabled {
            self.chat_scroll = 0;
        }
    }

    fn append_chunk_to_task_line(&mut self, task_id: u64, chunk: &str) {
        let Some(mut line_idx) = self.active_task_lines.get(&task_id).copied() else {
            return;
        };

        let max_cols = self.chat_view_cols.max(8);
        for ch in chunk.chars() {
            let need_new_line = if ch == '\n' {
                true
            } else {
                self.lines
                    .get(line_idx)
                    .map(|line| line.chars().count() >= max_cols)
                    .unwrap_or(true)
            };

            if need_new_line {
                self.push_line(String::new());
                line_idx = self.lines.len().saturating_sub(1);
                if ch == '\n' {
                    continue;
                }
            }

            if let Some(line) = self.lines.get_mut(line_idx) {
                line.push(ch);
            }
        }

        self.active_task_lines.insert(task_id, line_idx);
    }

    fn scroll_up(&mut self, step: usize) {
        self.chat_scroll = self
            .chat_scroll
            .saturating_add(step)
            .min(self.max_scroll_offset());
        if self.chat_scroll > 0 {
            self.auto_scroll_enabled = false;
        }
    }

    fn scroll_down(&mut self, step: usize) {
        self.chat_scroll = self.chat_scroll.saturating_sub(step);
        if self.chat_scroll == 0 {
            self.auto_scroll_enabled = true;
        }
    }

    fn page_down(&mut self, step: usize) {
        if self.chat_scroll <= step {
            self.enable_auto_scroll();
        } else {
            self.scroll_down(step);
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.chat_scroll = 0;
    }

    fn is_at_bottom(&self) -> bool {
        self.chat_scroll == 0
    }

    fn enable_auto_scroll(&mut self) {
        self.auto_scroll_enabled = true;
        self.scroll_to_bottom();
    }

    fn maybe_auto_scroll_on_reply(&mut self) {
        if self.is_at_bottom() {
            self.auto_scroll_enabled = true;
        }
        if self.auto_scroll_enabled {
            self.scroll_to_bottom();
        }
    }

    fn remember_history(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        let should_push = self
            .input_history
            .last()
            .map(|last| last != &line)
            .unwrap_or(true);
        if should_push {
            self.input_history.push(line);
        }
    }

    fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }

        match self.history_cursor {
            None => {
                self.history_draft = self.input.clone();
                let idx = self.input_history.len() - 1;
                self.history_cursor = Some(idx);
                self.input = self.input_history[idx].clone();
            }
            Some(0) => {}
            Some(idx) => {
                let new_idx = idx - 1;
                self.history_cursor = Some(new_idx);
                self.input = self.input_history[new_idx].clone();
            }
        }
    }

    fn history_next(&mut self) {
        let Some(idx) = self.history_cursor else {
            return;
        };

        if idx + 1 < self.input_history.len() {
            let new_idx = idx + 1;
            self.history_cursor = Some(new_idx);
            self.input = self.input_history[new_idx].clone();
        } else {
            self.history_cursor = None;
            self.input = self.history_draft.clone();
            self.history_draft.clear();
        }
    }

    fn reset_history_nav_on_edit(&mut self) {
        if self.history_cursor.is_some() {
            self.history_cursor = None;
            self.history_draft.clear();
        }
    }

    fn reset_history_nav_on_send(&mut self) {
        self.history_cursor = None;
        self.history_draft.clear();
    }

    fn open_session_popup(&mut self, sessions: Vec<SessionMeta>, current_id: &str) {
        self.popup_selected = sessions
            .iter()
            .position(|s| s.id == current_id)
            .unwrap_or(0);
        self.popup_sessions = sessions;
        self.show_session_popup = true;
    }

    fn close_session_popup(&mut self) {
        self.show_session_popup = false;
        self.popup_sessions.clear();
        self.popup_selected = 0;
    }

    fn open_loaded_popup(&mut self, session: &ChatSession) {
        self.show_loaded_popup = true;
        self.loaded_popup_lines.clear();
        self.loaded_popup_lines.push(format!("session: {} ({})", session.id, session.title));
        self.loaded_popup_lines
            .push(format!("memory_loaded: {}", join_loaded_entries(&session.memory_loaded.entries)));
        self.loaded_popup_lines
            .push(format!("skills_loaded: {}", join_loaded_entries(&session.skills_loaded.entries)));
        self.loaded_popup_lines
            .push(format!("tools_loaded: {}", join_loaded_entries(&session.tools_loaded.entries)));
    }

    fn close_loaded_popup(&mut self) {
        self.show_loaded_popup = false;
        self.loaded_popup_lines.clear();
    }

    fn popup_prev(&mut self) {
        if self.popup_sessions.is_empty() {
            return;
        }
        self.popup_selected = self.popup_selected.saturating_sub(1);
    }

    fn popup_next(&mut self) {
        if self.popup_sessions.is_empty() {
            return;
        }
        let last = self.popup_sessions.len() - 1;
        self.popup_selected = (self.popup_selected + 1).min(last);
    }

    fn selected_session(&self) -> Option<&SessionMeta> {
        self.popup_sessions.get(self.popup_selected)
    }

    fn task_ids_for_session(&self, session_id: &str) -> Vec<u64> {
        self.active_task_sessions
            .iter()
            .filter_map(|(task_id, sid)| if sid == session_id { Some(*task_id) } else { None })
            .collect()
    }

    fn all_task_ids(&self) -> Vec<u64> {
        self.active_task_sessions.keys().copied().collect()
    }

    fn set_assistant_phase(&mut self, session_id: &str, phase: AssistantPhase) {
        self.assistant_phase_by_session
            .insert(session_id.to_string(), phase);
    }

    fn current_assistant_phase(&self) -> AssistantPhase {
        self.assistant_phase_by_session
            .get(&self.session_id)
            .copied()
            .unwrap_or(AssistantPhase::Idle)
    }

    fn flush_stream_buffers(&mut self, force: bool) {
        if self.stream_tasks_dirty.is_empty() {
            return;
        }

        if !force {
            let pending_bytes = self
                .stream_tasks_dirty
                .iter()
                .filter_map(|task_id| self.stream_token_buffers.get(task_id))
                .map(|buf| buf.len())
                .sum::<usize>();

            let reached_interval = self.last_stream_flush.elapsed() >= self.stream_flush_interval;
            let reached_pending_limit = pending_bytes >= STREAM_FORCE_FLUSH_BYTES;

            if !reached_interval && !reached_pending_limit {
                return;
            }
        }

        let task_ids = self.stream_tasks_dirty.iter().copied().collect::<Vec<_>>();
        for task_id in task_ids {
            self.flush_stream_buffer_for_task(task_id);
        }
        self.last_stream_flush = Instant::now();
    }

    fn flush_stream_buffer_for_task(&mut self, task_id: u64) {
        let Some(buffer) = self.stream_token_buffers.get_mut(&task_id) else {
            self.stream_tasks_dirty.remove(&task_id);
            return;
        };
        if buffer.is_empty() {
            self.stream_tasks_dirty.remove(&task_id);
            return;
        }

        let chunk = std::mem::take(buffer);
        self.stream_tasks_dirty.remove(&task_id);

        if self.active_task_lines.contains_key(&task_id) {
            self.maybe_auto_scroll_on_reply();
            self.append_chunk_to_task_line(task_id, &chunk);
        }
    }

    fn handle_backend_event(&mut self, event: BackendEvent) {
        match event {
            BackendEvent::AssistantStarted { session_id, task_id } => {
                self.set_assistant_phase(&session_id, AssistantPhase::Thinking);
                if self.session_id == session_id {
                    self.maybe_auto_scroll_on_reply();
                    self.push_line("assistant>".to_string());
                    let idx = self.lines.len().saturating_sub(1);
                    self.active_task_lines.insert(task_id, idx);
                }
            }
            BackendEvent::AssistantAfterToolStarted { session_id, task_id } => {
                self.set_assistant_phase(&session_id, AssistantPhase::Thinking);
                if self.session_id == session_id {
                    self.maybe_auto_scroll_on_reply();
                    self.push_line("assistant(after_tool)>".to_string());
                    let idx = self.lines.len().saturating_sub(1);
                    self.active_task_lines.insert(task_id, idx);
                }
            }
            BackendEvent::CallingTools {
                session_id,
                task_id,
                tool_count,
                tool_details,
            } => {
                self.flush_stream_buffer_for_task(task_id);
                self.set_assistant_phase(&session_id, AssistantPhase::CallingTools);
                if self.session_id == session_id {
                    self.push_line(format!("system> 调用工具中（{}）...", tool_count));
                    for detail in tool_details {
                        self.push_line(format!("system> tool {}", detail));
                    }
                }
            }
            BackendEvent::Token {
                session_id,
                task_id,
                token,
            } => {
                self.set_assistant_phase(&session_id, AssistantPhase::Answering);
                if self.session_id == session_id {
                    self.maybe_auto_scroll_on_reply();
                    let flush_immediately = token.contains('\n') || token.len() >= 24;
                    self.stream_token_buffers
                        .entry(task_id)
                        .or_default()
                        .push_str(&token);
                    self.stream_tasks_dirty.insert(task_id);
                    if flush_immediately {
                        self.flush_stream_buffers(true);
                    }
                }
            }
            BackendEvent::SessionChanged { id, title } => {
                if self.session_id == id {
                    self.session_title = title;
                }
            }
            BackendEvent::TurnFinished { session_id, task_id } => {
                self.flush_stream_buffer_for_task(task_id);
                self.mark_task_finished(&session_id, task_id);
            }
            BackendEvent::Error {
                session_id,
                task_id,
                error,
            } => {
                self.flush_stream_buffer_for_task(task_id);
                if self.session_id == session_id {
                    self.push_line(format!("error> {}", error));
                } else {
                    self.push_line(format!("system> 会话 {} 出错: {}", session_id, error));
                }
                self.mark_task_finished(&session_id, task_id);
            }
            BackendEvent::TaskCanceled {
                session_id,
                task_id,
                reason,
            } => {
                self.flush_stream_buffer_for_task(task_id);
                if self.session_id == session_id {
                    self.push_line(format!("system> 任务 {} 已取消: {}", task_id, reason));
                } else {
                    self.push_line(format!(
                        "system> 会话 {} 的任务 {} 已取消: {}",
                        session_id, task_id, reason
                    ));
                }
                self.mark_task_finished(&session_id, task_id);
            }
            BackendEvent::Info { session_id, message } => {
                if self.session_id == session_id {
                    self.push_line(format!("system> {}", message));
                } else {
                    self.push_line(format!("system> 会话 {}: {}", session_id, message));
                }
            }
            BackendEvent::QuitRequested => {
                self.should_quit = true;
            }
        }
    }
}

fn parse_tui_color(raw: &str, fallback: Color) -> Color {
    let text = raw.trim();
    if text.is_empty() {
        return fallback;
    }

    let lower = text.to_lowercase();
    match lower.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => {
            if let Some(hex) = lower.strip_prefix('#') {
                if hex.len() == 6 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&hex[0..2], 16),
                        u8::from_str_radix(&hex[2..4], 16),
                        u8::from_str_radix(&hex[4..6], 16),
                    ) {
                        return Color::Rgb(r, g, b);
                    }
                }
            }
            fallback
        }
    }
}

fn message_role_color(line: &str, app: &TuiApp, previous: Color) -> Color {
    if line.starts_with("assistant>") || line.starts_with("assistant(after_tool)>") {
        app.assistant_msg_color
    } else if line.starts_with("user>") {
        app.user_msg_color
    } else if line.starts_with("system>") {
        app.system_msg_color
    } else {
        previous
    }
}

fn format_tool_call_for_tui(call: &ToolCall, args_limit: usize) -> String {
    let tool_name = if call.function.name.trim().is_empty() {
        "<unknown>"
    } else {
        call.function.name.as_str()
    };
    let compact_args = call
        .function
        .arguments
        .replace('\n', " ")
        .replace('\r', " ");
    let args_preview = truncate_with_ellipsis(&compact_args, args_limit);
    format!("{} args={}", tool_name, args_preview)
}

fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return "...".to_string();
    }

    let mut out = String::new();
    let mut count = 0usize;
    for ch in input.chars() {
        if count >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn cancel_task_by_id(
    app: &mut TuiApp,
    running_tasks: &mut HashMap<u64, JoinHandle<()>>,
    task_id: u64,
    reason: &str,
) {
    if let Some(handle) = running_tasks.remove(&task_id) {
        handle.abort();
        let session_id = app
            .active_task_sessions
            .get(&task_id)
            .cloned()
            .unwrap_or_else(|| app.session_id.clone());
        app.handle_backend_event(BackendEvent::TaskCanceled {
            session_id,
            task_id,
            reason: reason.to_string(),
        });
    } else {
        app.push_line(format!("system> 未找到运行中的任务 {}", task_id));
    }
}

fn interrupt_current_session(
    app: &mut TuiApp,
    running_tasks: &mut HashMap<u64, JoinHandle<()>>,
    reason: &str,
) {
    let mut task_ids = app.task_ids_for_session(&app.session_id);
    task_ids.sort_unstable();
    if task_ids.is_empty() {
        app.push_line("system> 当前会话无可打断任务".to_string());
        return;
    }
    for task_id in task_ids {
        if let Some(handle) = running_tasks.remove(&task_id) {
            handle.abort();
            app.handle_backend_event(BackendEvent::TaskCanceled {
                session_id: app.session_id.clone(),
                task_id,
                reason: reason.to_string(),
            });
        }
    }
}

fn abort_all_tasks(
    app: &mut TuiApp,
    running_tasks: &mut HashMap<u64, JoinHandle<()>>,
    reason: &str,
) {
    let mut task_ids = app.all_task_ids();
    task_ids.sort_unstable();
    for task_id in task_ids {
        if let Some(handle) = running_tasks.remove(&task_id) {
            handle.abort();
            let session_id = app
                .active_task_sessions
                .get(&task_id)
                .cloned()
                .unwrap_or_else(|| app.session_id.clone());
            app.handle_backend_event(BackendEvent::TaskCanceled {
                session_id,
                task_id,
                reason: reason.to_string(),
            });
        }
    }
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, app: &TuiApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let max_lines = chunks[0].height.saturating_sub(2) as usize;
    let visual_lines = app.visual_lines();
    let total = visual_lines.len();
    let scroll = app.chat_scroll.min(app.max_scroll_offset());
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(max_lines);
    let mut role_color = app.system_msg_color;
    let visible = visual_lines[start..end]
        .iter()
        .map(|line| {
            role_color = message_role_color(line, app, role_color);
            Line::from(Span::styled(
                line.clone(),
                Style::default().fg(role_color),
            ))
        })
        .collect::<Vec<Line>>();

    let chat_block = Block::default()
        .title(format!(
            "Chat [{}] {}{}",
            app.session_id,
            app.session_title,
            if app.chat_scroll > 0 { " (滚动)" } else { "" }
        ))
        .borders(Borders::ALL);
    let chat = Paragraph::new(visible).block(chat_block).wrap(Wrap { trim: false });
    frame.render_widget(chat, chunks[0]);

    let input_block = Block::default()
        .title("Input (Enter发送, Esc退出, ↑/↓历史)")
        .borders(Borders::ALL);
    let input = Paragraph::new(app.input.as_str()).block(input_block);
    frame.render_widget(input, chunks[1]);

    let pending = app.pending_total();
    let phase = app.current_assistant_phase();
    let (phase_label, status_style) = match phase {
        AssistantPhase::Idle => ("idle", Style::default().fg(Color::Green)),
        AssistantPhase::Thinking => ("thinking", Style::default().fg(Color::Yellow)),
        AssistantPhase::Answering => ("answering", Style::default().fg(Color::Cyan)),
        AssistantPhase::CallingTools => ("calling tools", Style::default().fg(Color::Magenta)),
    };
    let status_text = format!("状态: {} | pending={} ", phase_label, pending);
    let mode = if app.stream_enabled { "stream" } else { "non-stream" };
    let status = Paragraph::new(Line::from(vec![
        Span::styled(status_text, status_style),
        Span::raw(format!(" | 模式: {} | PgUp/PgDn滚动 | F2会话 | F3资源", mode)),
    ]));
    frame.render_widget(status, chunks[2]);

    if app.show_session_popup {
        draw_session_popup(frame, app);
    }

    if app.show_loaded_popup {
        draw_loaded_popup(frame, app);
    }
}

fn draw_session_popup(frame: &mut ratatui::Frame<'_>, app: &TuiApp) {
    let area = centered_rect(70, 70, frame.area());
    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = app
        .popup_sessions
        .iter()
        .map(|s| ListItem::new(format!("{} | {} | {}", s.id, s.title, s.updated_at)))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title("会话列表（↑/↓选择，Enter切换，Esc关闭）")
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));

    let mut state = ListState::default();
    state.select(Some(app.popup_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_loaded_popup(frame: &mut ratatui::Frame<'_>, app: &TuiApp) {
    let area = centered_rect(75, 45, frame.area());
    frame.render_widget(Clear, area);

    let lines = app
        .loaded_popup_lines
        .iter()
        .cloned()
        .map(Line::from)
        .collect::<Vec<_>>();

    let panel = Paragraph::new(lines)
        .block(
            Block::default()
                .title("已加载资源（Esc/F3/Enter关闭）")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(panel, area);
}

fn join_loaded_entries(entries: &[String]) -> String {
    if entries.is_empty() {
        "(none)".to_string()
    } else {
        entries.join(", ")
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);

    horizontal[1]
}
