use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::openai_compat::{Tool, ToolCall, ToolFunction};
use crate::terminal_session::{TerminalCreateRequest, TerminalManager};
use crate::tools::{
    edit_file, read_file, write_file, BashArgs, BashTool, EditFileArgs, ReadFileArgs,
    WriteFileArgs,
};

fn terminal_manager() -> &'static TerminalManager {
    static MANAGER: OnceLock<TerminalManager> = OnceLock::new();
    MANAGER.get_or_init(TerminalManager::new)
}

pub fn builtin_tool_definitions() -> Vec<Tool> {
    vec![
        tool(
            "bash",
            "在当前工作目录执行 shell 命令。",
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
        ),
        tool(
            "terminal_create",
            "创建一个可持续交互的终端会话。",
            json!({
                "type": "object",
                "properties": {
                    "cwd": { "type": "string" },
                    "shell": { "type": "string" },
                    "env": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "key": { "type": "string" },
                                "value": { "type": "string" }
                            },
                            "required": ["key", "value"]
                        }
                    }
                }
            }),
        ),
        tool(
            "terminal_write",
            "向指定终端会话写入输入。",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "input": { "type": "string" }
                },
                "required": ["session_id", "input"]
            }),
        ),
        tool(
            "terminal_read",
            "读取指定终端会话的增量输出。",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "from": { "type": "integer", "minimum": 0 }
                },
                "required": ["session_id"]
            }),
        ),
        tool(
            "terminal_list",
            "列出当前所有终端会话。",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "terminal_status",
            "查看指定终端会话状态。",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" }
                },
                "required": ["session_id"]
            }),
        ),
        tool(
            "terminal_kill",
            "结束指定终端会话。",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" }
                },
                "required": ["session_id"]
            }),
        ),
        tool(
            "read_file",
            "读取文件内容。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_lines": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "write_file",
            "写入文件。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        tool(
            "edit_file",
            "替换文件中的精确文本。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old": { "type": "string" },
                    "new": { "type": "string" }
                },
                "required": ["path", "old", "new"]
            }),
        ),
    ]
}

pub fn handle_builtin_tool_call(call: &ToolCall) -> anyhow::Result<Option<String>> {
    let output = match call.function.name.as_str() {
        "bash" => {
            let args: BashArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid bash arguments")?;
            BashTool::run(&args.command)?
        }
        "terminal_create" => {
            let args: TerminalCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid terminal_create arguments")?;
            let request = TerminalCreateRequest {
                cwd: args.cwd.map(PathBuf::from),
                shell: args.shell,
                env: args
                    .env
                    .unwrap_or_default()
                    .into_iter()
                    .map(|item| (item.key, item.value))
                    .collect(),
            };
            serde_json::to_string_pretty(&terminal_manager().create(request)?)?
        }
        "terminal_write" => {
            let args: TerminalWriteArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid terminal_write arguments")?;
            terminal_manager().write(&args.session_id, &args.input)?;
            format!("已写入会话 {}", args.session_id)
        }
        "terminal_read" => {
            let args: TerminalReadArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid terminal_read arguments")?;
            serde_json::to_string_pretty(
                &terminal_manager().read(&args.session_id, args.from.unwrap_or(0))?,
            )?
        }
        "terminal_list" => serde_json::to_string_pretty(&terminal_manager().list()?)?,
        "terminal_status" => {
            let args: TerminalSessionArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid terminal_status arguments")?;
            serde_json::to_string_pretty(&terminal_manager().status(&args.session_id)?)?
        }
        "terminal_kill" => {
            let args: TerminalSessionArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid terminal_kill arguments")?;
            serde_json::to_string_pretty(&terminal_manager().kill(&args.session_id)?)?
        }
        "read_file" => {
            let args: ReadFileArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid read_file arguments")?;
            read_file(&args)?
        }
        "write_file" => {
            let args: WriteFileArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid write_file arguments")?;
            write_file(&args)?
        }
        "edit_file" => {
            let args: EditFileArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid edit_file arguments")?;
            edit_file(&args)?
        }
        _ => return Ok(None),
    };

    Ok(Some(output))
}

pub fn reset_terminal_manager() -> anyhow::Result<()> {
    terminal_manager().reset()
}

pub fn clear_terminal_manager_live_sessions() -> anyhow::Result<()> {
    terminal_manager().clear_live_sessions()
}

fn tool(name: &str, description: &str, parameters: serde_json::Value) -> Tool {
    Tool {
        r#type: "function".to_string(),
        function: ToolFunction {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}

#[derive(Debug, Deserialize)]
struct TerminalEnvArg {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct TerminalCreateArgs {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    env: Option<Vec<TerminalEnvArg>>,
}

#[derive(Debug, Deserialize)]
struct TerminalSessionArgs {
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct TerminalWriteArgs {
    session_id: String,
    input: String,
}

#[derive(Debug, Deserialize)]
struct TerminalReadArgs {
    session_id: String,
    #[serde(default)]
    from: Option<usize>,
}
