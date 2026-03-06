use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

use crate::openai_compat::{Tool, ToolCall, ToolFunction};

use super::ProjectContext;

pub fn project_tool_definitions() -> Vec<Tool> {
    vec![
        tool(
            "task_create",
            "在共享任务板中创建任务。",
            json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["subject"]
            }),
        ),
        tool(
            "task_list",
            "列出所有任务及其 owner/worktree。",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "task_get",
            "按 ID 获取任务详情。",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "integer" } },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_update",
            "更新任务状态或 owner。",
            json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                    "owner": { "type": "string" }
                },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_bind_worktree",
            "将任务绑定到一个 worktree 名称。",
            json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "worktree": { "type": "string" },
                    "owner": { "type": "string" }
                },
                "required": ["task_id", "worktree"]
            }),
        ),
        tool(
            "worktree_create",
            "创建 git worktree 并可选绑定任务。",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "task_id": { "type": "integer" },
                    "base_ref": { "type": "string" }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_list",
            "列出 .worktrees/index.json 中的 worktree。",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "worktree_status",
            "查看某个 worktree 的 git 状态。",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_run",
            "在指定 worktree 中执行命令。",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "command": { "type": "string" }
                },
                "required": ["name", "command"]
            }),
        ),
        tool(
            "worktree_keep",
            "将 worktree 标记为保留。",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_remove",
            "移除 worktree，可选同时完成任务。",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "force": { "type": "boolean" },
                    "complete_task": { "type": "boolean" }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_events",
            "查看最近的 worktree 生命周期事件。",
            json!({
                "type": "object",
                "properties": { "limit": { "type": "integer" } }
            }),
        ),
    ]
}

pub fn handle_project_tool_call(
    context: &ProjectContext,
    call: &ToolCall,
) -> anyhow::Result<Option<String>> {
    let tasks = context.tasks();
    let events = context.events();
    let worktrees = context.worktrees();

    let output = match call.function.name.as_str() {
        "task_create" => {
            let args: TaskCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_create arguments")?;
            tasks.create(&args.subject, args.description.as_deref().unwrap_or(""))?
        }
        "task_list" => tasks.list_all()?,
        "task_get" => {
            let args: TaskIdArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_get arguments")?;
            tasks.get(args.task_id)?
        }
        "task_update" => {
            let args: TaskUpdateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_update arguments")?;
            tasks.update(args.task_id, args.status.as_deref(), args.owner.as_deref())?
        }
        "task_bind_worktree" => {
            let args: TaskBindArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_bind_worktree arguments")?;
            tasks.bind_worktree(
                args.task_id,
                &args.worktree,
                args.owner.as_deref().unwrap_or(""),
            )?
        }
        "worktree_create" => {
            let args: WorktreeCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_create arguments")?;
            worktrees.create(
                &args.name,
                args.task_id,
                args.base_ref.as_deref().unwrap_or("HEAD"),
            )?
        }
        "worktree_list" => worktrees.list_all()?,
        "worktree_status" => {
            let args: WorktreeNameArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_status arguments")?;
            worktrees.status(&args.name)?
        }
        "worktree_run" => {
            let args: WorktreeRunArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_run arguments")?;
            worktrees.run(&args.name, &args.command)?
        }
        "worktree_keep" => {
            let args: WorktreeNameArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_keep arguments")?;
            worktrees.keep(&args.name)?
        }
        "worktree_remove" => {
            let args: WorktreeRemoveArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_remove arguments")?;
            worktrees.remove(
                &args.name,
                args.force.unwrap_or(false),
                args.complete_task.unwrap_or(false),
            )?
        }
        "worktree_events" => {
            let args: EventsArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_events arguments")?;
            events.list_recent(args.limit.unwrap_or(20))?
        }
        _ => return Ok(None),
    };

    Ok(Some(output))
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
struct TaskCreateArgs {
    subject: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskIdArgs {
    task_id: u64,
}

#[derive(Debug, Deserialize)]
struct TaskUpdateArgs {
    task_id: u64,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskBindArgs {
    task_id: u64,
    worktree: String,
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorktreeCreateArgs {
    name: String,
    #[serde(default)]
    task_id: Option<u64>,
    #[serde(default)]
    base_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorktreeNameArgs {
    name: String,
}

#[derive(Debug, Deserialize)]
struct WorktreeRunArgs {
    name: String,
    command: String,
}

#[derive(Debug, Deserialize)]
struct WorktreeRemoveArgs {
    name: String,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    complete_task: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct EventsArgs {
    #[serde(default)]
    limit: Option<usize>,
}
