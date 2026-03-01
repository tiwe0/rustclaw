use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{json, Value};
use std::time::Duration;

use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

const DEFAULT_TIMEOUT_SECS: u64 = 15;
const MAX_BODY_PREVIEW: usize = 4000;

pub struct HttpTool;

#[async_trait]
impl ToolPlugin for HttpTool {
    fn name(&self) -> &'static str {
        "http_request"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "发起异步 HTTP 请求，适用于查询外部 API。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "请求 URL，必须是 http 或 https" },
                        "method": { "type": "string", "description": "HTTP 方法，默认 GET" },
                        "headers": { "type": "object", "description": "请求头，键值都为字符串" },
                        "query": { "type": "object", "description": "查询参数对象" },
                        "body": { "description": "请求体，可为对象/数组/字符串" },
                        "timeout_seconds": { "type": "integer", "description": "超时秒数，默认 15" }
                    },
                    "required": ["url"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .context("http_request 缺少 url")?;

        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Ok(json!({
                "ok": false,
                "error": "仅允许 http/https URL"
            }));
        }

        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let method = Method::from_bytes(method.as_bytes()).context("无效的 HTTP method")?;

        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, 60);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("创建 HTTP 客户端失败")?;

        let mut request = client.request(method.clone(), url);

        if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(value_str) = value.as_str() {
                    request = request.header(key, value_str);
                }
            }
        }

        if let Some(query) = args.get("query").and_then(|v| v.as_object()) {
            let pairs: Vec<(String, String)> = query
                .iter()
                .map(|(k, v)| (k.clone(), json_value_to_query(v)))
                .collect();
            request = request.query(&pairs);
        }

        if let Some(body) = args.get("body") {
            match body {
                Value::String(s) => {
                    request = request.body(s.clone());
                }
                _ => {
                    request = request.json(body);
                }
            }
        }

        let response = request.send().await.context("发送 HTTP 请求失败")?;
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.context("读取响应内容失败")?;

        let body_preview = if text.len() > MAX_BODY_PREVIEW {
            format!("{}...(truncated)", &text[..MAX_BODY_PREVIEW])
        } else {
            text
        };

        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        Ok(json!({
            "ok": status.is_success(),
            "url": url,
            "method": method.as_str(),
            "status": status.as_u16(),
            "content_type": content_type,
            "body": body_preview
        }))
    }
}

fn json_value_to_query(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        _ => value.to_string(),
    }
}
