use crate::openai_compat::Message;
use crate::project_tools::{ProjectContext, UiUserIntentMemory};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DEFAULT_UI_PORT: u16 = 3847;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiIntent {
    pub(crate) intent_type: String,
    pub(crate) desired_view: String,
    pub(crate) primary_request: String,
    pub(crate) constraints: Vec<String>,
    pub(crate) operator_notes: Vec<String>,
}

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

pub(crate) fn parse_ui_intent(input: &str) -> Option<UiIntent> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return None;
    }

    let lowered = trimmed.to_lowercase();
    let english_exact = [
        "open ui",
        "open the ui",
        "open dashboard",
        "open the dashboard",
        "open status page",
        "open the status page",
        "open control panel",
        "open management page",
        "show dashboard",
        "show status page",
        "show control panel",
    ];
    if english_exact
        .iter()
        .any(|pattern| lowered.contains(pattern))
    {
        return Some(UiIntent {
            intent_type: "open_management_page".to_string(),
            desired_view: "project_state".to_string(),
            primary_request: trimmed.to_string(),
            constraints: Vec::new(),
            operator_notes: vec!["intent detected from direct ui open phrasing".to_string()],
        });
    }

    let has_open = [
        "open",
        "show",
        "launch",
        "start",
        "\u{6253}\u{5f00}",
        "\u{5f00}\u{542f}",
        "\u{5f00}\u{4e2a}",
        "\u{7ed9}\u{6211}\u{5f00}",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword));
    let wants_tasks = ["task", "tasks", "任务", "工单"]
        .iter()
        .any(|keyword| lowered.contains(keyword));
    let wants_sessions = ["session", "sessions", "会话"]
        .iter()
        .any(|keyword| lowered.contains(keyword));
    let wants_approval = ["approval", "approvals", "审批"]
        .iter()
        .any(|keyword| lowered.contains(keyword));
    let wants_residents = ["resident", "residents", "agent", "agents", "驻留", "代理"]
        .iter()
        .any(|keyword| lowered.contains(keyword));
    let has_ui_surface = [
        "ui",
        "dashboard",
        "status page",
        "control panel",
        "management page",
        "page",
        "panel",
        "\u{9875}\u{9762}",
        "\u{754c}\u{9762}",
        "\u{9762}\u{677f}",
        "\u{72b6}\u{6001}\u{9875}",
        "\u{7ba1}\u{7406}\u{9875}",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword));
    let has_management_intent = [
        "status",
        "current state",
        "system state",
        "project state",
        "manage",
        "management",
        "current",
        "\u{72b6}\u{6001}",
        "\u{5f53}\u{524d}",
        "\u{7ba1}\u{7406}",
        "\u{7cfb}\u{7edf}",
        "\u{9879}\u{76ee}",
    ]
    .iter()
    .any(|keyword| lowered.contains(keyword));

    if !((has_open || has_management_intent) && has_ui_surface) {
        return None;
    }

    let desired_view = if wants_tasks {
        "task_board"
    } else if wants_sessions {
        "session_console"
    } else if wants_approval {
        "approval_overview"
    } else if wants_residents {
        "resident_monitor"
    } else {
        "project_state"
    };

    Some(UiIntent {
        intent_type: "open_management_page".to_string(),
        desired_view: desired_view.to_string(),
        primary_request: trimmed.to_string(),
        constraints: Vec::new(),
        operator_notes: vec![format!("derived desired_view={}", desired_view)],
    })
}

pub(crate) fn ui_intent_to_memory(intent: &UiIntent) -> UiUserIntentMemory {
    UiUserIntentMemory {
        updated_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
        intent_type: intent.intent_type.clone(),
        desired_view: intent.desired_view.clone(),
        primary_request: intent.primary_request.clone(),
        constraints: intent.constraints.clone(),
        operator_notes: intent.operator_notes.clone(),
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn open_browser(url: &str) -> anyhow::Result<()> {
    Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn open_browser(url: &str) -> anyhow::Result<()> {
    Command::new("open").arg(url).spawn()?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn open_browser(url: &str) -> anyhow::Result<()> {
    Command::new("xdg-open").arg(url).spawn()?;
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
pub(crate) fn open_browser(_url: &str) -> anyhow::Result<()> {
    anyhow::bail!("opening a browser is not supported on this platform")
}

pub(crate) fn resolve_ui_port(project: &ProjectContext) -> u16 {
    project
        .residents()
        .get("ui")
        .ok()
        .flatten()
        .and_then(|config| config.listen_port)
        .unwrap_or(DEFAULT_UI_PORT)
}

pub(crate) fn ui_base_url(project: &ProjectContext) -> String {
    format!("http://127.0.0.1:{}", resolve_ui_port(project))
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

#[cfg(test)]
mod tests {
    use super::parse_ui_intent;

    #[test]
    fn ui_open_request_detection_matches_natural_language() {
        assert!(parse_ui_intent("打开一个管理当前状态的页面").is_some());
        assert!(parse_ui_intent("open dashboard").is_some());
        assert!(parse_ui_intent("show me the current status page").is_some());
        assert!(parse_ui_intent("/session current").is_none());
        assert!(parse_ui_intent("continue working on the task").is_none());
    }

    #[test]
    fn ui_intent_parser_derives_specialized_view() {
        let intent =
            parse_ui_intent("open a task dashboard for the current project").expect("intent");
        assert_eq!(intent.intent_type, "open_management_page");
        assert_eq!(intent.desired_view, "task_board");
    }
}
