use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StreamChoice {
    pub delta: Option<MessageDelta>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: Option<usize>,
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub function: Option<ToolFunctionCallDelta>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolFunctionCallDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolSchema,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub struct StreamResult {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

pub struct AssistantReply {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}
