use anyhow::Result;
use serde_json::to_string;

use crate::model::ChatModel;
use crate::session::{MemoryLoaded, SkillsLoaded, ToolsLoaded};
use crate::tools::ToolManager;
use crate::types::{AssistantReply, Message, ToolCall};

const DEFAULT_STOP_MARKER: &str = "[[REACT_STOP]]";

#[derive(Debug, Clone)]
pub struct ReActOptions {
    pub stream_enabled: bool,
    pub max_loops: usize,
    pub stop_marker: String,
    pub max_message_chars: usize,
    pub window_size_chars: usize,
}

impl ReActOptions {
    pub fn normalized(self) -> Self {
        let max_loops = self.max_loops.clamp(1, 64);
        let stop_marker = if self.stop_marker.trim().is_empty() {
            DEFAULT_STOP_MARKER.to_string()
        } else {
            self.stop_marker
        };
        Self {
            stream_enabled: self.stream_enabled,
            max_loops,
            stop_marker,
            max_message_chars: self.max_message_chars,
            window_size_chars: self.window_size_chars,
        }
    }
}

impl Default for ReActOptions {
    fn default() -> Self {
        Self {
            stream_enabled: true,
            max_loops: 8,
            stop_marker: DEFAULT_STOP_MARKER.to_string(),
            max_message_chars: 0,
            window_size_chars: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ReActStopReason {
    AssistantFinished,
    ModelRequestedStop,
    MaxLoopsReached,
    Interrupted,
}

#[derive(Debug, Clone)]
pub struct ReActSummary {
    pub loops_used: usize,
    pub stop_reason: ReActStopReason,
}

#[derive(Debug, Clone)]
pub struct SessionLoadedState {
    pub skills_loaded: SkillsLoaded,
    pub memory_loaded: MemoryLoaded,
    pub tools_loaded: ToolsLoaded,
}

pub async fn run_react_loop<FStart, FToken, FTool, FToolResult>(
    client: &dyn ChatModel,
    tool_manager: &ToolManager,
    messages: &mut Vec<Message>,
    mut session_provider: impl FnMut() -> Option<SessionLoadedState>,
    session_id: Option<&str>,
    options: ReActOptions,
    mut should_interrupt: impl FnMut() -> bool,
    mut on_assistant_started: FStart,
    mut on_token: FToken,
    mut on_tool_calls_started: FTool,
    mut on_tool_results: FToolResult,
) -> Result<ReActSummary>
where
    FStart: FnMut(usize),
    FToken: FnMut(&str) + Send,
    FTool: FnMut(&[ToolCall]),
    FToolResult: FnMut(&[Message]),
{
    let options = options.normalized();
    let tools = tool_manager.definitions();

    for loop_idx in 0..options.max_loops {
        if should_interrupt() {
            return Ok(ReActSummary {
                loops_used: loop_idx,
                stop_reason: ReActStopReason::Interrupted,
            });
        }

        on_assistant_started(loop_idx + 1);

        let current_session_state = session_provider();

        let reply = request_assistant(
            client,
            messages,
            current_session_state.as_ref(),
            &tools,
            options.stream_enabled,
            options.max_message_chars,
            options.window_size_chars,
            &mut on_token,
        )
        .await?;

        if should_interrupt() {
            return Ok(ReActSummary {
                loops_used: loop_idx + 1,
                stop_reason: ReActStopReason::Interrupted,
            });
        }

        let (content, stopped_by_marker) = strip_stop_marker(reply.content, &options.stop_marker);
        let tool_calls = reply.tool_calls;

        messages.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls.clone())
            },
            tool_call_id: None,
            name: None,
        });

        if stopped_by_marker {
            return Ok(ReActSummary {
                loops_used: loop_idx + 1,
                stop_reason: ReActStopReason::ModelRequestedStop,
            });
        }

        if tool_calls.is_empty() {
            return Ok(ReActSummary {
                loops_used: loop_idx + 1,
                stop_reason: ReActStopReason::AssistantFinished,
            });
        }

        if should_interrupt() {
            return Ok(ReActSummary {
                loops_used: loop_idx + 1,
                stop_reason: ReActStopReason::Interrupted,
            });
        }

        on_tool_calls_started(&tool_calls);
        let tool_messages = tool_manager
            .run_tool_calls_in_loop(&tool_calls, Some(loop_idx + 1), session_id)
            .await?;

        if should_interrupt() {
            return Ok(ReActSummary {
                loops_used: loop_idx + 1,
                stop_reason: ReActStopReason::Interrupted,
            });
        }

        on_tool_results(&tool_messages);
        messages.extend(tool_messages);
    }

    messages.push(Message {
        role: "assistant".to_string(),
        content: Some("[ReAct] 已达到最大循环次数，已强制停止。".to_string()),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });

    Ok(ReActSummary {
        loops_used: options.max_loops,
        stop_reason: ReActStopReason::MaxLoopsReached,
    })
}

async fn request_assistant<FToken>(
    client: &dyn ChatModel,
    messages: &[Message],
    session: Option<&SessionLoadedState>,
    tools: &[crate::types::ToolDefinition],
    stream_enabled: bool,
    max_message_chars: usize,
    window_size_chars: usize,
    on_token: &mut FToken,
) -> Result<AssistantReply>
where
    FToken: FnMut(&str) + Send,
{
    let request_messages = build_request_messages(
        messages,
        session,
        max_message_chars,
        window_size_chars,
    );
    if stream_enabled {
        let mut forward_token = |token: &str| {
            on_token(token);
        };
        let streamed = client
            .stream_chat_collect(&request_messages, Some(tools), &mut forward_token)
            .await?;
        Ok(AssistantReply {
            content: streamed.content,
            tool_calls: streamed.tool_calls,
        })
    } else {
        let reply = client.chat_once(&request_messages, Some(tools)).await?;
        if let Some(text) = &reply.content {
            on_token(text);
        }
        Ok(reply)
    }
}

fn build_request_messages(
    messages: &[Message],
    session: Option<&SessionLoadedState>,
    max_message_chars: usize,
    window_size_chars: usize,
) -> Vec<Message> {
    let mut expanded = Vec::new();

    if let Some(session_state) = session {
        expanded.extend(build_session_injected_system_messages(session_state));
    }

    for message in messages {
        expanded.extend(split_message_if_needed(message, max_message_chars));
    }

    apply_window_size(expanded, window_size_chars)
}

fn build_session_injected_system_messages(session_state: &SessionLoadedState) -> Vec<Message> {
    let skills_json = to_string(&session_state.skills_loaded)
        .unwrap_or_else(|_| "{\"entries\":[]}".to_string());
    let memory_json = to_string(&session_state.memory_loaded)
        .unwrap_or_else(|_| "{\"entries\":[]}".to_string());
    let tools_json = to_string(&session_state.tools_loaded)
        .unwrap_or_else(|_| "{\"entries\":[]}".to_string());

    vec![
        Message {
            role: "system".to_string(),
            content: Some(format!("skills_loaded={}", skills_json)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: "system".to_string(),
            content: Some(format!("memory_loaded={}", memory_json)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: "system".to_string(),
            content: Some(format!("tools_loaded={}", tools_json)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ]
}

fn split_message_if_needed(message: &Message, max_message_chars: usize) -> Vec<Message> {
    if max_message_chars == 0 {
        return vec![message.clone()];
    }

    let Some(content) = &message.content else {
        return vec![message.clone()];
    };

    if content.chars().count() <= max_message_chars {
        return vec![message.clone()];
    }

    match message.role.as_str() {
        "user" | "tool" | "system" => split_message_with_same_role(message, max_message_chars),
        _ => vec![message.clone()],
    }
}

fn split_message_with_same_role(message: &Message, max_message_chars: usize) -> Vec<Message> {
    let Some(content) = &message.content else {
        return vec![message.clone()];
    };

    split_text_chunks(content, max_message_chars)
        .into_iter()
        .map(|chunk| {
            let mut m = message.clone();
            m.content = Some(chunk);
            m
        })
        .collect()
}

fn split_text_chunks(text: &str, max_message_chars: usize) -> Vec<String> {
    if max_message_chars == 0 {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for ch in text.chars() {
        if current_len >= max_message_chars {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }
        current.push(ch);
        current_len += 1;
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

fn apply_window_size(messages: Vec<Message>, window_size_chars: usize) -> Vec<Message> {
    if window_size_chars == 0 {
        return messages;
    }

    let mut keep_non_system = vec![false; messages.len()];
    let mut used = 0usize;
    let mut kept_any = false;

    for idx in (0..messages.len()).rev() {
        if messages[idx].role == "system" {
            continue;
        }

        let current_len = message_text_len(&messages[idx]);
        if used + current_len <= window_size_chars || !kept_any {
            keep_non_system[idx] = true;
            used = used.saturating_add(current_len);
            kept_any = true;
        } else {
            break;
        }
    }

    messages
        .into_iter()
        .enumerate()
        .filter_map(|(idx, message)| {
            if message.role == "system" || keep_non_system[idx] {
                Some(message)
            } else {
                None
            }
        })
        .collect()
}

fn message_text_len(message: &Message) -> usize {
    let mut total = 0usize;

    if let Some(content) = &message.content {
        total = total.saturating_add(content.chars().count());
    }

    if let Some(tool_calls) = &message.tool_calls {
        for call in tool_calls {
            total = total.saturating_add(call.function.name.chars().count());
            total = total.saturating_add(call.function.arguments.chars().count());
        }
    }

    if let Some(id) = &message.tool_call_id {
        total = total.saturating_add(id.chars().count());
    }
    if let Some(name) = &message.name {
        total = total.saturating_add(name.chars().count());
    }

    total
}

fn strip_stop_marker(content: Option<String>, stop_marker: &str) -> (Option<String>, bool) {
    let Some(raw) = content else {
        return (None, false);
    };

    if stop_marker.trim().is_empty() {
        return (Some(raw), false);
    }

    if raw.contains(stop_marker) {
        let cleaned = raw.replace(stop_marker, "").trim().to_string();
        let cleaned = if cleaned.is_empty() { None } else { Some(cleaned) };
        return (cleaned, true);
    }

    (Some(raw), false)
}
