use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<MessageContentPart>),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageContentPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<MessageImageUrl>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageImageUrl {
    pub url: String,
}

impl MessageContent {
    pub fn text<S: Into<String>>(value: S) -> Self {
        Self::Text(value.into())
    }

    pub fn text_with_image_url<S: Into<String>, U: Into<String>>(text: S, image_url: U) -> Self {
        Self::Parts(vec![
            MessageContentPart {
                kind: "text".to_string(),
                text: Some(text.into()),
                image_url: None,
            },
            MessageContentPart {
                kind: "image_url".to_string(),
                text: None,
                image_url: Some(MessageImageUrl {
                    url: image_url.into(),
                }),
            },
        ])
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(text) => Some(text.as_str()),
            MessageContent::Parts(_) => None,
        }
    }

    pub fn is_non_empty(&self) -> bool {
        match self {
            MessageContent::Text(text) => !text.trim().is_empty(),
            MessageContent::Parts(parts) => !parts.is_empty(),
        }
    }

    pub fn to_plain_text(&self) -> String {
        match self {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    if part.kind == "text" {
                        if let Some(text) = part.text.as_ref() {
                            out.push(text.clone());
                        }
                    } else if part.kind == "image_url" {
                        out.push("[image_url]".to_string());
                    }
                }
                out.join(" ")
            }
        }
    }
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
