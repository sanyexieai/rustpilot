use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::system_model::SystemModel;
use super::ui_surface::UiSurface;
use crate::anthropic_compat;
use crate::config::{LlmConfig, default_llm_user_agent, is_model_unsupported_error};
use crate::llm_profiles::LlmApiKind;
use crate::openai_compat::{ChatRequest, ChatResponse, Message};
use crate::project_tools::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSchema {
    pub generated_at: f64,
    #[serde(default)]
    pub source_fingerprint: String,
    pub title: String,
    pub subtitle: String,
    #[serde(default = "default_theme_name")]
    pub theme_name: String,
    pub sections: Vec<UiSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSection {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub source: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub empty_text: String,
    #[serde(default)]
    pub columns: usize,
    #[serde(default, deserialize_with = "deserialize_target_options")]
    pub target_options: Vec<UiOption>,
    #[serde(default)]
    pub labels: Vec<UiLabel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiLabel {
    pub key: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct UiSchemaManager {
    path: PathBuf,
}

impl UiSchemaManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("ui_schema.json"),
        })
    }

    pub fn snapshot(&self) -> anyhow::Result<Option<UiSchema>> {
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
            .ok_or_else(|| anyhow::anyhow!("failed to resolve repo root for ui schema manager"))
    }

    pub fn needs_refresh(&self, fingerprint: &str) -> anyhow::Result<bool> {
        let existing = self.snapshot()?;
        Ok(existing
            .map(|item| item.source_fingerprint != fingerprint)
            .unwrap_or(true))
    }

    pub fn generate_from_surface(
        &self,
        model: &SystemModel,
        surface: &UiSurface,
        desired_view: &str,
        fingerprint: &str,
    ) -> anyhow::Result<UiSchema> {
        let page = surface
            .pages
            .first()
            .ok_or_else(|| anyhow::anyhow!("ui surface must contain at least one page"))?;
        let mut sections = vec![UiSection {
            id: "summary".to_string(),
            title: "metrics".to_string(),
            kind: "metrics".to_string(),
            source: "summary".to_string(),
            description: String::new(),
            empty_text: String::new(),
            columns: 6,
            target_options: Vec::new(),
            labels: Vec::new(),
        }];

        if page.supported_sections.iter().any(|item| item == "alerts") && !model.alerts.is_empty() {
            sections.push(UiSection {
                id: "alerts".to_string(),
                title: "alerts".to_string(),
                kind: "alerts".to_string(),
                source: "alerts".to_string(),
                description: String::new(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "main_chat")
        {
            sections.push(UiSection {
                id: "main-chat".to_string(),
                title: "main".to_string(),
                kind: "main_chat".to_string(),
                source: "main_friend".to_string(),
                description: "Primary direct conversation for the current root actor.".to_string(),
                empty_text: String::new(),
                columns: 2,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "group_chat")
        {
            sections.push(UiSection {
                id: "group-chat".to_string(),
                title: "agent team".to_string(),
                kind: "group_chat".to_string(),
                source: "group_chat".to_string(),
                description: "Shared team thread and coordination timeline.".to_string(),
                empty_text: String::new(),
                columns: 2,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "agent_details")
        {
            sections.push(UiSection {
                id: "agent-details".to_string(),
                title: "agent detail".to_string(),
                kind: "agent_details".to_string(),
                source: "agent_details".to_string(),
                description: "Selected agent transcript and runtime detail.".to_string(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "process_tree")
        {
            sections.push(UiSection {
                id: "process-tree".to_string(),
                title: "process tree".to_string(),
                kind: "process_tree".to_string(),
                source: "process_tree".to_string(),
                description: "Parent-child execution hierarchy for agents, tasks, and launches."
                    .to_string(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "launches")
        {
            sections.push(UiSection {
                id: "launches".to_string(),
                title: "launches".to_string(),
                kind: "launches".to_string(),
                source: "launches".to_string(),
                description: "Live launch registry with per-window lifecycle controls.".to_string(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "residents")
        {
            sections.push(UiSection {
                id: "residents".to_string(),
                title: "residents".to_string(),
                kind: "residents".to_string(),
                source: "residents".to_string(),
                description: String::new(),
                empty_text: String::new(),
                columns: if model.residents.len() > 1 { 2 } else { 1 },
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "composer")
        {
            sections.push(UiSection {
                id: "composer".to_string(),
                title: "composer".to_string(),
                kind: "composer".to_string(),
                source: "composer".to_string(),
                description: String::new(),
                empty_text: String::new(),
                columns: 1,
                target_options: page
                    .actions
                    .iter()
                    .map(|item| UiOption {
                        value: item.target.clone(),
                        label: item.target.clone(),
                    })
                    .collect(),
                labels: Vec::new(),
            });
        }

        if page.supported_sections.iter().any(|item| item == "tasks") && !model.tasks.is_empty() {
            sections.push(UiSection {
                id: "tasks".to_string(),
                title: "tasks".to_string(),
                kind: "tasks".to_string(),
                source: "tasks".to_string(),
                description: String::new(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "proposals")
            && !model.proposals.is_empty()
        {
            sections.push(UiSection {
                id: "proposals".to_string(),
                title: "proposals".to_string(),
                kind: "proposals".to_string(),
                source: "proposals".to_string(),
                description: String::new(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        if page
            .supported_sections
            .iter()
            .any(|item| item == "decisions")
            && !model.decisions.is_empty()
        {
            sections.push(UiSection {
                id: "decisions".to_string(),
                title: "decisions".to_string(),
                kind: "decisions".to_string(),
                source: "decisions".to_string(),
                description: String::new(),
                empty_text: String::new(),
                columns: 1,
                target_options: Vec::new(),
                labels: Vec::new(),
            });
        }

        reorder_sections_for_view(&mut sections, desired_view);
        let schema = UiSchema {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint.to_string(),
            title: schema_title_for_view(desired_view, &page.title),
            subtitle: schema_subtitle_for_view(desired_view, &page.purpose),
            theme_name: "paper-console".to_string(),
            sections,
        };
        self.save(&schema)?;
        Ok(schema)
    }

    pub fn generate_with_ui_agent(
        &self,
        model: &SystemModel,
        surface: &UiSurface,
        prompt_text: &str,
        desired_view: &str,
        fingerprint: &str,
    ) -> anyhow::Result<UiSchema> {
        let fallback = self.generate_from_surface(model, surface, desired_view, fingerprint)?;
        let model_json = serde_json::to_string_pretty(model)?;
        let surface_json = serde_json::to_string_pretty(surface)?;
        let fallback_json = serde_json::to_string_pretty(&fallback)?;
        let repo_root = self.repo_root()?;
        let prompt = format!(
            "You are Rustpilot's UI Agent. Your inputs are the system model, the persisted ui_surface specification, and an evolvable prompt maintained on disk.\n\
Return exactly one JSON object. Do not output explanations, Markdown, or code fences.\n\
\n\
Prompt text:\n{prompt_text}\n\
\n\
Desired view:\n{desired_view}\n\
\n\
Rules:\n\
1. The page structure must follow ui_surface first, not free-form invention.\n\
2. You may only use sections, sources, and targets that are supported by backend protocols.\n\
3. Do not invent pages, APIs, events, actions, or data sources.\n\
4. Required sections metrics, residents, and composer must remain. If alerts exist, alerts must appear.\n\
5. When supported by backend protocols, preserve chat-oriented sections for main chat, group chat, agent details, process tree, and launches.\n\
6. You may optimize title, subtitle, section order, descriptions, empty states, labels, target_options, columns, and theme_name.\n\
7. Any language or copy preference must come from the prompt text and surface spec, not from hardcoded defaults.\n\
8. The goal is to evolve UI expression when system capability or prompt text changes, without breaking protocol constraints.\n\
9. The desired view should influence section emphasis and ordering.\n\
\n\
System model:\n{model_json}\n\
\n\
UI surface spec:\n{surface_json}\n\
\n\
Fallback schema:\n{fallback_json}\n\
\n\
Now output the final JSON:",
            desired_view = desired_view
        );

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
                        "You are a protocol-constrained UI schema generator. Output valid JSON only and do not invent unsupported capabilities."
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
                                .ok_or_else(|| anyhow::anyhow!("ui agent returned no schema content"))
                        }
                        LlmApiKind::AnthropicMessages => {
                            let parsed: anthropic_compat::AnthropicResponse = response.json()?;
                            anthropic_compat::parse_response(parsed)
                                .content
                                .ok_or_else(|| anyhow::anyhow!("ui agent returned no schema content"))
                        }
                    };
                }
                let status = response.status();
                let body = response.text().unwrap_or_default();
                last_error = Some(format!(
                    "ui agent schema request failed with model {} and status {}: {}",
                    model, status, body
                ));
                if !is_model_unsupported_error(status, &body) {
                    anyhow::bail!(
                        "ui agent schema request failed with model {} and status {}: {}",
                        model,
                        status,
                        body
                    );
                }
            }
            anyhow::bail!(
                "{}",
                last_error.unwrap_or_else(|| "ui agent schema request failed without provider response".to_string())
            )
        })
        .join()
        .map_err(|_| anyhow::anyhow!("ui agent schema worker thread panicked"))??;

        let json_text = extract_json_object(&content)
            .ok_or_else(|| anyhow::anyhow!("ui agent did not return valid json object"))?;
        let mut schema: UiSchema = serde_json::from_str(json_text)?;
        schema.generated_at = now_secs_f64();
        schema.source_fingerprint = fingerprint.to_string();
        normalize_schema(&mut schema, &fallback);
        validate_schema(&schema, model)?;
        self.save(&schema)?;
        Ok(schema)
    }

    pub fn save(&self, schema: &UiSchema) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(schema)?)?;
        Ok(())
    }
}

fn default_theme_name() -> String {
    "paper-console".to_string()
}

fn deserialize_target_options<'de, D>(deserializer: D) -> Result<Vec<UiOption>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawOption {
        Text(String),
        Full(UiOption),
    }

    let raw = Vec::<RawOption>::deserialize(deserializer)?;
    Ok(raw
        .into_iter()
        .map(|item| match item {
            RawOption::Text(value) => UiOption {
                value: value.clone(),
                label: value,
            },
            RawOption::Full(option) => option,
        })
        .collect())
}

fn normalize_schema(schema: &mut UiSchema, fallback: &UiSchema) {
    if schema.title.trim().is_empty() {
        schema.title = fallback.title.clone();
    }
    if schema.subtitle.trim().is_empty() {
        schema.subtitle = fallback.subtitle.clone();
    }
    if schema.theme_name.trim().is_empty() {
        schema.theme_name = fallback.theme_name.clone();
    }
    if schema.sections.is_empty() {
        schema.sections = fallback.sections.clone();
    }
}

fn reorder_sections_for_view(sections: &mut [UiSection], desired_view: &str) {
    let priority = match desired_view {
        "task_board" => [
            "tasks",
            "summary",
            "composer",
            "alerts",
            "residents",
            "proposals",
            "decisions",
        ],
        "session_console" => [
            "composer",
            "summary",
            "residents",
            "alerts",
            "tasks",
            "proposals",
            "decisions",
        ],
        "approval_overview" => [
            "alerts",
            "summary",
            "decisions",
            "proposals",
            "residents",
            "tasks",
            "composer",
        ],
        "resident_monitor" => [
            "residents",
            "alerts",
            "summary",
            "tasks",
            "composer",
            "proposals",
            "decisions",
        ],
        _ => [
            "summary",
            "alerts",
            "residents",
            "tasks",
            "composer",
            "proposals",
            "decisions",
        ],
    };

    sections.sort_by_key(|section| {
        priority
            .iter()
            .position(|item| *item == section.id || *item == section.kind)
            .unwrap_or(priority.len())
    });
}

fn schema_title_for_view(desired_view: &str, default_title: &str) -> String {
    match desired_view {
        "task_board" => "Task Board".to_string(),
        "session_console" => "Session Console".to_string(),
        "approval_overview" => "Approval Overview".to_string(),
        "resident_monitor" => "Resident Monitor".to_string(),
        _ => default_title.to_string(),
    }
}

fn schema_subtitle_for_view(desired_view: &str, default_subtitle: &str) -> String {
    match desired_view {
        "task_board" => "Focus on queued work, blockers, and dispatch flow.".to_string(),
        "session_console" => {
            "Focus on sessions, focus routing, and current interaction state.".to_string()
        }
        "approval_overview" => {
            "Focus on policy state, approval blocks, and governance decisions.".to_string()
        }
        "resident_monitor" => {
            "Focus on resident health, backlog, and runtime activity.".to_string()
        }
        _ => default_subtitle.to_string(),
    }
}

fn validate_schema(schema: &UiSchema, model: &SystemModel) -> anyhow::Result<()> {
    if schema.title.trim().is_empty() {
        anyhow::bail!("ui schema title cannot be empty");
    }
    if schema.subtitle.trim().is_empty() {
        anyhow::bail!("ui schema subtitle cannot be empty");
    }

    let allowed_kinds = HashSet::from([
        "metrics",
        "alerts",
        "residents",
        "main_chat",
        "group_chat",
        "agent_details",
        "process_tree",
        "launches",
        "composer",
        "tasks",
        "proposals",
        "decisions",
    ]);
    let allowed_sources = HashSet::from([
        "summary",
        "alerts",
        "residents",
        "main_friend",
        "group_chat",
        "agent_details",
        "process_tree",
        "launches",
        "composer",
        "tasks",
        "proposals",
        "decisions",
    ]);
    let section_support = model
        .protocols
        .iter()
        .flat_map(|item| {
            item.supported_sections
                .iter()
                .cloned()
                .map(|section| (section, item.readonly))
                .collect::<Vec<_>>()
        })
        .fold(
            HashMap::<String, bool>::new(),
            |mut acc, (section, readonly)| {
                let entry = acc.entry(section).or_insert(false);
                *entry |= readonly;
                acc
            },
        );
    let source_support = model
        .protocols
        .iter()
        .flat_map(|item| item.supported_sources.iter().cloned())
        .collect::<HashSet<_>>();
    let composer_protocols = model
        .protocols
        .iter()
        .filter(|item| {
            !item.readonly
                && item
                    .supported_sections
                    .iter()
                    .any(|section| section == "composer")
        })
        .collect::<Vec<_>>();
    let protocol_targets = composer_protocols
        .iter()
        .flat_map(|item| item.targets.iter().cloned())
        .collect::<HashSet<_>>();
    let composer_writable = !composer_protocols.is_empty();

    let mut ids = HashSet::new();
    let mut has_summary = false;
    let mut has_residents = false;
    let mut has_composer = false;
    let mut has_alerts = false;
    let chat_contract_available = source_support.contains("main_friend")
        && source_support.contains("group_chat")
        && source_support.contains("agent_details");
    let process_tree_available = source_support.contains("process_tree");
    let launches_available = source_support.contains("launches");
    let mut has_main_chat = false;
    let mut has_group_chat = false;
    let mut has_agent_details = false;
    let mut has_process_tree = false;
    let mut has_launches = false;

    for section in &schema.sections {
        if !ids.insert(section.id.clone()) {
            anyhow::bail!("duplicate ui section id '{}'", section.id);
        }
        if !allowed_kinds.contains(section.kind.as_str()) {
            anyhow::bail!("unsupported ui section kind '{}'", section.kind);
        }
        if !allowed_sources.contains(section.source.as_str()) {
            anyhow::bail!("unsupported ui section source '{}'", section.source);
        }
        if section.columns > 6 {
            anyhow::bail!("ui section '{}' columns out of range", section.id);
        }
        if !section_support.contains_key(section.kind.as_str()) {
            anyhow::bail!(
                "ui section '{}' kind '{}' is not backed by any backend protocol",
                section.id,
                section.kind
            );
        }
        if !source_support.contains(section.source.as_str()) {
            anyhow::bail!(
                "ui section '{}' source '{}' is not backed by any backend protocol",
                section.id,
                section.source
            );
        }

        match section.kind.as_str() {
            "metrics" => has_summary = true,
            "residents" => has_residents = true,
            "main_chat" => has_main_chat = true,
            "group_chat" => has_group_chat = true,
            "agent_details" => has_agent_details = true,
            "process_tree" => has_process_tree = true,
            "launches" => has_launches = true,
            "composer" => {
                has_composer = true;
                if !composer_writable {
                    anyhow::bail!(
                        "ui schema cannot expose composer without writable dispatch protocol"
                    );
                }
                for option in &section.target_options {
                    if !protocol_targets.contains(&option.value) {
                        anyhow::bail!(
                            "composer target '{}' is not supported by backend protocol",
                            option.value
                        );
                    }
                }
            }
            "alerts" => has_alerts = true,
            _ => {}
        }
    }

    if !has_summary || !has_residents || !has_composer {
        anyhow::bail!("ui schema must include metrics, residents, and composer sections");
    }
    if !model.alerts.is_empty() && !has_alerts {
        anyhow::bail!("ui schema must include alerts section when alerts exist");
    }
    if chat_contract_available && (!has_main_chat || !has_group_chat || !has_agent_details) {
        anyhow::bail!(
            "ui schema must include main_chat, group_chat, and agent_details when chat contracts are available"
        );
    }
    if process_tree_available && !has_process_tree {
        anyhow::bail!("ui schema must include process_tree when process tree data is available");
    }
    if launches_available && !has_launches {
        anyhow::bail!("ui schema must include launches when launch data is available");
    }
    for required in ["metrics", "residents"] {
        if !section_support.get(required).copied().unwrap_or(false) {
            anyhow::bail!(
                "backend protocol contract must expose readonly support for required section '{}'",
                required
            );
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
    use super::UiSchemaManager;
    use crate::project_tools::{
        SystemLaunch, SystemModel, SystemSummary, UiAction, UiSurface, UiSurfacePage,
    };

    #[test]
    fn generate_from_surface_respects_desired_view() {
        let temp_dir = std::env::temp_dir().join(format!(
            "rustpilot-ui-schema-test-{}-{}",
            std::process::id(),
            crate::project_tools::util::now_secs_f64()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let manager = UiSchemaManager::new(temp_dir.clone()).expect("manager");
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
                pending_tasks: 1,
                running_tasks: 0,
                blocked_tasks: 0,
                completed_tasks: 0,
                open_proposals: 0,
                recent_decisions: 0,
            },
            alerts: Vec::new(),
            protocols: vec![
                crate::project_tools::SystemProtocol {
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
                        "main_chat".to_string(),
                        "group_chat".to_string(),
                        "agent_details".to_string(),
                        "process_tree".to_string(),
                        "launches".to_string(),
                        "residents".to_string(),
                        "tasks".to_string(),
                    ],
                    supported_sources: vec![
                        "summary".to_string(),
                        "main_friend".to_string(),
                        "group_chat".to_string(),
                        "agent_details".to_string(),
                        "process_tree".to_string(),
                        "launches".to_string(),
                        "residents".to_string(),
                        "tasks".to_string(),
                    ],
                    event_types: Vec::new(),
                },
                crate::project_tools::SystemProtocol {
                    id: "ui.wire.dispatch".to_string(),
                    transport: "http".to_string(),
                    method: "POST".to_string(),
                    path: "/api/wire".to_string(),
                    purpose: "dispatch".to_string(),
                    readonly: false,
                    requires_confirmation: false,
                    targets: vec!["ui".to_string()],
                    request_fields: Vec::new(),
                    response_fields: Vec::new(),
                    supported_sections: vec!["composer".to_string()],
                    supported_sources: vec!["composer".to_string()],
                    event_types: Vec::new(),
                },
            ],
            launches: Vec::new(),
            residents: Vec::new(),
            recent_prompt_changes: Vec::new(),
            tasks: vec![crate::project_tools::SystemTask {
                id: 1,
                subject: "test".to_string(),
                status: "pending".to_string(),
                priority: "medium".to_string(),
                role: "developer".to_string(),
                owner: "lead".to_string(),
                parent_task_id: None,
                depth: 0,
            }],
            proposals: Vec::new(),
            decisions: Vec::new(),
        };
        let surface = UiSurface {
            generated_at: 0.0,
            source_fingerprint: "surface".to_string(),
            title: "System Overview".to_string(),
            summary: "summary".to_string(),
            pages: vec![UiSurfacePage {
                id: "system-overview".to_string(),
                title: "System Overview".to_string(),
                purpose: "Operate the system".to_string(),
                audience: "operator".to_string(),
                data_sources: vec![
                    "summary".to_string(),
                    "main_friend".to_string(),
                    "group_chat".to_string(),
                    "agent_details".to_string(),
                    "process_tree".to_string(),
                    "launches".to_string(),
                    "residents".to_string(),
                    "tasks".to_string(),
                ],
                supported_sections: vec![
                    "metrics".to_string(),
                    "main_chat".to_string(),
                    "group_chat".to_string(),
                    "agent_details".to_string(),
                    "process_tree".to_string(),
                    "launches".to_string(),
                    "residents".to_string(),
                    "composer".to_string(),
                    "tasks".to_string(),
                ],
                actions: vec![UiAction {
                    id: "dispatch-ui".to_string(),
                    title: "Dispatch".to_string(),
                    protocol_id: "ui.wire.dispatch".to_string(),
                    target: "ui".to_string(),
                    description: "Send request".to_string(),
                }],
                notes: Vec::new(),
            }],
        };

        let schema = manager
            .generate_from_surface(&model, &surface, "task_board", "fp")
            .expect("schema");

        assert_eq!(schema.title, "Task Board");
        assert_eq!(
            schema.sections.first().map(|item| item.id.as_str()),
            Some("tasks")
        );
        assert!(schema.sections.iter().any(|item| item.kind == "main_chat"));
        assert!(schema.sections.iter().any(|item| item.kind == "group_chat"));
        assert!(schema.sections.iter().any(|item| item.kind == "agent_details"));
        assert!(schema.sections.iter().any(|item| item.kind == "process_tree"));
        assert!(schema.sections.iter().any(|item| item.kind == "launches"));

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn fallback_schema_preserves_chat_and_tree_sections_when_supported() {
        let temp_dir = std::env::temp_dir().join(format!(
            "rustpilot-ui-schema-chat-test-{}-{}",
            std::process::id(),
            crate::project_tools::util::now_secs_f64()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let manager = UiSchemaManager::new(temp_dir.clone()).expect("manager");
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
            protocols: vec![
                crate::project_tools::SystemProtocol {
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
                },
                crate::project_tools::SystemProtocol {
                    id: "ui.wire.dispatch".to_string(),
                    transport: "http".to_string(),
                    method: "POST".to_string(),
                    path: "/api/wire".to_string(),
                    purpose: "write".to_string(),
                    readonly: false,
                    requires_confirmation: false,
                    targets: vec!["ui".to_string()],
                    request_fields: Vec::new(),
                    response_fields: Vec::new(),
                    supported_sections: vec!["composer".to_string()],
                    supported_sources: vec!["composer".to_string()],
                    event_types: Vec::new(),
                },
            ],
            launches: vec![SystemLaunch {
                launch_id: "launch-1".to_string(),
                agent_id: "ui".to_string(),
                owner: "ui".to_string(),
                role: "ui".to_string(),
                kind: "resident".to_string(),
                status: "running".to_string(),
                pid: Some(42),
                process_started_at: Some(1_700_000_000.0),
                task_id: None,
                parent_task_id: None,
                parent_agent_id: Some("root".to_string()),
                channel: "launch".to_string(),
                target: "window".to_string(),
                window_title: "UI".to_string(),
                log_path: "launch-1.log".to_string(),
                error: String::new(),
            }],
            residents: Vec::new(),
            recent_prompt_changes: Vec::new(),
            tasks: Vec::new(),
            proposals: Vec::new(),
            decisions: Vec::new(),
        };
        let surface = UiSurface {
            generated_at: 0.0,
            source_fingerprint: "surface".to_string(),
            title: "Surface".to_string(),
            summary: "Summary".to_string(),
            pages: vec![UiSurfacePage {
                id: "system-overview".to_string(),
                title: "System Overview".to_string(),
                purpose: "Operate".to_string(),
                audience: "operator".to_string(),
                data_sources: vec![
                    "summary".to_string(),
                    "residents".to_string(),
                    "main_friend".to_string(),
                    "group_chat".to_string(),
                    "agent_details".to_string(),
                    "process_tree".to_string(),
                    "launches".to_string(),
                ],
                supported_sections: vec![
                    "metrics".to_string(),
                    "residents".to_string(),
                    "main_chat".to_string(),
                    "group_chat".to_string(),
                    "agent_details".to_string(),
                    "process_tree".to_string(),
                    "launches".to_string(),
                    "composer".to_string(),
                ],
                actions: vec![UiAction {
                    id: "dispatch-ui".to_string(),
                    title: "Dispatch".to_string(),
                    protocol_id: "ui.wire.dispatch".to_string(),
                    target: "ui".to_string(),
                    description: "Send".to_string(),
                }],
                notes: Vec::new(),
            }],
        };

        let schema = manager
            .generate_from_surface(&model, &surface, "project_state", "fp")
            .expect("schema");
        let kinds = schema
            .sections
            .iter()
            .map(|item| item.kind.as_str())
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"main_chat"));
        assert!(kinds.contains(&"group_chat"));
        assert!(kinds.contains(&"agent_details"));
        assert!(kinds.contains(&"process_tree"));
        assert!(kinds.contains(&"launches"));

        let _ = std::fs::remove_dir_all(temp_dir);
    }
}
