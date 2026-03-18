use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const ROOT_PROMPT_FILE: &str = "root_agent_prompt.md";
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

pub fn load_root_prompt(repo_root: &Path) -> anyhow::Result<String> {
    let path = resolve_root_prompt_path(repo_root);
    ensure_prompt_file(&path, default_root_prompt())?;
    Ok(fs::read_to_string(path)?)
}

pub fn load_lead_prompt(repo_root: &Path) -> anyhow::Result<String> {
    load_root_prompt(repo_root)
}

pub fn load_worker_prompt(repo_root: &Path) -> anyhow::Result<String> {
    let path = repo_root.join(".team").join(WORKER_PROMPT_FILE);
    ensure_prompt_file(&path, default_worker_prompt())?;
    Ok(fs::read_to_string(path)?)
}

pub fn adapt_root_prompt(repo_root: &Path, error_text: &str) -> anyhow::Result<bool> {
    Ok(adapt_root_prompt_detailed(repo_root, error_text)?.changed)
}

pub fn adapt_root_prompt_detailed(
    repo_root: &Path,
    error_text: &str,
) -> anyhow::Result<PromptAdaptation> {
    let path = resolve_root_prompt_path(repo_root);
    adapt_prompt_with_error(
        &path,
        default_root_prompt(),
        "root-recovery",
        "root",
        error_text,
    )
}

pub fn adapt_lead_prompt(repo_root: &Path, error_text: &str) -> anyhow::Result<bool> {
    adapt_root_prompt(repo_root, error_text)
}

pub fn adapt_lead_prompt_detailed(
    repo_root: &Path,
    error_text: &str,
) -> anyhow::Result<PromptAdaptation> {
    adapt_root_prompt_detailed(repo_root, error_text)
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

pub fn render_root_system_prompt(repo_root: &Path) -> anyhow::Result<String> {
    let prompt = load_root_prompt(repo_root)?;
    Ok(format!(
        "{}\n\n{}\n\n{}\n\n{}\n\nRepository: {}",
        prompt.trim(),
        hierarchical_task_protocol(),
        skill_authoring_protocol(),
        root_planning_protocol(),
        repo_root.display()
    ))
}

pub fn render_lead_system_prompt(repo_root: &Path) -> anyhow::Result<String> {
    render_root_system_prompt(repo_root)
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
        .replace("{repo_root}", &repo_root.display().to_string())
        + "\n\n"
        + hierarchical_task_protocol())
}

pub fn root_prompt_recovery(repo_root: &Path) -> anyhow::Result<Option<PromptRecoveryInfo>> {
    read_prompt_recovery(&resolve_root_prompt_path(repo_root))
}

pub fn worker_prompt_recovery(repo_root: &Path) -> anyhow::Result<Option<PromptRecoveryInfo>> {
    read_prompt_recovery(&repo_root.join(".team").join(WORKER_PROMPT_FILE))
}

pub fn lead_prompt_recovery(repo_root: &Path) -> anyhow::Result<Option<PromptRecoveryInfo>> {
    root_prompt_recovery(repo_root)
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

fn resolve_root_prompt_path(repo_root: &Path) -> PathBuf {
    let team_dir = repo_root.join(".team");
    let root = team_dir.join(ROOT_PROMPT_FILE);
    if root.exists() {
        return root;
    }
    team_dir.join(LEAD_PROMPT_FILE)
}

fn default_root_prompt() -> &'static str {
    "You are the root architect and coordinator agent. Your role is to plan and delegate — not to implement directly.\n\
     \n\
     ALLOWED for root:\n\
     - Reading files and exploring the codebase (read_file, bash with read-only commands)\n\
     - Running read-only shell commands (git status, git log, git diff, ls, cat, rg, find, etc.)\n\
     - Creating tasks with task_create to delegate implementation work to child agents\n\
     - Using delegate_long_running for background processes and long-running commands\n\
     - Writing to .tasks/ and .team/ directories (planning and coordination artifacts)\n\
     - Sending and receiving team messages (team_send, team_inbox)\n\
     - Reviewing and summarizing results returned by child agents\n\
     \n\
     NOT ALLOWED for root:\n\
     - Directly writing or editing source files (write_file, edit_file on non-planning paths)\n\
     - Running state-mutating shell commands (git commit, git push, mkdir, rm, cp, cargo fmt, etc.)\n\
     - Running expensive or long-running commands (cargo build, cargo test, npm install, etc.)\n\
     \n\
     When given any request, ask one question first: can this be fully resolved in a single LLM response turn?\n\
     - YES (answer, explain, summarize, give advice): respond directly. No tools needed.\n\
     - NO (anything requiring execution, file changes, multi-step work, or external interaction): delegate via task_create. Never attempt it yourself."
}

fn default_worker_prompt() -> &'static str {
    "You are team member {owner}, role={role}, task_priority={task_priority}. {prompt_focus}\n\
     \n\
     EXECUTION PRINCIPLE:\n\
     Complete the current task autonomously as far as possible.\n\
     Attempt every step you can execute yourself: install dependencies, run scripts, create files, execute commands.\n\
     Only stop and surface to the user when a step is genuinely impossible to automate —\n\
     such as scanning a QR code, entering a CAPTCHA, or approving an irreversible real-world action.\n\
     For those blockers: give the user exactly one clear instruction, then wait.\n\
     Never ask the user to do something you can do yourself.\n\
     \n\
     TEAM PLANNING PRINCIPLE (when spawning a team):\n\
     Before creating multiple agents, first create one planning task.\n\
     The planner's job: analyze what functional agents are needed, define each agent's input/output contract,\n\
     then create them with task_create. Each agent receives a precise description with goal, method, and success condition.\n\
     Agents coordinate via team_send. Results flow back to the parent.\n\
     \n\
     SELF-HEALING PRINCIPLE:\n\
     If you are stuck, failing, or blocked on a sub-problem:\n\
     1. Assess: can a child agent resolve this?\n\
        YES — create child tasks (Option A: single task_create; Option B: planning task first, then a team).\n\
        Describe the specific problem clearly so the child can start without asking for clarification.\n\
        Then mark yourself as waiting (blocked) for the child's result.\n\
     2. If the problem cannot be resolved by any agent (missing credentials, external system unavailable):\n\
        explain the blocker clearly and stop. Do NOT create child tasks that will also fail.\n\
     \n\
     Tasks are the control plane and worktrees are the execution plane.\n\
     Use team_send and team_inbox when coordination is required. Repository: {repo_root}"
}

fn hierarchical_task_protocol() -> &'static str {
    "Hierarchical Task Expansion Protocol:\n\
1. First decide whether the current task can be completed directly.\n\
2. Treat a task as directly completable when it needs no more than 2 tool types and usually no more than 10 steps per tool.\n\
3. If it is not directly completable, decompose it and delegate to sub-agents.\n\
4. Keep delegation bounded: a parent agent should create no more than 10 direct sub-agents.\n\
5. Every delegated task must state the exact task, expected deliverable, and success condition.\n\
6. A child agent must apply the same protocol before creating deeper children.\n\
7. If a child cannot finish promptly, or depth, sub-agent count, or step count exceeds the threshold, report back to the parent for replanning instead of pushing forward blindly.\n\
8. The parent agent may create, pause, cancel, or raise priority on child tasks when replanning is needed.\n\
9. If an over-threshold decision cannot be resolved by the current parent, escalate upward level by level; if the chain still cannot decide, escalate to the user."
}

fn skill_authoring_protocol() -> &'static str {
    "Skill Authoring Protocol:\n\
Creating a skill MUST be done exclusively via the `skill_create` tool. Never write SKILL.md or any files under skills/ directly with file-writing tools. \
The `skill_create` tool requires: `name`, `description`, `body`, `test_prompt` (a representative user request that exercises the skill's core capability), and `expect_response_contains` (keywords that must appear in the LLM response). \
After creation the tool automatically runs a live LLM test using the skill as system prompt; the skill is only considered complete when all tests pass. \
If the LLM test fails, revise the skill body or the test expectations and call `skill_create` again with a corrected version (delete the failed skill directory first). \
For skills involving real execution (browser automation, API calls, etc.), write an integration test by editing tests/integration.py — a template is generated automatically. \
The integration test can pause for human interaction by calling wait_for_human('message'), which sends a mail notification; resume by calling `skill_test_signal` after completing the action. \
To validate an existing skill (all test tiers), use `skill_validate` with the skill name."
}

fn root_planning_protocol() -> &'static str {
    "Root Delegation Protocol:\n\
\n\
Step 1 — Assess: can this request be fully resolved in a single LLM response, with no tools?\n\
  YES → answer directly (explain, summarize, advise). Done.\n\
  NO  → proceed to Step 2.\n\
\n\
Step 2 — Assess complexity: how many roles or phases does this require?\n\
  SIMPLE (one agent can own it end-to-end): use task_create once.\n\
    The task description must include: goal, approach, expected deliverable, success condition.\n\
    Also specify: which steps require user interaction (e.g. scan QR, enter captcha) and which the agent must do autonomously.\n\
  COMPLEX (multiple roles, phases, or parallel workstreams):\n\
    First create ONE planning task. The planner's job is to determine exactly which functional\n\
    agents are needed, define each one's input/output contract, and create them with task_create.\n\
    Do not create all team tasks yourself — let the planner do that after analyzing the full scope.\n\
    Agents coordinate via team_send. Results flow back up.\n\
\n\
Step 3 — Delegate in your first response. Do not explore the codebase or run commands first.\n\
  The child agent will do the discovery. Your job is to write a clear enough task description\n\
  that the child can start without asking you for clarification.\n\
\n\
Step 4 — Coordinate: monitor via task_list and team_inbox. Replan when a child is blocked.\n\
\n\
Special cases:\n\
  Long-running processes (dev server, watcher, log tail): use delegate_long_running.\n\
  Anything else requiring execution: task_create. Never run it from root."
}

#[cfg(test)]
mod tests {
    use super::{
        RecoveryStrategy, classify_recovery_strategy, compress_prompt_body, load_root_prompt,
        strip_recovery_section,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "{}_{}_{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("time")
                    .as_nanos()
            );
            let path = std::env::temp_dir().join("rustpilot_prompt_tests").join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

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

    #[test]
    fn root_prompt_prefers_root_file_when_present() {
        let temp = TestDir::new("root_prompt_prefers_root");
        let team_dir = temp.path().join(".team");
        fs::create_dir_all(&team_dir).expect("team dir");
        fs::write(
            team_dir.join("root_agent_prompt.md"),
            "You are the root agent.",
        )
        .expect("write root prompt");
        fs::write(
            team_dir.join("lead_agent_prompt.md"),
            "You are the legacy lead agent.",
        )
        .expect("write legacy prompt");

        let prompt = load_root_prompt(temp.path()).expect("load prompt");
        assert_eq!(prompt, "You are the root agent.");
    }

    #[test]
    fn root_prompt_falls_back_to_legacy_lead_file() {
        let temp = TestDir::new("root_prompt_falls_back");
        let team_dir = temp.path().join(".team");
        fs::create_dir_all(&team_dir).expect("team dir");
        fs::write(
            team_dir.join("lead_agent_prompt.md"),
            "You are the legacy lead agent.",
        )
        .expect("write legacy prompt");

        let prompt = load_root_prompt(temp.path()).expect("load prompt");
        assert_eq!(prompt, "You are the legacy lead agent.");
    }
}
