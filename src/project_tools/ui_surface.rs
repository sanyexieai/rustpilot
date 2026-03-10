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
        Ok(fs::read_to_string(&self.ui_prompt_path)?)
    }

    pub fn prompt_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&self.prompt_text()?)
    }

    pub fn planner_prompt_text(&self) -> anyhow::Result<String> {
        if !self.planner_prompt_path.exists() {
            fs::write(&self.planner_prompt_path, default_planner_prompt())?;
        }
        Ok(fs::read_to_string(&self.planner_prompt_path)?)
    }

    pub fn planner_prompt_fingerprint(&self) -> anyhow::Result<String> {
        hash_string(&self.planner_prompt_text()?)
    }

    pub fn collection_fingerprint(&self, model: &SystemModel) -> anyhow::Result<String> {
        Ok(format!(
            "{}:{}",
            self.planner_prompt_fingerprint()?,
            model_fingerprint(model)?
        ))
    }

    pub fn rebuild_from_model(&self, model: &SystemModel) -> anyhow::Result<UiSurface> {
        let fingerprint = model_fingerprint(model)?;
        let page = UiPage {
            id: "system-overview".to_string(),
            title: "系统总览".to_string(),
            purpose: "统一承载系统运行状态、治理信息和对常驻 agent 的操作入口".to_string(),
            audience: "操作者、调度者、观察者".to_string(),
            data_sources: vec![
                "summary".to_string(),
                "alerts".to_string(),
                "residents".to_string(),
                "tasks".to_string(),
                "proposals".to_string(),
                "decisions".to_string(),
            ],
            supported_sections: vec![
                "metrics".to_string(),
                "alerts".to_string(),
                "residents".to_string(),
                "composer".to_string(),
                "tasks".to_string(),
                "proposals".to_string(),
                "decisions".to_string(),
            ],
            actions: vec![
                UiAction {
                    id: "dispatch-ui".to_string(),
                    title: "投递给 UI Agent".to_string(),
                    protocol_id: "ui.request.dispatch".to_string(),
                    target: "ui".to_string(),
                    description: "用于请求页面设计、结构调整和界面演化".to_string(),
                },
                UiAction {
                    id: "dispatch-concierge".to_string(),
                    title: "投递给需求接待".to_string(),
                    protocol_id: "ui.request.dispatch".to_string(),
                    target: "concierge".to_string(),
                    description: "用于把自然语言需求整理成系统任务".to_string(),
                },
                UiAction {
                    id: "dispatch-reviewer".to_string(),
                    title: "投递给评审整理".to_string(),
                    protocol_id: "ui.request.dispatch".to_string(),
                    target: "reviewer".to_string(),
                    description: "用于汇总阻塞、失败和优化建议".to_string(),
                },
            ],
            notes: vec![
                "页面结构必须服从系统协议，不允许编造不存在的数据源或操作".to_string(),
                "页面信息文件是 UI Agent 的设计输入，不是最终页面代码".to_string(),
            ],
        };
        let surface = UiSurface {
            generated_at: now_secs_f64(),
            source_fingerprint: fingerprint,
            title: "Rustpilot 页面信息".to_string(),
            summary: "由页面信息收集阶段整理出的可展示页面、数据源和可用动作，供 UI Agent 生成界面"
                .to_string(),
            pages: vec![page],
        };
        self.save(&surface)?;
        Ok(surface)
    }

    pub fn generate_with_collector(
        &self,
        model: &SystemModel,
        planner_prompt: &str,
    ) -> anyhow::Result<UiSurface> {
        let fallback = self.rebuild_from_model(model)?;
        let model_json = serde_json::to_string_pretty(model)?;
        let fallback_json = serde_json::to_string_pretty(&fallback)?;
        let prompt = format!(
            "你是 Rustpilot 的页面信息收集 agent。你的职责是从系统业务模型中整理出“应该展示哪些页面、页面服务谁、依赖哪些数据源、允许哪些动作”，并固化为 ui_surface.json。\n\
只返回一个 JSON 对象，不要输出解释、Markdown 或代码块。\n\
\n\
收集提示词如下：\n{planner_prompt}\n\
\n\
规则：\n\
1. 你的输出是页面信息文件，不是最终页面代码，也不是 UiSchema。\n\
2. 页面信息必须来自系统功能和后端协议，不得虚构页面能力。\n\
3. actions.protocol_id、actions.target 必须来自 protocols 中真实存在的能力。\n\
4. pages[].supported_sections 和 pages[].data_sources 只能使用系统协议允许的 section/source。\n\
5. 页面文案默认使用简体中文。\n\
\n\
系统业务模型如下：\n{model_json}\n\
\n\
兜底页面信息如下：\n{fallback_json}\n\
\n\
现在输出最终 JSON："
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
                        "你是严格受协议约束的页面信息规划器。只能输出合法 JSON，不能虚构不存在的页面能力。"
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
                                .ok_or_else(|| anyhow::anyhow!("surface collector returned no content"))
                        }
                        LlmApiKind::AnthropicMessages => {
                            let parsed: anthropic_compat::AnthropicResponse = response.json()?;
                            anthropic_compat::parse_response(parsed)
                                .content
                                .ok_or_else(|| anyhow::anyhow!("surface collector returned no content"))
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
                last_error.unwrap_or_else(|| "ui surface request failed without provider response".to_string())
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
    r#"你是 Rustpilot 的 UI Agent。

你的职责：
- 读取页面信息文件 ui_surface.json
- 读取系统业务模型 system_model.json
- 在后端协议允许的边界内设计页面结构和文案
- 产出 UiSchema 作为页面缓存

你的限制：
- 不得虚构不存在的接口、事件、按钮动作或数据源
- 不得删除系统关键区块：metrics、residents、composer
- 有 alerts 时必须体现 alerts
- 页面文案默认使用简体中文
- 页面应优先表达系统业务和协作状态，而不是仅表现单个 agent

你的演化目标：
- 当系统功能增加时，能根据页面信息文件新增或重组页面区块
- 当用户更新提示词时，能在不突破协议约束的前提下优化布局、文案和层级
- 最终页面应缓存到本地，供后续快速加载与比对
"#
}

fn default_planner_prompt() -> &'static str {
    r#"你是 Rustpilot 的页面信息收集 agent。

你的职责：
- 从 system_model.json 收集系统当前可见的功能、角色、数据源和动作
- 整理成稳定的页面信息文件 ui_surface.json
- 让 UI Agent 后续能基于这份页面信息演化页面

你的限制：
- 不得直接生成页面代码
- 不得虚构协议、接口、事件、目标角色或数据源
- 页面信息应体现系统业务，而不是只体现单个 agent
- 页面信息默认使用简体中文

你的演化目标：
- 当系统功能升级时，补充或重组 pages / actions / supported_sections
- 当协议能力变化时，同步调整页面信息
- 为 UI Agent 提供稳定、可缓存、可审查的设计输入
"#
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
