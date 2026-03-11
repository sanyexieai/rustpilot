use crate::project_tools::{ApprovalBlockRecord, ApprovalMode, ApprovalPolicy, ProjectContext};

pub(crate) fn render_approval_status(project: &ProjectContext) -> anyhow::Result<String> {
    let policy = project.approval().get_policy()?;
    Ok(render_approval_status_text(&policy))
}

pub(crate) fn render_approval_status_text(policy: &ApprovalPolicy) -> String {
    let mut lines = vec![
        format!("approval mode: {}", approval_mode_name(policy.mode)),
        approval_mode_summary(policy.mode).to_string(),
    ];
    lines.push(format!(
        "allowed model tools: {}",
        approval_allowed_tools(policy.mode).join(", ")
    ));
    lines.push(format!(
        "recent block: {}",
        approval_last_block_summary(policy.last_block.as_ref())
    ));
    lines.join("\n")
}

pub(crate) fn approval_mode_name(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Auto => "auto",
        ApprovalMode::ReadOnly => "read_only",
        ApprovalMode::Manual => "manual",
    }
}

pub(crate) fn approval_mode_summary(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Auto => {
            "model shell policy: allow normal shell/worktree commands; still block dangerous commands"
        }
        ApprovalMode::ReadOnly => {
            "model shell policy: allow only clearly read-only shell/worktree commands such as pwd, ls, rg, git status, read_file; block write-like commands"
        }
        ApprovalMode::Manual => {
            "model shell policy: block model-driven shell/worktree execution; use /shell for explicit manual execution"
        }
    }
}

pub(crate) fn approval_allowed_tools(mode: ApprovalMode) -> Vec<&'static str> {
    match mode {
        ApprovalMode::Auto => vec![
            "bash",
            "worktree_run",
            "read_file",
            "write_file",
            "edit_file",
        ],
        ApprovalMode::ReadOnly => vec![
            "bash(read-only only)",
            "worktree_run(read-only only)",
            "read_file",
        ],
        ApprovalMode::Manual => vec!["read_file", "write_file", "edit_file", "manual /shell only"],
    }
}

pub(crate) fn approval_last_block_summary(last_block: Option<&ApprovalBlockRecord>) -> String {
    match last_block {
        Some(block) => format!(
            "{} actor={} reason={} tool={} command={}",
            block.ts, block.actor_id, block.reason_code, block.tool_name, block.command
        ),
        None => "none".to_string(),
    }
}
