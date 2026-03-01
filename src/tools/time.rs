use anyhow::Result;
use async_trait::async_trait;
use chrono::Local;
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::tools::ToolPlugin;
use crate::types::{ToolDefinition, ToolSchema};

pub struct TimeTool;

#[async_trait]
impl ToolPlugin for TimeTool {
    fn name(&self) -> &'static str {
        "get_time"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "获取当前时间，支持传入时区".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "timezone": { "type": "string", "description": "IANA 时区，例如 Asia/Shanghai" }
                    },
                    "required": []
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tz = args
            .get("timezone")
            .and_then(|v| v.as_str())
            .unwrap_or("Asia/Shanghai");
        let now = Local::now();
        let (time, tz_used) = match tz.parse::<Tz>() {
            Ok(parsed) => (
                now.with_timezone(&parsed)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
                tz.to_string(),
            ),
            Err(_) => (now.format("%Y-%m-%d %H:%M:%S").to_string(), "local".to_string()),
        };

        Ok(json!({
            "timezone": tz_used,
            "time": time
        }))
    }
}
