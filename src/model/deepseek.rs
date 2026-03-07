use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;

use crate::log;
use crate::model::ChatModel;
use crate::types::{
    AssistantReply, ChatRequest, Message, MessageContent, MessageDelta, StreamChunk, StreamResult, ToolCall,
    ToolCallDelta, ToolDefinition, ToolFunctionCall,
};

pub struct DeepSeekModel {
    http: Client,
    api_key: String,
    base_url: String,
    model: String,
}

struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl DeepSeekModel {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            http: Client::new(),
            api_key,
            base_url,
            model,
        }
    }
}

#[async_trait]
impl ChatModel for DeepSeekModel {
    async fn chat_once(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
    ) -> Result<AssistantReply> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            stream: false,
            tools: tools.map(|t| t.to_vec()),
        };

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .context("发送请求失败")?;

        if response.status().as_u16() != 200 {
            let status = response.status();
            let body = response.text().await.unwrap_or_else(|_| "<failed to read body>".to_string());
            log::error(format!(
                "[model/deepseek] chat_once non-200: status={} model={} base_url={} message_count={} tool_count={} body={}",
                status,
                self.model,
                self.base_url,
                messages.len(),
                tools.map(|t| t.len()).unwrap_or(0),
                preview_http_body(&body, 500)
            ));
            return Err(anyhow!("请求返回错误状态: {}", status));
        }

        let response = response
            .json::<serde_json::Value>()
            .await
            .context("解析响应失败")?;

        let message = response
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("message"))
            .cloned()
            .context("响应中没有 message")?;

        let content = parse_response_content(message.get("content"));

        let tool_calls = match message.get("tool_calls") {
            Some(raw) => serde_json::from_value::<Vec<ToolCall>>(raw.clone())
                .context("解析 tool_calls 失败")?,
            None => Vec::new(),
        };

        Ok(AssistantReply { content, tool_calls })
    }

    async fn stream_chat_collect(
        &self,
        messages: &[Message],
        tools: Option<&[ToolDefinition]>,
        on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<StreamResult> {
        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages.to_vec(),
            stream: true,
            tools: tools.map(|t| t.to_vec()),
        };

        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .context("发送请求失败")?;

        if response.status().as_u16() != 200 {
            let status = response.status();
            let body = response.text().await.unwrap_or_else(|_| "<failed to read body>".to_string());
            log::error(format!(
                "[model/deepseek] stream_chat_collect non-200: status={} model={} base_url={} message_count={} tool_count={} body={}",
                status,
                self.model,
                self.base_url,
                messages.len(),
                tools.map(|t| t.len()).unwrap_or(0),
                preview_http_body(&body, 500)
            ));
            return Err(anyhow!("请求返回错误状态: {}", status));
        }

        let mut content = String::new();
        let mut tool_builders: Vec<ToolCallBuilder> = Vec::new();
        let mut buffer = String::new();

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("读取流失败")?;
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                let line = line.trim();
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line.trim_start_matches("data:").trim();
                if data == "[DONE]" {
                    break;
                }
                let chunk: StreamChunk = serde_json::from_str(data).context("解析流式 JSON 失败")?;
                let Some(choice) = chunk.choices.first() else {
                    continue;
                };
                let Some(delta) = &choice.delta else {
                    continue;
                };
                apply_delta_content(delta, &mut content, on_token);
                if let Some(tool_calls) = &delta.tool_calls {
                    merge_tool_call_deltas(tool_calls, &mut tool_builders);
                }
            }
        }

        let tool_calls = tool_builders
            .into_iter()
            .filter(|b| !b.name.is_empty())
            .map(|b| ToolCall {
                id: if b.id.is_empty() {
                    format!("call_{}", b.name)
                } else {
                    b.id
                },
                kind: "function".to_string(),
                function: ToolFunctionCall {
                    name: b.name,
                    arguments: b.arguments,
                },
            })
            .collect();

        Ok(StreamResult {
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls,
        })
    }
}

fn parse_response_content(raw: Option<&serde_json::Value>) -> Option<String> {
    let value = raw?;
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }

    let Ok(content) = serde_json::from_value::<MessageContent>(value.clone()) else {
        return None;
    };
    Some(content.to_plain_text())
}

fn apply_delta_content(
    delta: &MessageDelta,
    content: &mut String,
    on_token: &mut (dyn for<'a> FnMut(&'a str) + Send),
) {
    if let Some(text) = &delta.content {
        content.push_str(text);
        on_token(text);
    }
}

fn merge_tool_call_deltas(deltas: &[ToolCallDelta], builders: &mut Vec<ToolCallBuilder>) {
    for call in deltas {
        let index = call.index.unwrap_or(0);
        if builders.len() <= index {
            builders.resize_with(index + 1, || ToolCallBuilder {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }
        let builder = &mut builders[index];
        if let Some(id) = &call.id {
            builder.id = id.clone();
        }
        if let Some(function) = &call.function {
            if let Some(name) = &function.name {
                builder.name = name.clone();
            }
            if let Some(arguments) = &function.arguments {
                builder.arguments.push_str(arguments);
            }
        }
    }
}

fn preview_http_body(body: &str, max_chars: usize) -> String {
    let normalized = body.replace('\n', " ").trim().to_string();
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        format!("{}...", normalized.chars().take(max_chars).collect::<String>())
    }
}
