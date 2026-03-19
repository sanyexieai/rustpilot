#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStrategy {
    Generic,
    Timeout,
    JsonOnly,
    Endpoint,
    Auth,
}

#[derive(Debug, Clone)]
pub struct PromptPolicyConfig {
    timeout_tokens: Vec<String>,
    json_only_tokens: Vec<String>,
    auth_tokens: Vec<String>,
    endpoint_tokens: Vec<String>,
    timeout_prompt_max_chars: usize,
    compact_clause: String,
    json_contract_clause: String,
    diagnostic_contract_clause: String,
    recovery_base_notes: Vec<String>,
    timeout_recovery_note: String,
    endpoint_recovery_note: String,
    auth_recovery_note: String,
    json_only_recovery_note: String,
    default_root_prompt: String,
    default_worker_prompt: String,
}

impl Default for PromptPolicyConfig {
    fn default() -> Self {
        Self {
            timeout_tokens: env_csv("RUSTPILOT_PROMPT_TIMEOUT_TOKENS", &["timeout", "timed out"]),
            json_only_tokens: env_csv(
                "RUSTPILOT_PROMPT_JSON_ONLY_TOKENS",
                &["valid json", "missing field `text`", "missing field 'text'"],
            ),
            auth_tokens: env_csv(
                "RUSTPILOT_PROMPT_AUTH_TOKENS",
                &["401", "unauthorized"],
            ),
            endpoint_tokens: env_csv(
                "RUSTPILOT_PROMPT_ENDPOINT_TOKENS",
                &["404", "not found"],
            ),
            timeout_prompt_max_chars: env_usize("RUSTPILOT_PROMPT_TIMEOUT_MAX_CHARS", 420),
            compact_clause: env_string(
                "RUSTPILOT_PROMPT_COMPACT_CLAUSE",
                "Keep the response compact. Prefer the smallest complete answer and avoid unnecessary tool churn.",
            ),
            json_contract_clause: env_string(
                "RUSTPILOT_PROMPT_JSON_CONTRACT",
                "Return only the exact requested payload as plain text. No Markdown, no code fences, no wrapper objects, no commentary.",
            ),
            diagnostic_contract_clause: env_string(
                "RUSTPILOT_PROMPT_DIAGNOSTIC_CONTRACT",
                "If the failure points to configuration, endpoint, or authentication, verify that first and choose the minimal corrective action before continuing.",
            ),
            recovery_base_notes: vec![
                "If the previous attempt failed, prefer the smallest complete answer that still moves the task forward.".to_string(),
                "Do not add unnecessary narration, markdown wrappers, or speculative alternatives.".to_string(),
                "When using tool calls, keep them minimal and directly relevant to the current task.".to_string(),
            ],
            timeout_recovery_note: env_string(
                "RUSTPILOT_TIMEOUT_RECOVERY_NOTE",
                "The previous attempt timed out. Reduce output size, reduce tool churn, and avoid unnecessary steps.",
            ),
            endpoint_recovery_note: env_string(
                "RUSTPILOT_ENDPOINT_RECOVERY_NOTE",
                "The previous attempt hit a missing endpoint or resource. Check base URLs, paths, and request shape before retrying.",
            ),
            auth_recovery_note: env_string(
                "RUSTPILOT_AUTH_RECOVERY_NOTE",
                "The previous attempt failed authentication. Verify token source, provider, and auth headers before retrying.",
            ),
            json_only_recovery_note: env_string(
                "RUSTPILOT_JSON_ONLY_RECOVERY_NOTE",
                "Return only the exact requested payload shape. Ignore hidden reasoning and avoid wrapper formats.",
            ),
            default_root_prompt: env_string(
                "RUSTPILOT_DEFAULT_ROOT_PROMPT",
                "You are the root architect and coordinator agent. Your role is to plan and delegate - not to implement directly.\n\
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
 - NO (anything requiring execution, file changes, multi-step work, or external interaction): delegate via task_create. Never attempt it yourself.",
            ),
            default_worker_prompt: env_string(
                "RUSTPILOT_DEFAULT_WORKER_PROMPT",
                "You are team member {owner}, role={role}, task_priority={task_priority}. {prompt_focus}\n\
 \n\
 EXECUTION PRINCIPLE:\n\
 Complete the current task autonomously as far as possible.\n\
 Attempt every step you can execute yourself: install dependencies, run scripts, create files, execute commands.\n\
 Only stop and surface to the user when a step is genuinely impossible to automate -\n\
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
    YES - create child tasks (Option A: single task_create; Option B: planning task first, then a team).\n\
    Describe the specific problem clearly so the child can start without asking for clarification.\n\
    Then mark yourself as waiting (blocked) for the child's result.\n\
 2. If the problem cannot be resolved by any agent (missing credentials, external system unavailable):\n\
    explain the blocker clearly and stop. Do NOT create child tasks that will also fail.\n\
 \n\
 Tasks are the control plane and worktrees are the execution plane.\n\
 Use team_send and team_inbox when coordination is required. Repository: {repo_root}",
            ),
        }
    }
}

impl PromptPolicyConfig {
    pub fn load() -> Self {
        Self::default()
    }

    pub fn default_root_prompt(&self) -> &str {
        &self.default_root_prompt
    }

    pub fn default_worker_prompt(&self) -> &str {
        &self.default_worker_prompt
    }

    pub fn classify_recovery_strategy(&self, error_text: &str) -> RecoveryStrategy {
        let lower = error_text.to_ascii_lowercase();
        if contains_any_token(&lower, &self.timeout_tokens) {
            RecoveryStrategy::Timeout
        } else if contains_any_token(&lower, &self.json_only_tokens) {
            RecoveryStrategy::JsonOnly
        } else if contains_any_token(&lower, &self.auth_tokens) {
            RecoveryStrategy::Auth
        } else if contains_any_token(&lower, &self.endpoint_tokens) {
            RecoveryStrategy::Endpoint
        } else {
            RecoveryStrategy::Generic
        }
    }

    pub fn recovery_note_lines(&self, scope: &str, error_text: &str) -> Vec<String> {
        let mut lines = vec![format!("Scope: {}", scope)];
        lines.extend(self.recovery_base_notes.iter().cloned());
        match self.classify_recovery_strategy(error_text) {
            RecoveryStrategy::Timeout => lines.push(self.timeout_recovery_note.clone()),
            RecoveryStrategy::JsonOnly => lines.push(self.json_only_recovery_note.clone()),
            RecoveryStrategy::Endpoint => lines.push(self.endpoint_recovery_note.clone()),
            RecoveryStrategy::Auth => lines.push(self.auth_recovery_note.clone()),
            RecoveryStrategy::Generic => {}
        }
        lines
    }

    pub fn transform_prompt_body(&self, prompt: &str, strategy: RecoveryStrategy) -> String {
        match strategy {
            RecoveryStrategy::Timeout => self.compress_prompt_body(prompt),
            RecoveryStrategy::JsonOnly => self.ensure_json_contract(prompt),
            RecoveryStrategy::Endpoint | RecoveryStrategy::Auth => {
                self.ensure_diagnostic_contract(prompt)
            }
            RecoveryStrategy::Generic => prompt.trim().to_string(),
        }
    }

    pub fn compact_clause(&self) -> &str {
        &self.compact_clause
    }

    pub fn json_contract_clause(&self) -> &str {
        &self.json_contract_clause
    }

    pub fn diagnostic_contract_clause(&self) -> &str {
        &self.diagnostic_contract_clause
    }

    pub fn timeout_prompt_max_chars(&self) -> usize {
        self.timeout_prompt_max_chars
    }

    pub fn compress_prompt_body(&self, prompt: &str) -> String {
        let trimmed = prompt.trim();
        if trimmed.chars().count() <= self.timeout_prompt_max_chars {
            return self.ensure_compact_clause(trimmed);
        }
        let end = trimmed
            .char_indices()
            .map(|(idx, _)| idx)
            .take_while(|idx| *idx < self.timeout_prompt_max_chars)
            .last()
            .unwrap_or(0);
        let compact = if end == 0 { trimmed } else { &trimmed[..end] };
        self.ensure_compact_clause(compact.trim())
    }

    pub fn ensure_compact_clause(&self, prompt: &str) -> String {
        append_unique_clause(prompt, self.compact_clause())
    }

    pub fn ensure_json_contract(&self, prompt: &str) -> String {
        append_unique_clause(prompt, self.json_contract_clause())
    }

    pub fn ensure_diagnostic_contract(&self, prompt: &str) -> String {
        append_unique_clause(prompt, self.diagnostic_contract_clause())
    }
}

fn append_unique_clause(prompt: &str, clause: &str) -> String {
    if prompt.contains(clause) {
        prompt.to_string()
    } else if prompt.trim().is_empty() {
        clause.to_string()
    } else {
        format!("{}\n\n{}", prompt.trim(), clause)
    }
}

fn contains_any_token(haystack: &str, tokens: &[String]) -> bool {
    tokens.iter().any(|token| haystack.contains(token))
}

fn env_csv(key: &str, defaults: &[&str]) -> Vec<String> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !parsed.is_empty() {
        parsed
    } else {
        defaults.iter().map(|value| value.to_string()).collect()
    }
}

fn env_string(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{PromptPolicyConfig, RecoveryStrategy};

    #[test]
    fn classifies_timeout_errors() {
        let config = PromptPolicyConfig::load();
        assert_eq!(
            config.classify_recovery_strategy("request timed out"),
            RecoveryStrategy::Timeout
        );
    }

    #[test]
    fn injects_json_clause() {
        let config = PromptPolicyConfig::load();
        let out = config.ensure_json_contract("hello");
        assert!(out.contains(config.json_contract_clause()));
    }
}
