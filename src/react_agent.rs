use anyhow::Result;

use crate::model::ChatModel;
use crate::tools::ToolManager;
use crate::types::{AssistantReply, Message};

const DEFAULT_STOP_MARKER: &str = "[[REACT_STOP]]";

#[derive(Debug, Clone)]
pub struct ReActOptions {
    pub stream_enabled: bool,
    pub max_loops: usize,
    pub stop_marker: String,
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
        }
    }
}

impl Default for ReActOptions {
    fn default() -> Self {
        Self {
            stream_enabled: true,
            max_loops: 8,
            stop_marker: DEFAULT_STOP_MARKER.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ReActStopReason {
    AssistantFinished,
    ModelRequestedStop,
    MaxLoopsReached,
}

#[derive(Debug, Clone)]
pub struct ReActSummary {
    pub loops_used: usize,
    pub stop_reason: ReActStopReason,
}

pub async fn run_react_loop<FStart, FToken, FTool>(
    client: &dyn ChatModel,
    tool_manager: &ToolManager,
    messages: &mut Vec<Message>,
    options: ReActOptions,
    mut on_assistant_started: FStart,
    mut on_token: FToken,
    mut on_tool_calls_started: FTool,
) -> Result<ReActSummary>
where
    FStart: FnMut(usize),
    FToken: FnMut(&str) + Send,
    FTool: FnMut(usize),
{
    let options = options.normalized();
    let tools = tool_manager.definitions();

    for loop_idx in 0..options.max_loops {
        on_assistant_started(loop_idx + 1);

        let reply = request_assistant(
            client,
            messages,
            &tools,
            options.stream_enabled,
            &mut on_token,
        )
        .await?;

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

        on_tool_calls_started(tool_calls.len());
        let tool_messages = tool_manager.run_tool_calls(&tool_calls).await?;
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
    tools: &[crate::types::ToolDefinition],
    stream_enabled: bool,
    on_token: &mut FToken,
) -> Result<AssistantReply>
where
    FToken: FnMut(&str) + Send,
{
    if stream_enabled {
        let mut forward_token = |token: &str| {
            on_token(token);
        };
        let streamed = client
            .stream_chat_collect(messages, Some(tools), &mut forward_token)
            .await?;
        Ok(AssistantReply {
            content: streamed.content,
            tool_calls: streamed.tool_calls,
        })
    } else {
        let reply = client.chat_once(messages, Some(tools)).await?;
        if let Some(text) = &reply.content {
            on_token(text);
        }
        Ok(reply)
    }
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
