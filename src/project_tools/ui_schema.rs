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

        let schema = UiSchema {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint.to_string(),
            title: page.title.clone(),
            subtitle: page.purpose.clone(),
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
        fingerprint: &str,
    ) -> anyhow::Result<UiSchema> {
        let fallback = self.generate_from_surface(model, surface, fingerprint)?;
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
Rules:\n\
1. The page structure must follow ui_surface first, not free-form invention.\n\
2. You may only use sections, sources, and targets that are supported by backend protocols.\n\
3. Do not invent pages, APIs, events, actions, or data sources.\n\
4. Required sections metrics, residents, and composer must remain. If alerts exist, alerts must appear.\n\
5. You may optimize title, subtitle, section order, descriptions, empty states, labels, target_options, columns, and theme_name.\n\
6. Any language or copy preference must come from the prompt text and surface spec, not from hardcoded defaults.\n\
7. The goal is to evolve UI expression when system capability or prompt text changes, without breaking protocol constraints.\n\
\n\
System model:\n{model_json}\n\
\n\
UI surface spec:\n{surface_json}\n\
\n\
Fallback schema:\n{fallback_json}\n\
\n\
Now output the final JSON:"
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
        "composer",
        "tasks",
        "proposals",
        "decisions",
    ]);
    let allowed_sources = HashSet::from([
        "summary",
        "alerts",
        "residents",
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
