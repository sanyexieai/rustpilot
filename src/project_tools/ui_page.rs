use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::system_model::SystemModel;
use super::ui_schema::UiSchema;
use super::ui_surface::UiSurface;
use crate::anthropic_compat;
use crate::config::{LlmConfig, default_llm_user_agent, is_model_unsupported_error};
use crate::llm_profiles::LlmApiKind;
use crate::openai_compat::{ChatRequest, ChatResponse, Message};
use crate::project_tools::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPageContext {
    pub generated_at: f64,
    pub source_fingerprint: String,
    pub user_goal: String,
    pub user_memory: UiUserIntentMemory,
    pub design_rules: UiDesignRules,
    pub system_model: SystemModel,
    pub ui_surface: UiSurface,
    pub ui_schema: UiSchema,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPage {
    pub generated_at: f64,
    pub source_fingerprint: String,
    pub html: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiUserIntentMemory {
    pub updated_at: f64,
    pub intent_type: String,
    pub desired_view: String,
    pub primary_request: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub operator_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiDesignRules {
    pub schema_version: u32,
    #[serde(default)]
    pub design_principles: Vec<String>,
    #[serde(default)]
    pub interaction_rules: Vec<String>,
    #[serde(default)]
    pub visual_rules: Vec<String>,
    #[serde(default)]
    pub protocol_rules: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct UiPageManager {
    page_path: PathBuf,
    context_path: PathBuf,
    prompt_path: PathBuf,
    request_path: PathBuf,
    rules_path: PathBuf,
}

impl UiPageManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        let prompt_path = dir.join("ui_page_prompt.md");
        if !prompt_path.exists() {
            fs::write(&prompt_path, default_page_prompt())?;
        }
        let request_path = dir.join("ui_page_request.json");
        if !request_path.exists() {
            fs::write(
                &request_path,
                serde_json::to_string_pretty(&default_user_request_memory())?,
            )?;
        }
        let rules_path = dir.join("ui_rules.json");
        if !rules_path.exists() {
            fs::write(
                &rules_path,
                serde_json::to_string_pretty(&default_design_rules())?,
            )?;
        }
        Ok(Self {
            page_path: dir.join("ui_page.html"),
            context_path: dir.join("ui_page_context.json"),
            prompt_path,
            request_path,
            rules_path,
        })
    }

    pub fn snapshot(&self) -> anyhow::Result<Option<UiPage>> {
        if !self.page_path.exists() {
            return Ok(None);
        }
        let html = fs::read_to_string(&self.page_path)?;
        if html.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(UiPage {
            generated_at: now_secs_f64(),
            source_fingerprint: self
                .context_snapshot()?
                .map(|c| c.source_fingerprint)
                .unwrap_or_default(),
            html,
        }))
    }

    pub fn context_snapshot(&self) -> anyhow::Result<Option<UiPageContext>> {
        if !self.context_path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.context_path)?;
        if content.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn needs_refresh(&self, fingerprint: &str) -> anyhow::Result<bool> {
        Ok(self
            .context_snapshot()?
            .map(|item| item.source_fingerprint != fingerprint)
            .unwrap_or(true))
    }

    pub fn prompt_text(&self) -> anyhow::Result<String> {
        if !self.prompt_path.exists() {
            fs::write(&self.prompt_path, default_page_prompt())?;
        }
        Ok(fs::read_to_string(&self.prompt_path)?)
    }

    pub fn prompt_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&self.prompt_text()?)
    }

    pub fn design_rules(&self) -> anyhow::Result<UiDesignRules> {
        if !self.rules_path.exists() {
            self.save_design_rules(&default_design_rules())?;
        }
        let content = fs::read_to_string(&self.rules_path)?;
        if content.trim().is_empty() {
            let rules = default_design_rules();
            self.save_design_rules(&rules)?;
            return Ok(rules);
        }
        Ok(serde_json::from_str(&content)?)
    }

    pub fn design_rules_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&serde_json::to_string(&self.design_rules()?)?)
    }

    pub fn user_request_memory(&self) -> anyhow::Result<UiUserIntentMemory> {
        if !self.request_path.exists() {
            self.save_user_request_memory(&default_user_request_memory())?;
        }
        let content = fs::read_to_string(&self.request_path)?;
        if content.trim().is_empty() {
            let memory = default_user_request_memory();
            self.save_user_request_memory(&memory)?;
            return Ok(memory);
        }
        if content.trim_start().starts_with('{') {
            return Ok(serde_json::from_str(&content)?);
        }
        Ok(UiUserIntentMemory {
            updated_at: now_secs_f64(),
            intent_type: "open_management_page".to_string(),
            desired_view: "project_state".to_string(),
            primary_request: content.trim().to_string(),
            constraints: Vec::new(),
            operator_notes: vec!["migrated from legacy plain text memory".to_string()],
        })
    }

    pub fn user_request_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&serde_json::to_string(&self.user_request_memory()?)?)
    }

    pub fn save_user_request_text(&self, text: &str) -> anyhow::Result<()> {
        let trimmed = text.trim();
        let memory = if trimmed.is_empty() {
            default_user_request_memory()
        } else {
            UiUserIntentMemory {
                updated_at: now_secs_f64(),
                intent_type: "open_management_page".to_string(),
                desired_view: "project_state".to_string(),
                primary_request: trimmed.to_string(),
                constraints: Vec::new(),
                operator_notes: Vec::new(),
            }
        };
        self.save_user_request_memory(&memory)
    }

    pub fn save_user_request_memory(&self, memory: &UiUserIntentMemory) -> anyhow::Result<()> {
        fs::write(&self.request_path, serde_json::to_string_pretty(memory)?)?;
        Ok(())
    }

    pub fn save_design_rules(&self, rules: &UiDesignRules) -> anyhow::Result<()> {
        fs::write(&self.rules_path, serde_json::to_string_pretty(rules)?)?;
        Ok(())
    }

    pub fn build_context(
        &self,
        model: &SystemModel,
        surface: &UiSurface,
        schema: &UiSchema,
        fingerprint: &str,
    ) -> anyhow::Result<UiPageContext> {
        let user_memory = self.user_request_memory()?;
        let context = UiPageContext {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint.to_string(),
            user_goal: render_user_goal_summary(&user_memory),
            user_memory,
            design_rules: self.design_rules()?,
            system_model: model.clone(),
            ui_surface: surface.clone(),
            ui_schema: schema.clone(),
        };
        self.save_context(&context)?;
        Ok(context)
    }

    pub fn generate_with_ui_agent(
        &self,
        context: &UiPageContext,
        fallback_html: &str,
        fingerprint: &str,
    ) -> anyhow::Result<UiPage> {
        let prompt = format!(
            "You are Rustpilot's UI page agent.\nReturn exactly one complete HTML document.\n\nPrompt DNA:\n{}\n\nUI rules JSON:\n{}\n\nUser goal memory:\n{}\n\nDesired view:\n{}\n\nPage context JSON:\n{}\n\nRules:\n1. The generated page is the primary UI.\n2. The fallback HTML is only a minimal bootstrap shell.\n3. Use a chat-style layout with Main, Agent Team, and an agent detail panel.\n4. Only use `/api/status`, `/api/wire`, and `/ws`.\n5. Do not invent unsupported actions.\n\nFallback HTML:\n{}",
            self.prompt_text()?,
            serde_json::to_string_pretty(&context.design_rules)?,
            context.user_goal,
            context.user_memory.desired_view,
            serde_json::to_string_pretty(context)?,
            fallback_html
        );
        let repo_root = self
            .page_path
            .parent()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("failed to resolve repo root for ui page manager"))?;
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
                        "You are a protocol-aware UI page generator. Output full HTML only."
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
                    LlmApiKind::OpenAiChatCompletions => client
                        .post(format!(
                            "{}/chat/completions",
                            config.api_base_url.trim_end_matches('/')
                        ))
                        .bearer_auth(&config.api_key)
                        .json(&request)
                        .send()?,
                    LlmApiKind::AnthropicMessages => {
                        let anthropic_request = anthropic_compat::build_request(
                            &model,
                            &request.messages,
                            None,
                            None,
                            request.temperature,
                        );
                        client
                            .post(format!(
                                "{}/messages?beta=true",
                                config.api_base_url.trim_end_matches('/')
                            ))
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
                        LlmApiKind::OpenAiChatCompletions => response
                            .json::<ChatResponse>()?
                            .choices
                            .into_iter()
                            .next()
                            .and_then(|c| c.message.content)
                            .ok_or_else(|| anyhow::anyhow!("ui page agent returned no html")),
                        LlmApiKind::AnthropicMessages => anthropic_compat::parse_response(
                            response.json::<anthropic_compat::AnthropicResponse>()?,
                        )
                        .content
                        .ok_or_else(|| anyhow::anyhow!("ui page agent returned no html")),
                    };
                }
                let status = response.status();
                let body = response.text().unwrap_or_default();
                last_error = Some(format!(
                    "ui page request failed with model {} and status {}: {}",
                    model, status, body
                ));
                if !is_model_unsupported_error(status, &body) {
                    anyhow::bail!(
                        "ui page request failed with model {} and status {}: {}",
                        model,
                        status,
                        body
                    );
                }
            }
            anyhow::bail!(
                "{}",
                last_error.unwrap_or_else(
                    || "ui page request failed without provider response".to_string()
                )
            )
        })
        .join()
        .map_err(|_| anyhow::anyhow!("ui page worker thread panicked"))??;
        let html = if content.trim().to_ascii_lowercase().contains("<html") {
            content
        } else {
            fallback_html.to_string()
        };
        let page = UiPage {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint.to_string(),
            html,
        };
        self.save_page(&page)?;
        Ok(page)
    }

    pub fn generate_from_context(
        &self,
        context: &UiPageContext,
        port: u16,
        fingerprint: &str,
    ) -> anyhow::Result<UiPage> {
        let page = UiPage {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint.to_string(),
            html: render_bootstrap_html(context, port),
        };
        self.save_page(&page)?;
        Ok(page)
    }

    pub fn save_context(&self, context: &UiPageContext) -> anyhow::Result<()> {
        fs::write(&self.context_path, serde_json::to_string_pretty(context)?)?;
        Ok(())
    }

    pub fn save_page(&self, page: &UiPage) -> anyhow::Result<()> {
        fs::write(&self.page_path, &page.html)?;
        Ok(())
    }
}

fn default_design_rules() -> UiDesignRules {
    UiDesignRules {
        schema_version: 1,
        design_principles: vec![
            "Rust provides data, memory, and rules; the final UI code should be generated by the UI agent.".to_string(),
            "The primary layout must be a chat product, not a dashboard or admin table.".to_string(),
            "Use one fixed Main conversation, one Agent Team group conversation, and one agent detail panel.".to_string(),
        ],
        interaction_rules: vec![
            "The generated page is the primary UI.".to_string(),
            "Fallback HTML is only a minimal bootstrap shell.".to_string(),
            "Messages sent from the page go to Main by default, and Main coordinates the rest of the system.".to_string(),
            "Selecting a group member should switch the detail panel to that agent.".to_string(),
            "Show persisted transcript content when available.".to_string(),
        ],
        visual_rules: vec![
            "Prefer a messaging product layout over raw JSON inspection screens.".to_string(),
            "The left side should feel like a contact list, the center like a conversation thread, and the right side like a profile/detail drawer.".to_string(),
            "Do not render raw JSON blobs as the primary surface.".to_string(),
        ],
        protocol_rules: vec![
            "Only call backend protocols that exist in the page context.".to_string(),
            "Use `/api/status`, `/api/wire`, and `/ws` only.".to_string(),
            "Use `chat_ui.main_friend`, `chat_ui.group_chat`, `chat_ui.agents`, and `chat_ui.agent_details` as the primary data contract for chat rendering.".to_string(),
            "Use `chat_ui.group_chat.timeline[].direction`, `timeline[].kind`, `agent.runtime_source`, and transcript metadata to drive chat rendering instead of inferring from raw text alone.".to_string(),
        ],
    }
}

fn default_page_prompt() -> &'static str {
    "Generate a full standalone HTML page for the current project state. The page must look and behave like a messaging product rather than a dashboard: a fixed Main conversation in the center flow, an Agent Team group thread, and a right-side detail panel for the selected agent's transcript and runtime state. Main is the default chat target. Selecting an agent from the group must switch the detail panel to that agent. Use the `chat_ui` data contract from `/api/status` as the primary rendering source. Rust provides the context and rules; your generated page is the real UI."
}

fn default_user_request_memory() -> UiUserIntentMemory {
    UiUserIntentMemory {
        updated_at: 0.0,
        intent_type: "open_management_page".to_string(),
        desired_view: "project_state".to_string(),
        primary_request: "Open a management page for the current project state.".to_string(),
        constraints: vec![
            "Show a fixed Main friend conversation.".to_string(),
            "Show an Agent Team group conversation created around Main.".to_string(),
            "Allow selecting an agent from the group and inspecting that agent's transcript detail.".to_string(),
        ],
        operator_notes: vec![
            "Do not fall back to a hand-written full UI.".to_string(),
            "Prefer a WeChat-like messaging layout over dashboard cards.".to_string(),
        ],
    }
}

fn render_user_goal_summary(memory: &UiUserIntentMemory) -> String {
    let mut parts = vec![
        format!("intent_type={}", memory.intent_type),
        format!("desired_view={}", memory.desired_view),
        format!("primary_request={}", memory.primary_request),
    ];
    if !memory.constraints.is_empty() {
        parts.push(format!("constraints={}", memory.constraints.join(" | ")));
    }
    if !memory.operator_notes.is_empty() {
        parts.push(format!(
            "operator_notes={}",
            memory.operator_notes.join(" | ")
        ));
    }
    parts.join("\n")
}

fn render_bootstrap_html(context: &UiPageContext, port: u16) -> String {
    let desired_view = context.user_memory.desired_view.as_str();
    let title = fallback_view_title(desired_view, &context.ui_schema.title);
    let subtitle = fallback_view_subtitle(desired_view, &context.ui_schema.subtitle);
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{}</title>
  <style>
    body {{ margin:0; font-family:Segoe UI,sans-serif; background:#0b1117; color:#ecf3f8; }}
    main {{ min-height:100vh; display:grid; place-items:center; padding:24px; }}
    section {{ width:min(760px,100%); background:#111922; border:1px solid #263445; border-radius:24px; padding:28px; }}
    p, pre {{ color:#8fa0b2; line-height:1.6; }}
    button {{ padding:12px 16px; border:none; border-radius:14px; background:#57c08a; color:#072213; font-weight:700; cursor:pointer; }}
    pre {{ padding:16px; background:#0c131a; border:1px solid #263445; border-radius:16px; white-space:pre-wrap; word-break:break-word; }}
  </style>
</head>
<body>
  <main>
    <section>
      <h1>{}</h1>
      <p>{}</p>
      <p>view: {} | intent: {} | ui: 127.0.0.1:{}</p>
      <p>The real interface is generated by the UI agent. This bootstrap page only keeps the route alive while generation is in progress or retrying.</p>
      <button id="refresh">Retry Generated UI</button>
      <pre id="status">waiting for generated ui page...</pre>
    </section>
  </main>
  <script>
    async function refreshPage() {{
      const status = document.getElementById('status');
      status.textContent = 'checking /api/status...';
      try {{
        const response = await fetch('/api/status');
        const payload = await response.json();
        const count = Array.isArray(payload.chat_ui?.agents) ? payload.chat_ui.agents.length : 0;
        status.textContent = `backend ready; agents=${{count}}; reloading`;
        window.location.reload();
      }} catch (error) {{
        status.textContent = `bootstrap query failed: ${{error}}`;
      }}
    }}
    document.getElementById('refresh').addEventListener('click', refreshPage);
    setTimeout(refreshPage, 900);
  </script>
</body>
</html>"#,
        escape_html(title),
        escape_html(title),
        escape_html(subtitle),
        escape_html(desired_view),
        escape_html(&context.user_memory.intent_type),
        port
    )
}

fn fallback_view_title<'a>(desired_view: &str, default_title: &'a str) -> &'a str {
    match desired_view {
        "task_board" => "Task Board",
        "session_console" => "Session Console",
        "approval_overview" => "Approval Overview",
        "resident_monitor" => "Resident Monitor",
        _ => default_title,
    }
}

fn fallback_view_subtitle<'a>(desired_view: &str, default_subtitle: &'a str) -> &'a str {
    match desired_view {
        "task_board" => "Track queued work, blockers, and dispatch activity.",
        "session_console" => "Inspect sessions, focus routing, and live interaction state.",
        "approval_overview" => "Review policy state, blocks, and governance signals.",
        "resident_monitor" => "Watch resident agents, backlog, and runtime health.",
        _ => default_subtitle,
    }
}

fn hash_string(input: &str) -> anyhow::Result<String> {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    Ok(format!("{:x}", hasher.finish()))
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{UiDesignRules, UiPageContext, UiPageManager, UiUserIntentMemory};
    use crate::project_tools::{
        SystemModel, SystemProtocol, SystemSummary, UiAction, UiSchema, UiSection, UiSurface,
        UiSurfacePage,
    };

    #[test]
    fn bootstrap_page_is_minimal() {
        let temp_dir = std::env::temp_dir().join(format!(
            "rustpilot-ui-page-test-{}-{}",
            std::process::id(),
            crate::project_tools::util::now_secs_f64()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let manager = UiPageManager::new(temp_dir.clone()).expect("manager");
        let page = manager
            .generate_from_context(&sample_context(), 8800, "page")
            .expect("page");
        assert!(
            page.html
                .contains("The real interface is generated by the UI agent")
        );
        assert!(page.html.contains("/api/status"));
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn design_rules_round_trip() {
        let temp_dir = std::env::temp_dir().join(format!(
            "rustpilot-ui-rules-test-{}-{}",
            std::process::id(),
            crate::project_tools::util::now_secs_f64()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let manager = UiPageManager::new(temp_dir.clone()).expect("manager");
        let mut rules = manager.design_rules().expect("rules");
        rules.visual_rules.push("Use stable ordering.".to_string());
        manager.save_design_rules(&rules).expect("save rules");
        assert!(
            manager
                .design_rules()
                .expect("saved")
                .visual_rules
                .iter()
                .any(|item| item == "Use stable ordering.")
        );
        let _ = std::fs::remove_dir_all(temp_dir);
    }

    fn sample_context() -> UiPageContext {
        UiPageContext {
            generated_at: 0.0,
            source_fingerprint: "fp".to_string(),
            user_goal: "show project state".to_string(),
            user_memory: UiUserIntentMemory {
                updated_at: 0.0,
                intent_type: "open_management_page".to_string(),
                desired_view: "project_state".to_string(),
                primary_request: "show project state".to_string(),
                constraints: Vec::new(),
                operator_notes: Vec::new(),
            },
            design_rules: UiDesignRules {
                schema_version: 1,
                design_principles: vec!["one file".to_string()],
                interaction_rules: Vec::new(),
                visual_rules: Vec::new(),
                protocol_rules: Vec::new(),
            },
            system_model: SystemModel {
                generated_at: 0.0,
                summary: SystemSummary {
                    resident_count: 1,
                    pending_tasks: 0,
                    running_tasks: 0,
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
                    supported_sections: vec!["metrics".to_string(), "composer".to_string()],
                    supported_sources: vec!["summary".to_string(), "composer".to_string()],
                    event_types: vec!["system.snapshot".to_string()],
                }],
                residents: Vec::new(),
                recent_prompt_changes: Vec::new(),
                tasks: Vec::new(),
                proposals: Vec::new(),
                decisions: Vec::new(),
            },
            ui_surface: UiSurface {
                generated_at: 0.0,
                source_fingerprint: "surface".to_string(),
                title: "Control Surface".to_string(),
                summary: "Manage the project".to_string(),
                pages: vec![UiSurfacePage {
                    id: "system-overview".to_string(),
                    title: "System Overview".to_string(),
                    purpose: "Operate the system".to_string(),
                    audience: "operator".to_string(),
                    data_sources: vec!["summary".to_string()],
                    supported_sections: vec!["metrics".to_string(), "composer".to_string()],
                    actions: vec![UiAction {
                        id: "dispatch-ui".to_string(),
                        title: "Dispatch".to_string(),
                        protocol_id: "ui.wire.dispatch".to_string(),
                        target: "ui".to_string(),
                        description: "Send a request".to_string(),
                    }],
                    notes: Vec::new(),
                }],
            },
            ui_schema: UiSchema {
                generated_at: 0.0,
                source_fingerprint: "schema".to_string(),
                title: "Control Surface".to_string(),
                subtitle: "Operate the system".to_string(),
                theme_name: "paper-console".to_string(),
                sections: vec![UiSection {
                    id: "summary".to_string(),
                    title: "Summary".to_string(),
                    kind: "metrics".to_string(),
                    source: "summary".to_string(),
                    description: "Current state".to_string(),
                    empty_text: String::new(),
                    columns: 1,
                    target_options: Vec::new(),
                    labels: Vec::new(),
                }],
            },
        }
    }
}
