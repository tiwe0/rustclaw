# rustclaw

最小的大模型对话框架示例（Rust），支持流式输出与 function call。当前内置 `deepseek` 与 `openai` 两种模型 provider。

## 功能
- Ratatui 图形化终端界面（消息区 / 输入区 / 状态栏）
- TUI assistant 状态显示（idle / thinking / answering / calling tools）
- 流式响应平滑输出（缓冲刷新，减少抖动）
- 流式输出（SSE）
- function call（工具调用）
- tool_calls 并行执行
- ReAct 模式循环（输出/工具调用/结果回填，直到模型停止或达到上限）
- 终端长对话（多轮上下文）
- 多会话并发对话（后台任务互不阻塞）
- 会话管理（SQLite 持久化：创建/切换/列出/清空）
- tools 插件模式
- skills 工具（可插拔后端，当前支持 markdown）
- channel 模块（可接入 Telegram 等通讯软件）
- cron 模块（定时唤醒 agent 执行预设 job）
- log 模块（等级过滤 + 控制台/文件输出）

## 依赖
- Rust 1.78+（edition 2024）
- DeepSeek API Key 或 OpenAI API Key

## 配置（TOML）

默认从 `.rustclaw/config.toml` 加载配置（可用环境变量覆盖）：

```toml
[model]
backend = "deepseek"
stream = true
name = "deepseek-chat"
api_key = "YOUR_DEEPSEEK_API_KEY"
# base_url = "https://api.deepseek.com"

[base]
base_dir = ".rustclaw"

[log]
enabled = true
level = "info"
file_enabled = true
file_name = "rustclaw.log"

[memory]
enabled = true
provider = "markdown"
base_dir = "memory"
default_key = "main"

[skills]
enabled = true
provider = "markdown"
base_dir = "skills"
default_skill = "default"

[agent]
react_max_loops = 8
react_stop_marker = "[[REACT_STOP]]"

[channel]
enabled = true
provider = "telegram"

[channel.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
# chat_id = 123456789
poll_interval_ms = 1200
long_poll_timeout_secs = 20
api_base_url = "https://api.telegram.org"

[cron]
enabled = false
tick_ms = 1000
jobs_file = "cron_jobs.toml"

[tui]
stream_flush_ms = 30
assistant_msg_color = "cyan"
user_msg_color = "green"
system_msg_color = "yellow"
```

字段说明：
- `backend`：模型后端（当前支持 `deepseek` / `openai`）
- `stream`：是否启用流式输出
- `name`：模型名称
- `api_key`：模型 key
- `base_url`：可选，覆盖后端默认地址

`[base]` 字段说明：
- `base_dir`：全局数据根目录（默认 `.rustclaw`），其余模块的 `base_dir` 均基于该目录解析

`[log]` 字段说明：
- `enabled`：是否启用日志
- `level`：最小日志级别（`debug`/`info`/`warn`/`error`）
- `file_enabled`：是否写入日志文件
- `file_name`：日志文件名（位于 `[base].base_dir` 下）

`openai` 配置示例：

```toml
[model]
backend = "openai"
stream = true
name = "gpt-4o-mini"
api_key = "YOUR_OPENAI_API_KEY"
# base_url = "https://api.openai.com"
```

`[memory]` 字段说明：
- `enabled`：是否启用 memory 工具
- `provider`：memory 存储后端类型（当前支持 `markdown`）
- `base_dir`：memory 文件存储目录（相对路径，基于 `[base].base_dir`）
- `default_key`：未指定 key 时使用的默认记忆键

`[skills]` 字段说明：
- `enabled`：是否启用 skills 工具
- `provider`：skills 存储后端类型（当前支持 `markdown`）
- `base_dir`：skills 文件存储目录（相对路径，基于 `[base].base_dir`）
- `default_skill`：未指定 skill 时使用的默认技能名

`[agent]` 字段说明：
- `react_max_loops`：ReAct 最大循环轮数，超出后强制停止
- `react_stop_marker`：模型主动停止循环的标记（默认 `[[REACT_STOP]]`）

`[channel]` 字段说明：
- `enabled`：是否启用 channel 模式
- `provider`：通讯适配器类型（当前支持 `telegram`）

`[channel.telegram]` 字段说明：
- `bot_token`：Telegram Bot Token（必填）
- `chat_id`：可选，仅允许指定会话 ID（白名单）
- `poll_interval_ms`：轮询间隔（毫秒）
- `long_poll_timeout_secs`：Telegram 长轮询超时（秒）
- `api_base_url`：Telegram API 地址（默认官方地址）

`[cron]` 字段说明：
- `enabled`：是否启用 cron 调度器
- `tick_ms`：调度检查间隔（毫秒）
- `jobs_file`：cron jobs 独立配置文件路径（TOML）
	- 相对路径会基于 `[base].base_dir` 解析（默认即 `.rustclaw/cron_jobs.toml`）

`[tui]` 字段说明：
- `stream_flush_ms`：流式输出缓冲刷新间隔（毫秒，建议 10~500；值越小越实时，越大越平滑）
- `assistant_msg_color`：assistant 消息颜色（支持基础色名或 `#RRGGBB`）
- `user_msg_color`：user 消息颜色（支持基础色名或 `#RRGGBB`）
- `system_msg_color`：system 消息颜色（支持基础色名或 `#RRGGBB`）

`cron_jobs.toml` 示例：

```toml
[[jobs]]
name = "daily_summary"
session = "new"
prompt = "请总结今天项目可能的风险点，并给出三条改进建议。"
minute = "0"
hour = "*/1"
day = "*"
month = "*"
weekday = "*"
enabled = false

[[jobs]]
name = "dependency_check"
session = "ops_agent"
prompt = "请给出当前项目依赖安全检查建议清单。"
minute = "*/30"
hour = "*"
day = "*"
month = "*"
weekday = "mon-fri"
enabled = false
```

`[[jobs]]` 字段说明：
- `name`：job 唯一名称
- `session`：job 使用的 agent 会话标识
	- `"new"`：每次新建临时 session 执行，执行后销毁（不持久化）
	- 其他值：使用该名称的持久 session；若不存在会自动创建，执行后保留
- `prompt`：定时触发时发送给 agent 的对话内容
- `minute/hour/day/month/weekday`：类 Linux cron 字段（支持 `*`、`*/n`、`a,b,c`、`a-b`，weekday 支持 `mon..sun`）
- `enabled`：是否启用该 job

## ReAct system prompt 模板

默认会根据 `react_stop_marker` 自动生成系统提示词模板。你也可以参考下面模板自定义：

```text
你是一个具备 ReAct 工作流的助手。

目标：高质量完成用户请求。

推理与行动循环：
- 当你需要外部信息或执行动作时，优先调用工具。
- 每次拿到工具结果后，继续推进任务，可再次调用工具。
- 当你已经可以给出最终答案时，直接给出结论，不再调用工具。

停止规则：
- 当你决定结束 ReAct 循环时，在最终回复末尾输出停止标记：[[REACT_STOP]]
- 输出该标记时，必须同时给出对用户可读的最终答案。

输出要求：
- 回答简洁、准确、可执行。
- 不暴露内部思维链路，只给必要结论和步骤。
- 若工具失败，说明失败原因并给出可行替代方案。
```

## 快速开始（Windows cmd）

```cmd
cargo run
```

单次调用（callOnce）：

```cmd
cargo run -- --once "帮我先查北京时间，再总结成一句话"
```

导出全部会话（JSON）：

```cmd
cargo run -- --session-export
cargo run -- --session-export D:\temp\sessions_export.json
```

Channel 模式（Telegram 测试用例）：

```cmd
cargo run -- --channel telegram
```

统一对话接口模式（推荐）：

```cmd
cargo run -- --conversation tui
cargo run -- --conversation telegram
```

Cron 模式（定时任务）：

```cmd
cargo run -- --cron
```

说明：
- 程序启动后会加载 `CronJobManager`。
- 调度器按 `tick_ms` 检查是否有到期 job。
- 到期后自动唤醒 agent，执行 `callOnce(prompt)` 对话任务（按 `session` 规则选择会话）。
- 同一个 job 默认不会并发重入（上次运行未结束前不会再次触发）。

Channel 说明：
- 运行后程序会持续轮询 Telegram Bot 消息。
- 收到文本消息后，会调用 `ReAct + tools` 的 `callOnce` 流程并自动回发到 Telegram。
- 若配置了 `chat_id`，仅该会话可触发机器人响应。

启动后输入消息即可多轮对话。

TUI 键位：
- `Enter`：发送消息或执行命令
- `Esc`：清空输入框
- `Ctrl+C`：退出程序
- `Ctrl+K`：打断当前会话的运行任务
- `PgUp/PgDn`：滚动消息区
- `↑/↓`：输入历史导航
- `F2`：打开会话列表弹窗（可用 `↑/↓` 选择，`Enter` 切换）

会话命令：
- `/help`：查看命令
- `/new [title]`：新建并切换会话
- `/list`：打开会话列表弹窗
- `/use <session_id>`：切换到指定会话
- `/history`：查看当前会话历史
- `/clear`：清空当前会话（保留 system）
- `/tasks`：列出运行中的任务
- `/interrupt`：打断当前会话的运行任务
- `/cancel [task_id|all]`：取消指定任务或全部任务
- `/exit`：退出

可选环境变量：
- `RUSTCLAW_CONFIG`：配置文件路径，默认 `.rustclaw/config.toml`

## 示例说明
- 内置工具 `get_time`，会在模型请求时被调用。
- 新增工具 `http_request`，可异步访问外部 HTTP API。
- 新增工具 `exec_command`，可异步执行 shell 命令并返回 stdout/stderr。
- 新增工具 `memory_rw`，可读写记忆（当前 markdown 存储，后续可热插拔扩展）。
- 新增工具 `skills_manage`，可 list/load/save/delete 技能片段（markdown 存储）。
- 多个 tool call 会并行执行，提升工具阶段吞吐。
- 程序会在 TUI 消息区实时刷新回答；若出现工具调用，会执行工具并继续输出最终回答（流式或非流式由配置决定）。
- ReAct 循环规则：assistant 有 tool_calls 则继续回填工具结果并发起下一轮；无 tool_calls 则自然停止。
- 若 assistant 文本包含 `[[REACT_STOP]]`（可配置），会主动停止循环。
- 若达到 `react_max_loops`，程序会强制停止本次循环。
- 可在一个会话请求尚未完成时切换到其他会话继续发起对话。
- 会话会持久化到 `[base].base_dir/.sessions/sessions.db`（SQLite）。
- 程序启动时自动加载全部 session 到内存缓存；退出时自动保存所有 agent session 上下文。
- 可通过 `--session-export` 将 SQLite 中所有 session 导出为 JSON（默认输出 `[base].base_dir/.sessions/sessions_export.json`）。

## 结构
- `src/main.rs`：程序入口（仅做模块装配）
- `src/app.rs`：对话流程编排
- `src/config.rs`：TOML 配置加载与后端地址解析
- `src/client.rs`：模型请求（流式/非流式）
- `src/session.rs`：会话持久化与管理
- `src/memory/mod.rs`：memory 后端抽象与工厂
- `src/memory/markdown.rs`：markdown memory 后端实现
- `src/tools/mod.rs`：工具插件管理器
- `src/tools/time.rs`：`get_time` 插件
- `src/tools/http.rs`：`http_request` 异步 HTTP 插件
- `src/tools/exec.rs`：`exec_command` 异步命令执行插件
- `src/tools/memory.rs`：`memory_rw` 插件
- `src/skills/mod.rs`：skills 后端抽象与工厂
- `src/skills/markdown.rs`：markdown skills 后端实现
- `src/tools/skills.rs`：`skills_manage` 插件
- `src/react_agent.rs`：ReAct 循环引擎（交互式 + callOnce 复用）
- `src/conversation/mod.rs`：统一对话接口（TUI/Telegram 并列实现）
- `src/conversation/tui.rs`：TUI 对话实现
- `src/conversation/telegram.rs`：Telegram 对话实现
- `src/channel/mod.rs`：channel 传输层模块
- `src/channel/telegram.rs`：Telegram 轮询与回发实现
- `src/cron/mod.rs`：cron 调度器与 `CronJobManager`
- `cron_jobs.toml`：cron job 独立配置文件
- `src/types.rs`：共享数据结构

## 自定义工具
1. 新建插件文件，例如 `src/tools/weather.rs`
2. 实现 `ToolPlugin` trait（`name/definition/async execute`）
3. 在 `src/tools/mod.rs` 的 `with_builtin_plugins()` 中 `register`

`http_request` 参数示例：

```json
{
	"url": "https://httpbin.org/get",
	"method": "GET",
	"query": { "q": "rust" },
	"headers": { "Accept": "application/json" },
	"timeout_seconds": 10
}
```

`exec_command` 参数示例：

```json
{
	"command": "echo hello && dir",
	"timeout_seconds": 20,
	"cwd": "D:/Projects/rustclaw"
}
```

`memory_rw` 参数示例：

```json
{
	"action": "append",
	"key": "project_notes",
	"content": "- 完成 memory 插件接入\n"
}
```

`skills_manage` 参数示例：

```json
{
	"action": "save",
	"skill": "coding_style",
	"content": "- 输出尽量简洁\n",
	"mode": "overwrite"
}
```
