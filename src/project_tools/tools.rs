use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use crate::openai_compat::{Tool, ToolCall, ToolFunction};
use crate::skills::create_prompt_skill;

use super::{ProjectContext, TaskCreateOptions};

pub fn project_tool_definitions() -> Vec<Tool> {
    vec![
        tool(
            "team_send",
            "Send a mailbox message to another team member.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "msg_type": { "type": "string" },
                    "trace_id": { "type": "string" },
                    "requires_ack": { "type": "boolean" },
                    "in_reply_to": { "type": "string" },
                    "message": { "type": "string" },
                    "task_id": { "type": "integer" }
                },
                "required": ["to", "message"]
            }),
        ),
        tool(
            "team_ack",
            "Acknowledge a mailbox message.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "msg_id": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["owner", "msg_id"]
            }),
        ),
        tool(
            "team_poll",
            "Poll for new mailbox messages after a cursor.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "after_cursor": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["owner"]
            }),
        ),
        tool(
            "team_inbox",
            "Read mailbox messages for a team member.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["owner"]
            }),
        ),
        tool(
            "task_create",
            "Create a shared task, optionally linking it to a parent task and delegation depth.",
            json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string" },
                    "description": { "type": "string" },
                    "priority": { "type": "string", "enum": ["critical", "high", "medium", "low"] },
                    "role_hint": { "type": "string", "enum": ["developer", "design", "critic", "ui"] },
                    "parent_task_id": { "type": "integer" },
                    "depth": { "type": "integer", "minimum": 0 }
                },
                "required": ["subject"]
            }),
        ),
        tool(
            "delegate_long_running",
            "Delegate long-running work like dev servers, watch processes, and log-following to a worker-owned task.",
            json!({
                "type": "object",
                "properties": {
                    "goal": { "type": "string" },
                    "command": { "type": "string" },
                    "cwd": { "type": "string" },
                    "priority": { "type": "string", "enum": ["critical", "high", "medium", "low"] },
                    "role_hint": { "type": "string", "enum": ["developer", "design", "critic", "ui"] }
                },
                "required": ["goal", "command"]
            }),
        ),
        tool(
            "skill_create",
            "Create a prompt skill under skills/<name>/SKILL.md using the project's canonical format.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["name", "description", "body"]
            }),
        ),
        tool(
            "task_list",
            "List all tasks with owner and worktree routing data.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "task_get",
            "Get task details by id.",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "integer" } },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_update",
            "Update task status, owner, or priority. Supports paused and cancelled for sub-task control.",
            json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "blocked", "paused", "cancelled", "completed", "failed"] },
                    "owner": { "type": "string" },
                    "priority": { "type": "string", "enum": ["critical", "high", "medium", "low"] }
                },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_bind_worktree",
            "Bind a task to a worktree.",
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
            "Create a git worktree and optionally bind it to a task.",
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
            "List registered worktrees.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "worktree_status",
            "Inspect git status for a worktree.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_run",
            "Run a shell command inside a worktree.",
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
            "Mark a worktree to keep.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_remove",
            "Remove a worktree and optionally complete the bound task.",
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
            "List recent worktree lifecycle events.",
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
    let mailbox = context.mailbox();
    let worktrees = context.worktrees();

    let output = match call.function.name.as_str() {
        "team_send" => {
            let args: TeamSendArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_send arguments")?;
            mailbox.send_typed(
                args.from
                    .as_deref()
                    .unwrap_or(&current_agent_id()),
                &args.to,
                args.msg_type.as_deref().unwrap_or("message"),
                &args.message,
                args.task_id,
                args.trace_id.as_deref(),
                args.requires_ack.unwrap_or(false),
                args.in_reply_to.as_deref(),
            )?
        }
        "team_ack" => {
            let args: TeamAckArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_ack arguments")?;
            mailbox.ack(
                &args.owner,
                &args.msg_id,
                args.note.as_deref().unwrap_or(""),
            )?
        }
        "team_poll" => {
            let args: TeamPollArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_poll arguments")?;
            mailbox.poll(
                &args.owner,
                args.after_cursor.unwrap_or(0),
                args.limit.unwrap_or(20),
            )?
        }
        "team_inbox" => {
            let args: TeamInboxArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_inbox arguments")?;
            mailbox.inbox(&args.owner, args.limit.unwrap_or(20))?
        }
        "task_create" => {
            let args: TaskCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_create arguments")?;
            let inherited = current_task_hierarchy(tasks);
            tasks.create_detailed(
                &args.subject,
                args.description.as_deref().unwrap_or(""),
                TaskCreateOptions {
                    priority: args.priority,
                    role_hint: args.role_hint,
                    parent_task_id: args.parent_task_id.or(inherited.parent_task_id),
                    depth: args.depth.or(inherited.depth),
                },
            )?
        }
        "delegate_long_running" => {
            let args: DelegateLongRunningArgs =
                serde_json::from_str(&call.function.arguments)
                    .context("invalid delegate_long_running arguments")?;
            let priority = args.priority.unwrap_or_else(|| "medium".to_string());
            let role_hint = args.role_hint.unwrap_or_else(|| "developer".to_string());
            let inherited = current_task_hierarchy(tasks);
            let cwd_line = args
                .cwd
                .as_deref()
                .map(|cwd| format!("\nWorking directory:\n{}", cwd.trim()))
                .unwrap_or_default();
            let description = format!(
                "[SOURCE=delegate_long_running][PRIORITY={}]\nGoal:\n{}\n\nLong-running command:\n{}\n{}\n\nExecution notes:\n- This work must run in a worker-owned terminal session\n- The parent node must not hold the blocking process\n- Report back startup status, endpoint/port if relevant, and any blocker",
                priority,
                args.goal.trim(),
                args.command.trim(),
                cwd_line,
            );
            let created = tasks.create_detailed(
                &args.goal,
                &description,
                TaskCreateOptions {
                    priority: Some(priority.clone()),
                    role_hint: Some(role_hint),
                    parent_task_id: inherited.parent_task_id,
                    depth: inherited.depth,
                    ..TaskCreateOptions::default()
                },
            )?;
            format!(
                "delegated long-running work as task:\n{}\n\nnext: let the worker own the terminal session for `{}`",
                created,
                args.command.trim()
            )
        }
        "skill_create" => {
            let args: SkillCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid skill_create arguments")?;
            let created = create_prompt_skill(&args.name, &args.description, &args.body)?;
            format!("skill created: {}", created.join("SKILL.md").display())
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
            tasks.update(
                args.task_id,
                args.status.as_deref(),
                args.owner.as_deref(),
                args.priority.as_deref(),
            )?
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

fn current_agent_id() -> String {
    std::env::var("RUSTPILOT_AGENT_ID").unwrap_or_else(|_| "lead".to_string())
}

fn current_task_hierarchy(tasks: &super::TaskManager) -> TaskCreateOptions {
    let Some(task_id) = std::env::var("RUSTPILOT_TASK_ID")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return TaskCreateOptions::default();
    };

    match tasks.get_record(task_id) {
        Ok(task) => TaskCreateOptions {
            parent_task_id: Some(task.id),
            depth: Some(task.depth.saturating_add(1)),
            ..TaskCreateOptions::default()
        },
        Err(_) => TaskCreateOptions::default(),
    }
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
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    role_hint: Option<String>,
    #[serde(default)]
    parent_task_id: Option<u64>,
    #[serde(default)]
    depth: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DelegateLongRunningArgs {
    goal: String,
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    role_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillCreateArgs {
    name: String,
    description: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct TeamSendArgs {
    #[serde(default)]
    from: Option<String>,
    to: String,
    #[serde(default)]
    msg_type: Option<String>,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    requires_ack: Option<bool>,
    #[serde(default)]
    in_reply_to: Option<String>,
    message: String,
    #[serde(default)]
    task_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TeamAckArgs {
    owner: String,
    msg_id: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TeamPollArgs {
    owner: String,
    #[serde(default)]
    after_cursor: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TeamInboxArgs {
    owner: String,
    #[serde(default)]
    limit: Option<usize>,
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
    #[serde(default)]
    priority: Option<String>,
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
