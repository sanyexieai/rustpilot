use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::system_model::SystemModel;
use crate::anthropic_compat;
use crate::config::{LlmConfig, default_llm_user_agent, is_model_unsupported_error};
use crate::llm_profiles::LlmApiKind;
use crate::openai_compat::{ChatRequest, ChatResponse, Message};
use crate::project_tools::util::now_secs_f64;
use crate::prompt_manager::{
    PromptAdaptation, PromptRecoveryInfo, adapt_prompt_with_error, read_prompt_recovery,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSurface {
    pub generated_at: f64,
    pub source_fingerprint: String,
    pub title: String,
    pub summary: String,
    pub pages: Vec<UiPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPage {
    pub id: String,
    pub title: String,
    pub purpose: String,
    pub audience: String,
    #[serde(default)]
    pub data_sources: Vec<String>,
    #[serde(default)]
    pub supported_sections: Vec<String>,
    #[serde(default)]
    pub actions: Vec<UiAction>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiAction {
    pub id: String,
    pub title: String,
    pub protocol_id: String,
    pub target: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct UiSurfaceManager {
    path: PathBuf,
    ui_prompt_path: PathBuf,
    planner_prompt_path: PathBuf,
}

impl UiSurfaceManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        let ui_prompt_path = dir.join("ui_agent_prompt.md");
        if !ui_prompt_path.exists() {
            fs::write(&ui_prompt_path, default_ui_prompt())?;
        }
        let planner_prompt_path = dir.join("ui_surface_prompt.md");
        if !planner_prompt_path.exists() {
            fs::write(&planner_prompt_path, default_planner_prompt())?;
        }
        Ok(Self {
            path: dir.join("ui_surface.json"),
            ui_prompt_path,
            planner_prompt_path,
        })
    }

    pub fn snapshot(&self) -> anyhow::Result<Option<UiSurface>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&content)?))
    }

    fn repo_root(&self) -> anyhow::Result<PathBuf> {
        self.path
            .parent()
            .and_then(|item| item.parent())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("failed to resolve repo root for ui surface manager"))
    }

    pub fn needs_refresh(&self, fingerprint: &str) -> anyhow::Result<bool> {
        let existing = self.snapshot()?;
        Ok(existing
            .map(|item| item.source_fingerprint != fingerprint)
            .unwrap_or(true))
    }

    pub fn prompt_text(&self) -> anyhow::Result<String> {
        if !self.ui_prompt_path.exists() {
            fs::write(&self.ui_prompt_path, default_ui_prompt())?;
        }
        Ok(with_managed_ui_prompt_appendix(&fs::read_to_string(
            &self.ui_prompt_path,
        )?))
    }

    pub fn prompt_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&self.prompt_text()?)
    }

    pub fn planner_prompt_text(&self) -> anyhow::Result<String> {
        if !self.planner_prompt_path.exists() {
            fs::write(&self.planner_prompt_path, default_planner_prompt())?;
        }
        Ok(with_managed_planner_prompt_appendix(&fs::read_to_string(
            &self.planner_prompt_path,
        )?))
    }

    pub fn planner_prompt_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&self.planner_prompt_text()?)
    }

    pub fn adapt_ui_prompt_for_error(&self, error_text: &str) -> anyhow::Result<PromptAdaptation> {
        adapt_prompt_with_error(
            &self.ui_prompt_path,
            default_ui_prompt(),
            "ui-schema-recovery",
            "ui schema",
            error_text,
        )
    }

    pub fn adapt_planner_prompt_for_error(
        &self,
        error_text: &str,
    ) -> anyhow::Result<PromptAdaptation> {
        adapt_prompt_with_error(
            &self.planner_prompt_path,
            default_planner_prompt(),
            "ui-surface-recovery",
            "ui surface",
            error_text,
        )
    }

    pub fn ui_prompt_recovery(&self) -> anyhow::Result<Option<PromptRecoveryInfo>> {
        read_prompt_recovery(&self.ui_prompt_path)
    }

    pub fn planner_prompt_recovery(&self) -> anyhow::Result<Option<PromptRecoveryInfo>> {
        read_prompt_recovery(&self.planner_prompt_path)
    }

    pub fn collection_fingerprint(
        &self,
        model: &SystemModel,
        desired_view: &str,
    ) -> anyhow::Result<String> {
        Ok(format!(
            "{}:{}:{}",
            self.planner_prompt_fingerprint()?,
            model_fingerprint(model)?,
            desired_view
        ))
    }

    pub fn rebuild_from_model(
        &self,
        model: &SystemModel,
        desired_view: &str,
    ) -> anyhow::Result<UiSurface> {
        let fingerprint = model_fingerprint(model)?;
        let page = UiPage {
            id: "system-overview".to_string(),
            title: "System Overview".to_string(),
            purpose:
                "Provide one management surface for system state, governance signals, and common operator actions."
                    .to_string(),
            audience: "operator, dispatcher, observer".to_string(),
            data_sources: vec![
                "summary".to_string(),
                "alerts".to_string(),
                "residents".to_string(),
                "main_friend".to_string(),
                "group_chat".to_string(),
                "agent_details".to_string(),
                "process_tree".to_string(),
                "launches".to_string(),
                "tasks".to_string(),
                "proposals".to_string(),
                "decisions".to_string(),
            ],
            supported_sections: vec![
                "metrics".to_string(),
                "alerts".to_string(),
                "residents".to_string(),
                "main_chat".to_string(),
                "group_chat".to_string(),
                "agent_details".to_string(),
                "process_tree".to_string(),
                "launches".to_string(),
                "composer".to_string(),
                "tasks".to_string(),
                "proposals".to_string(),
                "decisions".to_string(),
            ],
            actions: vec![
                UiAction {
                    id: "dispatch-ui".to_string(),
                    title: "Send to UI Agent".to_string(),
                    protocol_id: "ui.request.dispatch".to_string(),
                    target: "ui".to_string(),
                    description:
                        "Request page design changes, structure adjustments, or UI evolution."
                            .to_string(),
                },
                UiAction {
                    id: "dispatch-concierge".to_string(),
                    title: "Send to Concierge".to_string(),
                    protocol_id: "ui.request.dispatch".to_string(),
                    target: "concierge".to_string(),
                    description: "Turn natural-language requests into structured system work."
                        .to_string(),
                },
                UiAction {
                    id: "dispatch-reviewer".to_string(),
                    title: "Send to Reviewer".to_string(),
                    protocol_id: "ui.request.dispatch".to_string(),
                    target: "reviewer".to_string(),
                    description:
                        "Collect blockers, failures, and optimization suggestions."
                            .to_string(),
                },
            ],
            notes: vec![
                "The page structure must follow supported backend protocols and real data sources."
                    .to_string(),
                "ui_surface.json is planning input for the UI agent, not final page code."
                    .to_string(),
            ],
        };
        let mut surface = UiSurface {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint,
            title: "Rustpilot UI Surface".to_string(),
            summary:
                "A planning artifact that captures pages, data sources, and actions for the UI agent."
                    .to_string(),
            pages: vec![page],
        };
        apply_surface_view_bias(&mut surface, desired_view);
        self.save(&surface)?;
        Ok(surface)
    }

    pub fn generate_with_collector(
        &self,
        model: &SystemModel,
        planner_prompt: &str,
        desired_view: &str,
    ) -> anyhow::Result<UiSurface> {
        let fallback = self.rebuild_from_model(model, desired_view)?;
        let model_json = serde_json::to_string_pretty(model)?;
        let fallback_json = serde_json::to_string_pretty(&fallback)?;
        let prompt = format!(
            "You are Rustpilot's UI surface planner. Your job is to inspect the system model and produce a stable `ui_surface.json` planning artifact.\n\
Return exactly one JSON object. Do not output explanations, Markdown, or code fences.\n\
\n\
Planner prompt:\n{planner_prompt}\n\
\n\
Desired view:\n{desired_view}\n\
\n\
Rules:\n\
1. Output planning data, not final HTML and not `UiSchema`.\n\
2. Every page, data source, section, and action must be backed by real backend protocols.\n\
3. `actions.protocol_id` and `actions.target` must match supported protocol definitions.\n\
4. `pages[].supported_sections` and `pages[].data_sources` may only contain supported section/source ids.\n\
5. Bias the plan toward the desired view when deciding what should be emphasized first.\n\
6. Keep the result stable, cacheable, and easy to audit.\n\
\n\
System model:\n{model_json}\n\
\n\
Fallback surface:\n{fallback_json}\n\
\n\
Now output the final JSON:"
        );

        let repo_root = self.repo_root()?;
        let content = std::thread::spawn(move || -> anyhow::Result<String> {
            let config = LlmConfig::from_repo_root(&repo_root)?;
            let client = reqwest::blocking::Client::builder()
                .user_agent(default_llm_user_agent())
                .timeout(Duration::from_secs(45))
                .build()?;
            let messages = vec![
                Message {
                    role: "system".to_string(),
                    content: Some(
                        "You are a protocol-constrained UI surface planner. Output valid JSON only."
                            .to_string(),
                    ),
                    tool_call_id: None,
                    tool_calls: None,
                },
                Message {
                    role: "user".to_string(),
                    content: Some(prompt),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ];

            let mut last_error = None::<String>;
            for model in config.model_candidates() {
                let request = ChatRequest {
                    model: model.clone(),
                    messages: messages.clone(),
                    tools: None,
                    tool_choice: None,
                    temperature: Some(0.2),
                };
                let response = match config.api_kind {
                    LlmApiKind::OpenAiChatCompletions => {
                        let url = format!(
                            "{}/chat/completions",
                            config.api_base_url.trim_end_matches('/')
                        );
                        client
                            .post(&url)
                            .bearer_auth(&config.api_key)
                            .json(&request)
                            .send()?
                    }
                    LlmApiKind::AnthropicMessages => {
                        let url = format!(
                            "{}/messages?beta=true",
                            config.api_base_url.trim_end_matches('/')
                        );
                        let anthropic_request = anthropic_compat::build_request(
                            &model,
                            &request.messages,
                            None,
                            None,
                            request.temperature,
                        );
                        client
                            .post(&url)
                            .bearer_auth(&config.api_key)
                            .header("x-api-key", &config.api_key)
                            .header("anthropic-version", "2023-06-01")
                            .header(
                                "anthropic-beta",
                                "claude-code-20250219,interleaved-thinking-2025-05-14",
                            )
                            .header("anthropic-dangerous-direct-browser-access", "true")
                            .header("x-app", "cli")
                            .json(&anthropic_request)
                            .send()?
                    }
                };
                if response.status().is_success() {
                    return match config.api_kind {
                        LlmApiKind::OpenAiChatCompletions => {
                            let parsed: ChatResponse = response.json()?;
                            parsed
                                .choices
                                .into_iter()
                                .next()
                                .and_then(|choice| choice.message.content)
                                .ok_or_else(|| {
                                    anyhow::anyhow!("surface collector returned no content")
                                })
                        }
                        LlmApiKind::AnthropicMessages => {
                            let parsed: anthropic_compat::AnthropicResponse = response.json()?;
                            anthropic_compat::parse_response(parsed)
                                .content
                                .ok_or_else(|| {
                                    anyhow::anyhow!("surface collector returned no content")
                                })
                        }
                    };
                }
                let status = response.status();
                let body = response.text().unwrap_or_default();
                last_error = Some(format!(
                    "ui surface request failed with model {} and status {}: {}",
                    model, status, body
                ));
                if !is_model_unsupported_error(status, &body) {
                    anyhow::bail!(
                        "ui surface request failed with model {} and status {}: {}",
                        model,
                        status,
                        body
                    );
                }
            }
            anyhow::bail!(
                "{}",
                last_error.unwrap_or_else(
                    || "ui surface request failed without provider response".to_string()
                )
            )
        })
        .join()
        .map_err(|_| anyhow::anyhow!("ui surface worker thread panicked"))??;

        let json_text = extract_json_object(&content)
            .ok_or_else(|| anyhow::anyhow!("surface collector did not return valid json"))?;
        let mut surface: UiSurface = serde_json::from_str(json_text)?;
        surface.generated_at = now_secs_f64();
        surface.source_fingerprint = model_fingerprint(model)?;
        normalize_surface(&mut surface, &fallback);
        validate_surface(&surface, model)?;
        self.save(&surface)?;
        Ok(surface)
    }

    pub fn save(&self, surface: &UiSurface) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(surface)?)?;
        Ok(())
    }
}

fn default_ui_prompt() -> &'static str {
    r#"You are Rustpilot's UI Agent.

Responsibilities:
- Read `ui_surface.json`
- Read `system_model.json`
- Design page structure and copy within backend protocol constraints
- Produce `UiSchema` as a cacheable UI planning artifact

Constraints:
- Do not invent unsupported endpoints, events, actions, or data sources
- Do not remove required core sections such as `metrics`, `residents`, and `composer`
- If alerts exist, alerts must still be represented
- Prefer operational clarity over decorative polish
- Represent system workflow and collaboration state, not just a single agent
- Treat `chat_ui.main_friend`, `chat_ui.group_chat`, `chat_ui.agent_details`, `chat_ui.process_tree`, and `chat_ui.launches` as first-class inputs
- The generated UI must show process hierarchy and launch controls when those contracts are present
- Do not push the real product interface back into Rust bootstrap HTML

Evolution goals:
- Adapt page structure when system capabilities change
- Respond to updated prompts without breaking protocol constraints
- Keep generated UI artifacts stable and comparable across revisions
"#
}

fn default_planner_prompt() -> &'static str {
    r#"You are Rustpilot's UI surface planner.

Responsibilities:
- Inspect `system_model.json`
- Collect visible capabilities, roles, data sources, and allowed actions
- Persist a stable `ui_surface.json` that later UI generation can build from

Constraints:
- Do not generate final page code
- Do not invent protocols, interfaces, events, roles, or data sources
- Reflect system workflow, not just one agent's point of view
- Keep the output structured, cacheable, and easy to audit
- Preserve chat/process-tree/launch oriented sections when the backend exposes them

Evolution goals:
- Adjust pages, actions, and supported sections when capabilities change
- Keep the surface spec aligned with protocol changes
- Provide stable planning input for downstream schema and page generation
"#
}

fn with_managed_ui_prompt_appendix(base: &str) -> String {
    let appendix = "\n\nManaged appendix:\n- Treat chat/process-tree/launch contracts as first-class UI inputs when present in the backend model.\n- Keep the generated interface in the UI agent output; do not push the real product UI back into Rust bootstrap HTML.\n";
    if base.contains("Managed appendix:") {
        base.to_string()
    } else {
        format!("{}{}", base.trim_end(), appendix)
    }
}

fn with_managed_planner_prompt_appendix(base: &str) -> String {
    let appendix = "\n\nManaged appendix:\n- Preserve supported sections and data sources for main chat, group chat, agent details, process tree, and launches when the backend exposes them.\n- Plan for a generated UI product surface, not a Rust-owned fallback dashboard.\n";
    if base.contains("Managed appendix:") {
        base.to_string()
    } else {
        format!("{}{}", base.trim_end(), appendix)
    }
}

fn model_fingerprint(model: &SystemModel) -> anyhow::Result<String> {
    let serialized = serde_json::to_string(model)?;
    hash_string(&serialized)
}

fn hash_string(input: &str) -> anyhow::Result<String> {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    Ok(format!("{:x}", hasher.finish()))
}

fn normalize_surface(surface: &mut UiSurface, fallback: &UiSurface) {
    if surface.title.trim().is_empty() {
        surface.title = fallback.title.clone();
    }
    if surface.summary.trim().is_empty() {
        surface.summary = fallback.summary.clone();
    }
    if surface.pages.is_empty() {
        surface.pages = fallback.pages.clone();
    }
}

fn apply_surface_view_bias(surface: &mut UiSurface, desired_view: &str) {
    surface.title = format!("Rustpilot surface ({})", desired_view);
    surface.summary = surface_summary_for_view(desired_view).to_string();
    if let Some(page) = surface.pages.first_mut() {
        page.title = surface_title_for_view(desired_view).to_string();
        page.purpose = surface_purpose_for_view(desired_view).to_string();
        page.audience = surface_audience_for_view(desired_view).to_string();
        reorder_surface_items_for_view(&mut page.data_sources, desired_view);
        reorder_surface_items_for_view(&mut page.supported_sections, desired_view);
    }
}

fn reorder_surface_items_for_view(items: &mut [String], desired_view: &str) {
    let priority = match desired_view {
        "task_board" => [
            "tasks",
            "summary",
            "alerts",
            "residents",
            "proposals",
            "decisions",
            "composer",
            "metrics",
        ],
        "session_console" => [
            "composer",
            "summary",
            "residents",
            "alerts",
            "tasks",
            "proposals",
            "decisions",
            "metrics",
        ],
        "approval_overview" => [
            "alerts",
            "decisions",
            "proposals",
            "summary",
            "residents",
            "tasks",
            "composer",
            "metrics",
        ],
        "resident_monitor" => [
            "residents",
            "alerts",
            "summary",
            "tasks",
            "composer",
            "proposals",
            "decisions",
            "metrics",
        ],
        _ => [
            "summary",
            "alerts",
            "residents",
            "tasks",
            "proposals",
            "decisions",
            "composer",
            "metrics",
        ],
    };
    items.sort_by_key(|item| {
        priority
            .iter()
            .position(|priority_item| *priority_item == item)
            .unwrap_or(priority.len())
    });
}

fn surface_title_for_view(desired_view: &str) -> &'static str {
    match desired_view {
        "task_board" => "Task Board",
        "session_console" => "Session Console",
        "approval_overview" => "Approval Overview",
        "resident_monitor" => "Resident Monitor",
        _ => "System Overview",
    }
}

fn surface_purpose_for_view(desired_view: &str) -> &'static str {
    match desired_view {
        "task_board" => "Focus the UI plan on queued work, blockers, and dispatch actions.",
        "session_console" => "Focus the UI plan on sessions, routing, and live operator actions.",
        "approval_overview" => {
            "Focus the UI plan on policy state, approval blocks, and governance context."
        }
        "resident_monitor" => {
            "Focus the UI plan on resident health, backlog, and runtime visibility."
        }
        _ => "Provide an overview of current system state and operator actions.",
    }
}

fn surface_audience_for_view(desired_view: &str) -> &'static str {
    match desired_view {
        "task_board" => "operator, dispatcher, reviewer",
        "session_console" => "operator, support, debugger",
        "approval_overview" => "operator, reviewer, governance owner",
        "resident_monitor" => "operator, maintainer, observer",
        _ => "operator, dispatcher, observer",
    }
}

fn surface_summary_for_view(desired_view: &str) -> &'static str {
    match desired_view {
        "task_board" => "Surface plan biased toward task flow, blockers, and work dispatch.",
        "session_console" => {
            "Surface plan biased toward sessions, routing state, and interaction controls."
        }
        "approval_overview" => {
            "Surface plan biased toward approval state, policy context, and decisions."
        }
        "resident_monitor" => "Surface plan biased toward resident runtime state and health.",
        _ => "Surface plan for the current project state and operational control.",
    }
}

fn validate_surface(surface: &UiSurface, model: &SystemModel) -> anyhow::Result<()> {
    if surface.title.trim().is_empty() {
        anyhow::bail!("ui surface title cannot be empty");
    }
    if surface.pages.is_empty() {
        anyhow::bail!("ui surface must contain at least one page");
    }

    let supported_sections = model
        .protocols
        .iter()
        .flat_map(|item| item.supported_sections.iter().cloned())
        .collect::<std::collections::HashSet<_>>();
    let supported_sources = model
        .protocols
        .iter()
        .flat_map(|item| item.supported_sources.iter().cloned())
        .collect::<std::collections::HashSet<_>>();
    let protocol_map = model
        .protocols
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<std::collections::HashMap<_, _>>();

    for page in &surface.pages {
        if page.id.trim().is_empty() || page.title.trim().is_empty() {
            anyhow::bail!("ui surface page id/title cannot be empty");
        }
        for section in &page.supported_sections {
            if !supported_sections.contains(section) {
                anyhow::bail!(
                    "ui surface page '{}' uses unsupported section '{}'",
                    page.id,
                    section
                );
            }
        }
        for source in &page.data_sources {
            if !supported_sources.contains(source) {
                anyhow::bail!(
                    "ui surface page '{}' uses unsupported source '{}'",
                    page.id,
                    source
                );
            }
        }
        for action in &page.actions {
            let protocol = protocol_map
                .get(action.protocol_id.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "ui surface action '{}' references unknown protocol '{}'",
                        action.id,
                        action.protocol_id
                    )
                })?;
            if !protocol
                .targets
                .iter()
                .any(|target| target == &action.target)
            {
                anyhow::bail!(
                    "ui surface action '{}' target '{}' is not allowed by protocol '{}'",
                    action.id,
                    action.target,
                    action.protocol_id
                );
            }
        }
    }

    Ok(())
}

fn extract_json_object(text: &str) -> Option<&str> {
    let fenced = text.trim();
    if fenced.starts_with("```") {
        let stripped = fenced
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        if stripped.starts_with('{') && stripped.ends_with('}') {
            return Some(stripped);
        }
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    text.get(start..=end)
}

#[cfg(test)]
mod tests {
    use super::UiSurfaceManager;
    use crate::project_tools::{SystemModel, SystemProtocol, SystemSummary};

    #[test]
    fn rebuild_from_model_includes_chat_tree_and_launch_contracts() {
        let temp_dir = std::env::temp_dir().join(format!(
            "rustpilot-ui-surface-test-{}-{}",
            std::process::id(),
            crate::project_tools::util::now_secs_f64()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let manager = UiSurfaceManager::new(temp_dir.clone()).expect("manager");
        let model = SystemModel {
            generated_at: 0.0,
            summary: SystemSummary {
                launch_mode: "multi_window".to_string(),
                launch_mode_description: "each launch opens a dedicated visible OS window"
                    .to_string(),
                launch_effective_mode: "multi_window".to_string(),
                launch_backend: "windows_start_process".to_string(),
                launch_backend_note:
                    "visible windows are launched through Start-Process cmd.exe hosts"
                        .to_string(),
                resident_count: 1,
                pending_tasks: 0,
                running_tasks: 1,
                blocked_tasks: 0,
                completed_tasks: 0,
                open_proposals: 0,
                recent_decisions: 0,
            },
            alerts: Vec::new(),
            protocols: vec![SystemProtocol {
                id: "ui.status".to_string(),
                transport: "http".to_string(),
                method: "GET".to_string(),
                path: "/api/status".to_string(),
                purpose: "status".to_string(),
                readonly: true,
                requires_confirmation: false,
                targets: Vec::new(),
                request_fields: Vec::new(),
                response_fields: Vec::new(),
                supported_sections: vec![
                    "metrics".to_string(),
                    "residents".to_string(),
                    "main_chat".to_string(),
                    "group_chat".to_string(),
                    "agent_details".to_string(),
                    "process_tree".to_string(),
                    "launches".to_string(),
                ],
                supported_sources: vec![
                    "summary".to_string(),
                    "residents".to_string(),
                    "main_friend".to_string(),
                    "group_chat".to_string(),
                    "agent_details".to_string(),
                    "process_tree".to_string(),
                    "launches".to_string(),
                ],
                event_types: Vec::new(),
            }],
            residents: Vec::new(),
            launches: Vec::new(),
            recent_prompt_changes: Vec::new(),
            tasks: Vec::new(),
            proposals: Vec::new(),
            decisions: Vec::new(),
        };

        let surface = manager
            .rebuild_from_model(&model, "project_state")
            .expect("surface");
        let page = surface.pages.first().expect("page");
        for source in [
            "main_friend",
            "group_chat",
            "agent_details",
            "process_tree",
            "launches",
        ] {
            assert!(page.data_sources.iter().any(|item| item == source));
        }
        for section in [
            "main_chat",
            "group_chat",
            "agent_details",
            "process_tree",
            "launches",
        ] {
            assert!(page.supported_sections.iter().any(|item| item == section));
        }

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn prompt_text_appends_managed_ui_appendix() {
        let temp_dir = std::env::temp_dir().join(format!(
            "rustpilot-ui-surface-prompt-test-{}-{}",
            std::process::id(),
            crate::project_tools::util::now_secs_f64()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let manager = UiSurfaceManager::new(temp_dir.clone()).expect("manager");
        std::fs::write(temp_dir.join("ui_agent_prompt.md"), "short prompt").expect("write");
        let prompt = manager.prompt_text().expect("prompt");
        assert!(prompt.contains("Managed appendix:"));
        assert!(prompt.contains("chat/process-tree/launch"));
        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
