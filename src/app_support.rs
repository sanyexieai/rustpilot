use crate::openai_compat::Message;
use crate::project_tools::ProjectContext;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InteractionMode {
    TeamQueue,
    Lead,
    Shell,
    Worker { task_id: u64 },
}

impl InteractionMode {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::TeamQueue => "team".to_string(),
            Self::Lead => "lead".to_string(),
            Self::Shell => "shell".to_string(),
            Self::Worker { task_id } => format!("worker({})", task_id),
        }
    }
}

pub(crate) fn parse_interaction_mode_label(raw: &str) -> anyhow::Result<InteractionMode> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("focus cannot be empty");
    }

    let normalized = trimmed.to_ascii_lowercase();
    match normalized.as_str() {
        "lead" => return Ok(InteractionMode::Lead),
        "shell" => return Ok(InteractionMode::Shell),
        "team" | "team_queue" => return Ok(InteractionMode::TeamQueue),
        _ => {}
    }

    if let Some(task_id) = parse_worker_task_id(trimmed) {
        return Ok(InteractionMode::Worker { task_id });
    }

    anyhow::bail!("unsupported focus: {}", raw)
}

fn parse_worker_task_id(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    let candidate = trimmed
        .strip_prefix("worker(")
        .and_then(|value| value.strip_suffix(')'))
        .or_else(|| trimmed.strip_prefix("worker:"))
        .or_else(|| trimmed.strip_prefix("worker "))
        .map(str::trim)?;
    candidate.parse::<u64>().ok()
}

pub(crate) struct TeammateArgs {
    pub(crate) repo_root: PathBuf,
    pub(crate) task_id: u64,
    pub(crate) owner: String,
    pub(crate) role_hint: String,
}

pub(crate) struct ResidentArgs {
    pub(crate) repo_root: PathBuf,
    pub(crate) agent_id: String,
    pub(crate) role: String,
    pub(crate) max_parallel: usize,
}

#[derive(Debug, Deserialize)]
struct MailPoll {
    next_cursor: usize,
    items: Vec<MailItem>,
}

#[derive(Debug, Deserialize)]
struct MailItem {
    cursor: usize,
    msg_id: String,
    msg_type: String,
    from: String,
    message: String,
    task_id: Option<u64>,
    #[serde(default)]
    requires_ack: bool,
}

pub(crate) fn load_repo_env(repo_root: &Path) {
    dotenvy::from_path_override(repo_root.join(".env")).ok();
}

pub(crate) fn trim_messages(messages: &[Message], keep_tail: usize) -> Vec<Message> {
    if messages.len() <= keep_tail.saturating_add(1) {
        return messages.to_vec();
    }
    let mut trimmed = Vec::new();
    if let Some(system) = messages.first().cloned() {
        trimmed.push(system);
    }
    let start = messages.len().saturating_sub(keep_tail);
    for message in &messages[start..] {
        trimmed.push(message.clone());
    }
    trimmed
}

pub(crate) fn parse_priority_prefixed_goal(input: &str) -> (String, String) {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('[')
        && let Some((raw_priority, goal)) = rest.split_once(']')
    {
        let priority = raw_priority.trim().to_lowercase();
        if matches!(priority.as_str(), "critical" | "high" | "medium" | "low") {
            let goal = goal.trim();
            if !goal.is_empty() {
                return (priority, goal.to_string());
            }
        }
    }
    ("medium".to_string(), trimmed.to_string())
}

pub(crate) fn build_priority_task_description(source: &str, priority: &str, goal: &str) -> String {
    format!(
        "[SOURCE={}][PRIORITY={}]\nGoal:\n{}\n\nExecution notes:\n{}",
        source,
        priority,
        goal,
        priority_execution_notes(priority)
    )
}

fn priority_execution_notes(priority: &str) -> &'static str {
    match priority {
        "critical" => {
            "- Confirm scope and impact first\n- Prefer minimal-risk changes\n- Verify behavior explicitly before marking complete\n- Escalate blockers immediately"
        }
        "high" => {
            "- Keep implementation focused\n- Call out assumptions clearly\n- Run at least one concrete verification step\n- Avoid unrelated refactors"
        }
        "medium" => "- Complete the requested work directly\n- Keep changes scoped to the task",
        "low" => "- Favor low-cost, low-risk cleanup\n- Do not expand scope unless necessary",
        _ => "- Complete the requested work directly\n- Keep changes scoped to the task",
    }
}

pub(crate) fn pump_lead_mailbox(
    project: &ProjectContext,
    cursor: &mut usize,
    messages: &mut Vec<Message>,
) -> anyhow::Result<()> {
    let raw = project.mailbox().poll("lead", *cursor, 50)?;
    let polled: MailPoll = serde_json::from_str(&raw)?;
    *cursor = polled.next_cursor;
    for item in polled.items {
        println!(
            "[mail][{}][{}][from={}] {}",
            item.cursor, item.msg_type, item.from, item.message
        );
        if item.msg_type == "task.request_clarification"
            && let Some(task_id) = item.task_id
        {
            let _ = project.tasks().update(task_id, Some("blocked"), None);
            println!(
                "[clarification] task {} blocked, use /reply {} <message>",
                task_id, task_id
            );
        }
        if item.requires_ack {
            let _ = project.mailbox().ack("lead", &item.msg_id, "received");
        }
        if matches!(
            item.msg_type.as_str(),
            "task.started"
                | "task.progress"
                | "task.result"
                | "task.failed"
                | "task.blocked"
                | "task.request_clarification"
        ) {
            messages.push(Message {
                role: "user".to_string(),
                content: Some(format!(
                    "[team-update][from={}][type={}][msg_id={}] {}",
                    item.from, item.msg_type, item.msg_id, item.message
                )),
                tool_call_id: None,
                tool_calls: None,
            });
        }
    }
    Ok(())
}

pub(crate) fn parse_teammate_args(args: &[String]) -> anyhow::Result<TeammateArgs> {
    let mut repo_root = None::<PathBuf>;
    let mut task_id = None::<u64>;
    let mut owner = None::<String>;
    let mut role_hint = None::<String>;
    let mut idx = 0usize;

    while idx < args.len() {
        match args[idx].as_str() {
            "--repo-root" => {
                idx += 1;
                repo_root =
                    Some(PathBuf::from(args.get(idx).ok_or_else(|| {
                        anyhow::anyhow!("missing --repo-root value")
                    })?));
            }
            "--task-id" => {
                idx += 1;
                task_id = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --task-id value"))?
                        .parse::<u64>()?,
                );
            }
            "--owner" => {
                idx += 1;
                owner = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --owner value"))?
                        .to_string(),
                );
            }
            "--role-hint" => {
                idx += 1;
                role_hint = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --role-hint value"))?
                        .to_string(),
                );
            }
            flag => anyhow::bail!("unknown argument: {}", flag),
        }
        idx += 1;
    }

    Ok(TeammateArgs {
        repo_root: repo_root.ok_or_else(|| anyhow::anyhow!("missing --repo-root"))?,
        task_id: task_id.ok_or_else(|| anyhow::anyhow!("missing --task-id"))?,
        owner: owner.unwrap_or_else(|| "teammate".to_string()),
        role_hint: role_hint.unwrap_or_else(|| "developer".to_string()),
    })
}

pub(crate) fn parse_resident_args(
    args: &[String],
    default_max_parallel: usize,
) -> anyhow::Result<ResidentArgs> {
    let mut repo_root = None::<PathBuf>;
    let mut agent_id = None::<String>;
    let mut role = None::<String>;
    let mut max_parallel = None::<usize>;
    let mut idx = 0usize;

    while idx < args.len() {
        match args[idx].as_str() {
            "--repo-root" => {
                idx += 1;
                repo_root =
                    Some(PathBuf::from(args.get(idx).ok_or_else(|| {
                        anyhow::anyhow!("missing --repo-root value")
                    })?));
            }
            "--agent-id" => {
                idx += 1;
                agent_id = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --agent-id value"))?
                        .to_string(),
                );
            }
            "--role" => {
                idx += 1;
                role = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --role value"))?
                        .to_string(),
                );
            }
            "--max-parallel" => {
                idx += 1;
                max_parallel = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --max-parallel value"))?
                        .parse::<usize>()?
                        .max(1),
                );
            }
            flag => anyhow::bail!("unknown argument: {}", flag),
        }
        idx += 1;
    }

    Ok(ResidentArgs {
        repo_root: repo_root.ok_or_else(|| anyhow::anyhow!("missing --repo-root"))?,
        agent_id: agent_id.ok_or_else(|| anyhow::anyhow!("missing --agent-id"))?,
        role: role.ok_or_else(|| anyhow::anyhow!("missing --role"))?,
        max_parallel: max_parallel.unwrap_or(default_max_parallel),
    })
}
