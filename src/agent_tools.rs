use anyhow::Context;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::openai_compat::{Tool, ToolCall, ToolFunction};
use crate::project_tools::TaskManager;
use crate::shell_file_tools::{
    BashArgs, BashTool, EditFileArgs, ReadFileArgs, WriteFileArgs, edit_file,
    is_likely_long_running_command, is_read_only_command, read_file, write_file,
};
use crate::terminal_session::{TerminalCreateRequest, TerminalManager};

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
            "terminal_resize",
            "调整指定终端会话的窗口大小。",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "cols": { "type": "integer", "minimum": 1 },
                    "rows": { "type": "integer", "minimum": 1 }
                },
                "required": ["session_id", "cols", "rows"]
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
        "bash" => run_builtin_tool("bash", || {
            let args: BashArgs = parse_tool_args("bash", &call.function.arguments)?;
            reject_blocking_parent_execution("bash", &args.command)?;
            reject_bash_skill_md_write(&args.command)?;
            run_with_classified_error("bash", BuiltinToolErrorKind::Execution, || {
                BashTool::run(&args.command)
            })
        })?,
        "terminal_create" => run_builtin_tool("terminal_create", || {
            let args: TerminalCreateArgs =
                parse_tool_args("terminal_create", &call.function.arguments)?;
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
            run_with_classified_error("terminal_create", BuiltinToolErrorKind::Session, || {
                Ok(serde_json::to_string_pretty(
                    &terminal_manager().create(request)?,
                )?)
            })
        })?,
        "terminal_write" => run_builtin_tool("terminal_write", || {
            let args: TerminalWriteArgs =
                parse_tool_args("terminal_write", &call.function.arguments)?;
            reject_blocking_parent_execution("terminal_write", &args.input)?;
            run_with_classified_error("terminal_write", BuiltinToolErrorKind::Session, || {
                terminal_manager().write(&args.session_id, &args.input)?;
                Ok(format!("已写入会话 {}", args.session_id))
            })
        })?,
        "terminal_read" => run_builtin_tool("terminal_read", || {
            let args: TerminalReadArgs =
                parse_tool_args("terminal_read", &call.function.arguments)?;
            run_with_classified_error("terminal_read", BuiltinToolErrorKind::Session, || {
                Ok(serde_json::to_string_pretty(
                    &terminal_manager().read(&args.session_id, args.from.unwrap_or(0))?,
                )?)
            })
        })?,
        "terminal_resize" => run_builtin_tool("terminal_resize", || {
            let args: TerminalResizeArgs =
                parse_tool_args("terminal_resize", &call.function.arguments)?;
            run_with_classified_error("terminal_resize", BuiltinToolErrorKind::Session, || {
                terminal_manager().resize(&args.session_id, args.cols, args.rows)?;
                Ok(format!(
                    "已调整会话 {} 的终端大小到 {}x{}",
                    args.session_id, args.cols, args.rows
                ))
            })
        })?,
        "terminal_list" => run_builtin_tool("terminal_list", || {
            run_with_classified_error("terminal_list", BuiltinToolErrorKind::Session, || {
                Ok(serde_json::to_string_pretty(&terminal_manager().list()?)?)
            })
        })?,
        "terminal_status" => run_builtin_tool("terminal_status", || {
            let args: TerminalSessionArgs =
                parse_tool_args("terminal_status", &call.function.arguments)?;
            run_with_classified_error("terminal_status", BuiltinToolErrorKind::Session, || {
                Ok(serde_json::to_string_pretty(
                    &terminal_manager().status(&args.session_id)?,
                )?)
            })
        })?,
        "terminal_kill" => run_builtin_tool("terminal_kill", || {
            let args: TerminalSessionArgs =
                parse_tool_args("terminal_kill", &call.function.arguments)?;
            run_with_classified_error("terminal_kill", BuiltinToolErrorKind::Session, || {
                Ok(serde_json::to_string_pretty(
                    &terminal_manager().kill(&args.session_id)?,
                )?)
            })
        })?,
        "read_file" => run_builtin_tool("read_file", || {
            let args: ReadFileArgs = parse_tool_args("read_file", &call.function.arguments)?;
            run_with_classified_error("read_file", BuiltinToolErrorKind::FileSystem, || {
                read_file(&args)
            })
        })?,
        "write_file" => run_builtin_tool("write_file", || {
            let args: WriteFileArgs = parse_tool_args("write_file", &call.function.arguments)?;
            reject_parent_file_write("write_file", &args.path)?;
            reject_skills_dir_write("write_file", &args.path)?;
            run_with_classified_error("write_file", BuiltinToolErrorKind::FileSystem, || {
                write_file(&args)
            })
        })?,
        "edit_file" => run_builtin_tool("edit_file", || {
            let args: EditFileArgs = parse_tool_args("edit_file", &call.function.arguments)?;
            reject_parent_file_write("edit_file", &args.path)?;
            reject_skills_dir_write("edit_file", &args.path)?;
            run_with_classified_error("edit_file", BuiltinToolErrorKind::FileSystem, || {
                edit_file(&args)
            })
        })?,
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

fn reject_blocking_parent_execution(tool: &str, command: &str) -> anyhow::Result<()> {
    if !current_node_is_parent().unwrap_or(false) {
        return Ok(());
    }
    // 白名单：root/parent 只允许只读命令直接执行
    if is_read_only_command(command) {
        return Ok(());
    }
    // 给长耗时命令提供更具体的委托建议
    if is_likely_long_running_command(command) {
        anyhow::bail!(
            "{} refused: root/parent coordinator may only run read-only commands directly. \
             Long-running commands must be delegated. Choose the appropriate path:\n\
             Option A — single child task: use delegate_long_running if one worker can own this process end-to-end. \
             Include the goal and this command: `{}`\n\
             Option B — team: if this process is one step in a larger workflow requiring planning, \
             implementation, and verification by different roles, create multiple tasks with task_create \
             (one per role or phase) and coordinate via team_send.",
            tool,
            command.trim()
        );
    }
    // 其余一切（包括任意 shell 脚本、web 自动化、任何非只读操作）都必须委托
    anyhow::bail!(
        "{} refused: root/parent coordinator may only run read-only shell commands directly \
         (git status/diff/log, ls, cat, grep, find, rg, etc.). \
         This command must be delegated. Choose the appropriate path:\n\
         Option A — single child task: if this is a self-contained action one agent can complete, \
         use task_create with the goal, expected deliverable, and success condition. Command: `{}`\n\
         Option B — team: if this requires coordinated effort across multiple roles or phases \
         (e.g., plan → implement → verify, or parallel workstreams), \
         create one task_create per role/phase and use team_send to coordinate between them.",
        tool,
        command.trim()
    )
}

fn reject_parent_file_write(tool: &str, path: &str) -> anyhow::Result<()> {
    if !current_node_is_parent().unwrap_or(false) {
        return Ok(());
    }
    let normalized = path.trim().replace('\\', "/");
    let allowed_prefixes = [".tasks/", ".team/", "decisions.jsonl"];
    if allowed_prefixes
        .iter()
        .any(|prefix| normalized.contains(prefix))
    {
        return Ok(());
    }
    anyhow::bail!(
        "{} refused: root/parent coordinator must not write source files directly. Path: {}. \
         Choose the appropriate path:\n\
         Option A — single child task: if this is an isolated file change one agent can own, \
         use task_create with the exact file path, expected content/diff, and success condition.\n\
         Option B — team: if this file change is part of a larger feature or refactor spanning \
         multiple files or roles, create one task_create per phase/role and coordinate via team_send.\n\
         Exception: writing to .tasks/ and .team/ planning directories is allowed.",
        tool,
        path
    )
}

fn reject_skills_dir_write(tool: &str, path: &str) -> anyhow::Result<()> {
    let normalized = path.trim().replace('\\', "/");
    let in_skills = normalized.contains("/skills/") || normalized.starts_with("skills/");
    if !in_skills {
        return Ok(());
    }

    // Extract the skill name (first path segment inside skills/)
    let after_skills = if let Some(idx) = normalized.find("/skills/") {
        &normalized[idx + "/skills/".len()..]
    } else {
        &normalized["skills/".len()..]
    };
    let skill_name = after_skills.split('/').next().unwrap_or("");

    // Always block direct creation of SKILL.md — must use skill_create
    let file_name = normalized.split('/').last().unwrap_or("");
    if file_name == "SKILL.md" {
        anyhow::bail!(
            "{} refused: cannot write SKILL.md directly — path: {}.\n\
             Skills MUST be created via the `skill_create` tool, which:\n\
             1. Writes SKILL.md with correct frontmatter\n\
             2. Generates tests/smoke.json with LLM test assertions\n\
             3. Generates tests/integration.py template\n\
             4. Immediately runs static + LLM tests and reports results",
            tool,
            path
        );
    }

    // If the skill directory already exists (SKILL.md is present), allow any other file writes.
    // This lets agents add helper scripts, config files, etc. after skill_create has run.
    if !skill_name.is_empty() {
        let repo_root = std::env::var("RUSTPILOT_REPO_ROOT")
            .ok()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let skill_md = repo_root.join("skills").join(skill_name).join("SKILL.md");
        if skill_md.exists() {
            return Ok(()); // skill was created via skill_create; additional files are allowed
        }
    }

    anyhow::bail!(
        "{} refused: cannot write directly to skills/ — path: {}.\n\
         The skill directory does not exist yet. Use `skill_create` first to initialise the skill \
         (writes SKILL.md, generates test templates, and runs LLM tests). \
         After that you may add helper scripts or other files freely.",
        tool,
        path
    )
}

/// Rejects bash commands that try to write SKILL.md directly, bypassing `skill_create`.
/// Detects common shell write patterns: `> SKILL.md`, `tee SKILL.md`, `cat > SKILL.md`, etc.
fn reject_bash_skill_md_write(command: &str) -> anyhow::Result<()> {
    let lower = command.to_ascii_lowercase();
    let has_skill_md = lower.contains("skill.md");
    if !has_skill_md {
        return Ok(());
    }
    // Detect write-producing patterns
    let is_write = lower.contains("> ") // redirection
        || lower.contains(">>")          // append
        || lower.contains("tee ")        // tee
        || lower.contains("tee\t");
    if is_write {
        anyhow::bail!(
            "bash refused: cannot write SKILL.md via shell command — detected write to SKILL.md.\n\
             Skills MUST be created or updated via the `skill_create` tool, which writes SKILL.md \
             with correct frontmatter, generates tests/smoke.json and tests/integration.py, \
             and immediately runs static + LLM tests."
        );
    }
    Ok(())
}

fn current_agent_id() -> String {
    std::env::var("RUSTPILOT_AGENT_ID").unwrap_or_else(|_| "lead".to_string())
}

pub(crate) fn current_node_is_parent() -> anyhow::Result<bool> {
    if current_task_id().is_none() && current_agent_id() == "lead" {
        return Ok(true);
    }
    let repo_root = std::env::var("RUSTPILOT_REPO_ROOT")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let tasks_dir = repo_root.join(".tasks");
    if !tasks_dir.is_dir() {
        return Ok(current_agent_id() == "lead");
    }
    let tasks = TaskManager::new(tasks_dir)?;
    let parent_task_id = current_task_id();
    Ok(tasks.active_child_count(parent_task_id)? > 0)
}

fn current_task_id() -> Option<u64> {
    std::env::var("RUSTPILOT_TASK_ID")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
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

#[derive(Clone, Copy)]
enum BuiltinToolErrorKind {
    Input,
    Execution,
    FileSystem,
    Session,
}

impl BuiltinToolErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Execution => "execution",
            Self::FileSystem => "filesystem",
            Self::Session => "session",
        }
    }
}

fn parse_tool_args<T: DeserializeOwned>(tool: &str, arguments: &str) -> anyhow::Result<T> {
    serde_json::from_str(arguments)
        .with_context(|| format!("invalid {} arguments", tool))
        .map_err(|err| wrap_builtin_error(tool, BuiltinToolErrorKind::Input, err))
}

fn run_builtin_tool<F>(tool: &str, run: F) -> anyhow::Result<String>
where
    F: FnOnce() -> anyhow::Result<String>,
{
    log_builtin_tool(tool, "start", None);
    match run() {
        Ok(output) => {
            let detail = format!("output_bytes={}", output.len());
            log_builtin_tool(tool, "ok", Some(&detail));
            Ok(output)
        }
        Err(err) => {
            let detail = err.to_string();
            log_builtin_tool(tool, "error", Some(&detail));
            Err(err)
        }
    }
}

fn run_with_classified_error<F>(
    tool: &str,
    kind: BuiltinToolErrorKind,
    run: F,
) -> anyhow::Result<String>
where
    F: FnOnce() -> anyhow::Result<String>,
{
    run().map_err(|err| wrap_builtin_error(tool, kind, err))
}

fn wrap_builtin_error(tool: &str, kind: BuiltinToolErrorKind, err: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "builtin tool '{}' failed [{}]: {}",
        tool,
        kind.as_str(),
        err
    )
}

fn log_builtin_tool(tool: &str, stage: &str, detail: Option<&str>) {
    match detail {
        Some(detail) => eprintln!("[builtin:{}] {} {}", tool, stage, detail),
        None => eprintln!("[builtin:{}] {}", tool, stage),
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

#[derive(Debug, Deserialize)]
struct TerminalResizeArgs {
    session_id: String,
    cols: u16,
    rows: u16,
}
