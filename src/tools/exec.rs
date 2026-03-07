use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::tools::{ToolPlugin, truncate_utf8};
use crate::types::{ToolDefinition, ToolSchema};

const DEFAULT_TIMEOUT_SECS: u64 = 20;
const MAX_OUTPUT_LEN: usize = 8000;

pub struct ExecTool;

#[async_trait]
impl ToolPlugin for ExecTool {
    fn name(&self) -> &'static str {
        "exec_command"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            kind: "function".to_string(),
            function: ToolSchema {
                name: self.name().to_string(),
                description: "执行 shell 命令并返回标准输出与标准错误（异步执行）。".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "要执行的命令" },
                        "cwd": { "type": "string", "description": "可选工作目录" },
                        "timeout_seconds": { "type": "integer", "description": "超时秒数，默认 20" }
                    },
                    "required": ["command"]
                }),
            },
        }
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .context("exec_command 缺少 command")?;

        let timeout_secs = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, 300);

        let cwd = args
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let mut cmd = shell_command(command);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Some(dir) = cwd {
            cmd.current_dir(Path::new(dir));
        }

        let output = match timeout(Duration::from_secs(timeout_secs), cmd.output()).await {
            Ok(result) => result.context("执行命令失败")?,
            Err(_) => {
                return Ok(json!({
                    "ok": false,
                    "timed_out": true,
                    "timeout_seconds": timeout_secs,
                    "stdout": "",
                    "stderr": "command timeout"
                }));
            }
        };

        let stdout = truncate(&String::from_utf8_lossy(&output.stdout), MAX_OUTPUT_LEN);
        let stderr = truncate(&String::from_utf8_lossy(&output.stderr), MAX_OUTPUT_LEN);

        Ok(json!({
            "ok": output.status.success(),
            "timed_out": false,
            "exit_code": output.status.code(),
            "stdout": stdout,
            "stderr": stderr
        }))
    }
}

fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

fn truncate(s: &str, max: usize) -> String {
    truncate_utf8(s, max)
}
