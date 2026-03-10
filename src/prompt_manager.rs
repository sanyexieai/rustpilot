use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const LEAD_PROMPT_FILE: &str = "lead_agent_prompt.md";
const WORKER_PROMPT_FILE: &str = "worker_agent_prompt.md";
const RECOVERY_BEGIN: &str = "<!-- auto-recovery:begin -->";
const RECOVERY_END: &str = "<!-- auto-recovery:end -->";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRecoveryInfo {
    pub strategy: String,
    pub trigger: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAdaptation {
    pub changed: bool,
    pub file_path: PathBuf,
    pub before: String,
    pub after: String,
    pub recovery: Option<PromptRecoveryInfo>,
}

pub fn load_lead_prompt(repo_root: &Path) -> anyhow::Result<String> {
    let path = repo_root.join(".team").join(LEAD_PROMPT_FILE);
    ensure_prompt_file(&path, default_lead_prompt())?;
    Ok(fs::read_to_string(path)?)
}

pub fn load_worker_prompt(repo_root: &Path) -> anyhow::Result<String> {
    let path = repo_root.join(".team").join(WORKER_PROMPT_FILE);
    ensure_prompt_file(&path, default_worker_prompt())?;
    Ok(fs::read_to_string(path)?)
}

pub fn adapt_lead_prompt(repo_root: &Path, error_text: &str) -> anyhow::Result<bool> {
    Ok(adapt_lead_prompt_detailed(repo_root, error_text)?.changed)
}

pub fn adapt_lead_prompt_detailed(
    repo_root: &Path,
    error_text: &str,
) -> anyhow::Result<PromptAdaptation> {
    let path = repo_root.join(".team").join(LEAD_PROMPT_FILE);
    adapt_prompt_with_error(
        &path,
        default_lead_prompt(),
        "lead-recovery",
        "lead",
        error_text,
    )
}

pub fn adapt_worker_prompt(repo_root: &Path, error_text: &str) -> anyhow::Result<bool> {
    Ok(adapt_worker_prompt_detailed(repo_root, error_text)?.changed)
}

pub fn adapt_worker_prompt_detailed(
    repo_root: &Path,
    error_text: &str,
) -> anyhow::Result<PromptAdaptation> {
    let path = repo_root.join(".team").join(WORKER_PROMPT_FILE);
    adapt_prompt_with_error(
        &path,
        default_worker_prompt(),
        "worker-recovery",
        "worker",
        error_text,
    )
}

pub fn render_lead_system_prompt(repo_root: &Path) -> anyhow::Result<String> {
    let prompt = load_lead_prompt(repo_root)?;
    Ok(format!(
        "{}\n\nRepository: {}",
        prompt.trim(),
        repo_root.display()
    ))
}

pub fn render_worker_system_prompt(
    repo_root: &Path,
    owner: &str,
    role: &str,
    task_priority: &str,
    prompt_focus: &str,
) -> anyhow::Result<String> {
    let template = load_worker_prompt(repo_root)?;
    Ok(template
        .replace("{owner}", owner)
        .replace("{role}", role)
        .replace("{task_priority}", task_priority)
        .replace("{prompt_focus}", prompt_focus)
        .replace("{repo_root}", &repo_root.display().to_string()))
}

pub fn lead_prompt_recovery(repo_root: &Path) -> anyhow::Result<Option<PromptRecoveryInfo>> {
    read_prompt_recovery(&repo_root.join(".team").join(LEAD_PROMPT_FILE))
}

pub fn worker_prompt_recovery(repo_root: &Path) -> anyhow::Result<Option<PromptRecoveryInfo>> {
    read_prompt_recovery(&repo_root.join(".team").join(WORKER_PROMPT_FILE))
}

pub fn adapt_prompt_with_error(
    path: &Path,
    default_content: &str,
    note_id: &str,
    scope: &str,
    error_text: &str,
) -> anyhow::Result<PromptAdaptation> {
    ensure_prompt_file(&path.to_path_buf(), default_content)?;
    adapt_prompt_file(
        &path.to_path_buf(),
        note_id,
        &recovery_note_for_error(scope, error_text),
    )
}

pub fn read_prompt_recovery(path: &Path) -> anyhow::Result<Option<PromptRecoveryInfo>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let Some(begin) = content.find(RECOVERY_BEGIN) else {
        return Ok(None);
    };
    let Some(end) = content.find(RECOVERY_END) else {
        return Ok(None);
    };
    let block = &content[begin..end];
    let strategy = block
        .lines()
        .find_map(|line| line.trim().strip_prefix("Strategy: "))
        .unwrap_or("Generic")
        .trim()
        .to_string();
    let trigger = block
        .lines()
        .find_map(|line| line.trim().strip_prefix("Recovery trigger: "))
        .unwrap_or("")
        .trim()
        .to_string();
    let note = block
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty()
                && trimmed != RECOVERY_BEGIN
                && !trimmed.starts_with("<!-- auto-recovery:")
                && trimmed != "## Auto-Recovery Note"
                && !trimmed.starts_with("Strategy: ")
                && !trimmed.starts_with("Recovery trigger: ")
        })
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Some(PromptRecoveryInfo {
        strategy,
        trigger,
        note,
    }))
}

fn ensure_prompt_file(path: &PathBuf, default_content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        fs::write(path, default_content)?;
    }
    Ok(())
}

fn adapt_prompt_file(
    path: &PathBuf,
    note_id: &str,
    note_body: &str,
) -> anyhow::Result<PromptAdaptation> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let strategy = classify_recovery_strategy(note_body);
    let base_prompt = strip_recovery_section(&existing).trim().to_string();
    let transformed_base = transform_prompt_body(&base_prompt, strategy);
    let recovery_section = render_recovery_section(note_id, note_body, strategy);
    let updated = format_prompt_with_recovery(&transformed_base, &recovery_section);
    if normalize_prompt_text(&existing) == normalize_prompt_text(&updated) {
        return Ok(PromptAdaptation {
            changed: false,
            file_path: path.clone(),
            before: existing.clone(),
            after: existing,
            recovery: read_prompt_recovery(path)?,
        });
    }
    fs::write(path, updated)?;
    Ok(PromptAdaptation {
        changed: true,
        file_path: path.clone(),
        before: existing,
        after: fs::read_to_string(path)?,
        recovery: read_prompt_recovery(path)?,
    })
}

fn recovery_note_for_error(scope: &str, error_text: &str) -> String {
    let mut lines = vec![
        format!("Scope: {}", scope),
        "If the previous attempt failed, prefer the smallest complete answer that still moves the task forward.".to_string(),
        "Do not add unnecessary narration, markdown wrappers, or speculative alternatives.".to_string(),
        "When using tool calls, keep them minimal and directly relevant to the current task.".to_string(),
    ];

    let lower = error_text.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        lines.push("The previous attempt timed out. Reduce output size, reduce tool churn, and avoid unnecessary steps.".to_string());
    }
    if lower.contains("404") || lower.contains("not found") {
        lines.push("The previous attempt hit a missing endpoint or resource. Check base URLs, paths, and request shape before retrying.".to_string());
    }
    if lower.contains("401") || lower.contains("unauthorized") {
        lines.push("The previous attempt failed authentication. Verify token source, provider, and auth headers before retrying.".to_string());
    }
    if lower.contains("valid json")
        || lower.contains("missing field `text`")
        || lower.contains("missing field 'text'")
    {
        lines.push("Return only the exact requested payload shape. Ignore hidden reasoning and avoid wrapper formats.".to_string());
    }

    lines.push(format!(
        "Recovery trigger: {}",
        error_text.lines().next().unwrap_or(error_text).trim()
    ));
    lines.join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryStrategy {
    Generic,
    Timeout,
    JsonOnly,
    Endpoint,
    Auth,
}

fn classify_recovery_strategy(error_text: &str) -> RecoveryStrategy {
    let lower = error_text.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        RecoveryStrategy::Timeout
    } else if lower.contains("valid json")
        || lower.contains("missing field `text`")
        || lower.contains("missing field 'text'")
    {
        RecoveryStrategy::JsonOnly
    } else if lower.contains("401") || lower.contains("unauthorized") {
        RecoveryStrategy::Auth
    } else if lower.contains("404") || lower.contains("not found") {
        RecoveryStrategy::Endpoint
    } else {
        RecoveryStrategy::Generic
    }
}

fn strip_recovery_section(text: &str) -> String {
    if let (Some(begin), Some(end)) = (text.find(RECOVERY_BEGIN), text.find(RECOVERY_END)) {
        let mut output = String::new();
        output.push_str(text[..begin].trim_end());
        if end + RECOVERY_END.len() < text.len() {
            let tail = text[end + RECOVERY_END.len()..].trim();
            if !tail.is_empty() {
                if !output.is_empty() {
                    output.push_str("\n\n");
                }
                output.push_str(tail);
            }
        }
        output
    } else {
        text.to_string()
    }
}

fn transform_prompt_body(prompt: &str, strategy: RecoveryStrategy) -> String {
    match strategy {
        RecoveryStrategy::Timeout => compress_prompt_body(prompt, 420),
        RecoveryStrategy::JsonOnly => ensure_json_contract(prompt),
        RecoveryStrategy::Endpoint | RecoveryStrategy::Auth => ensure_diagnostic_contract(prompt),
        RecoveryStrategy::Generic => prompt.trim().to_string(),
    }
}

fn compress_prompt_body(prompt: &str, max_chars: usize) -> String {
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= max_chars {
        return ensure_compact_clause(trimmed);
    }
    let end = trimmed
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx < max_chars)
        .last()
        .unwrap_or(0);
    let compact = if end == 0 { trimmed } else { &trimmed[..end] };
    ensure_compact_clause(compact.trim())
}

fn ensure_compact_clause(prompt: &str) -> String {
    let clause = "Keep the response compact. Prefer the smallest complete answer and avoid unnecessary tool churn.";
    if prompt.contains(clause) {
        prompt.to_string()
    } else if prompt.is_empty() {
        clause.to_string()
    } else {
        format!("{}\n\n{}", prompt, clause)
    }
}

fn ensure_json_contract(prompt: &str) -> String {
    let clause = "Return only the exact requested payload as plain text. No Markdown, no code fences, no wrapper objects, no commentary.";
    if prompt.contains(clause) {
        prompt.to_string()
    } else {
        format!("{}\n\n{}", prompt.trim(), clause)
    }
}

fn ensure_diagnostic_contract(prompt: &str) -> String {
    let clause = "If the failure points to configuration, endpoint, or authentication, verify that first and choose the minimal corrective action before continuing.";
    if prompt.contains(clause) {
        prompt.to_string()
    } else {
        format!("{}\n\n{}", prompt.trim(), clause)
    }
}

fn render_recovery_section(note_id: &str, note_body: &str, strategy: RecoveryStrategy) -> String {
    format!(
        "{RECOVERY_BEGIN}\n\
<!-- auto-recovery:{note_id} -->\n\
## Auto-Recovery Note\n\
Strategy: {:?}\n\
{}\n\
{RECOVERY_END}",
        strategy,
        note_body.trim()
    )
}

fn format_prompt_with_recovery(base: &str, recovery: &str) -> String {
    let mut output = base.trim().to_string();
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(recovery);
    output.push('\n');
    output
}

fn normalize_prompt_text(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_string()
}

fn default_lead_prompt() -> &'static str {
    "You are the lead coding agent. Use task_* and worktree_* for delegated work. Use team_send and team_inbox when coordinating with the team. Keep momentum, prefer concrete verification, and summarize clearly."
}

fn default_worker_prompt() -> &'static str {
    "You are team member {owner}, role={role}, task_priority={task_priority}. {prompt_focus} Only complete the current task and report the result. Tasks are the control plane and worktrees are the execution plane. Use team_send and team_inbox when coordination is required. Repository: {repo_root}"
}

#[cfg(test)]
mod tests {
    use super::{
        RecoveryStrategy, classify_recovery_strategy, compress_prompt_body, strip_recovery_section,
    };

    #[test]
    fn classify_timeout_strategy() {
        assert_eq!(
            classify_recovery_strategy("operation timed out"),
            RecoveryStrategy::Timeout
        );
    }

    #[test]
    fn classify_json_strategy() {
        assert_eq!(
            classify_recovery_strategy("missing field `text` at line 1 column 10"),
            RecoveryStrategy::JsonOnly
        );
    }

    #[test]
    fn strip_recovery_section_removes_managed_block() {
        let text = "base\n\n<!-- auto-recovery:begin -->\nfoo\n<!-- auto-recovery:end -->\n";
        assert_eq!(strip_recovery_section(text), "base");
    }

    #[test]
    fn compress_prompt_keeps_it_shorter() {
        let prompt = "a".repeat(800);
        assert!(compress_prompt_body(&prompt, 120).chars().count() < prompt.chars().count());
    }
}
