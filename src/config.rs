use crate::llm_profiles::{
    LlmApiKind, LlmProfileManager, default_model_for_provider, normalize_base_url, normalize_model,
    normalize_provider,
};
use std::collections::HashSet;
use std::path::Path;

pub const DEFAULT_LLM_USER_AGENT: &str = "openclaw";

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub provider: String,
    pub profile_id: String,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub api_kind: LlmApiKind,
    pub source: String,
}

impl LlmConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::from_repo_root(&cwd)
    }

    pub fn from_repo_root(repo_root: &Path) -> anyhow::Result<Self> {
        let manager = LlmProfileManager::new(repo_root)?;
        let resolved = manager.resolve_from_env()?;
        Ok(Self {
            provider: resolved.provider,
            profile_id: resolved.profile_id,
            api_key: resolved.api_key,
            api_base_url: resolved.api_base_url,
            model: resolved.model,
            api_kind: resolved.api_kind,
            source: resolved.source,
        })
    }

    pub fn model_candidates(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut models = Vec::new();
        let mut push = |model: String| {
            let trimmed = model.trim().to_string();
            if !trimmed.is_empty() && seen.insert(trimmed.clone()) {
                models.push(trimmed);
            }
        };

        push(self.model.clone());

        if let Ok(raw) = std::env::var("LLM_FALLBACK_MODELS") {
            for item in raw.split(',') {
                push(normalize_model(&self.provider, item));
            }
        }

        if self.provider.eq_ignore_ascii_case("minimax") && self.model == "MiniMax-M2.5" {
            push("codex-MiniMax-M2.5".to_string());
        }
        if self.provider.eq_ignore_ascii_case("kimi-coding") && self.model == "kimi-for-coding" {
            push("k2p5".to_string());
        }

        models
    }
}

pub fn is_model_unsupported_error(status: reqwest::StatusCode, body: &str) -> bool {
    if !(status.is_client_error() || status.is_server_error()) {
        return false;
    }
    let text = body.to_ascii_lowercase();
    text.contains("not support model")
        || text.contains("unsupported model")
        || text.contains("model not supported")
        || text.contains("current code plan not support model")
}

pub fn normalize_provider_for_env(
    raw: &str,
    base_url: Option<&str>,
    model: Option<&str>,
) -> String {
    normalize_provider(raw, base_url, model)
}

pub fn normalize_base_url_for_env(provider: &str, raw: Option<&str>) -> String {
    normalize_base_url(provider, raw)
}

pub fn default_model_for_provider_env(provider: &str) -> String {
    default_model_for_provider(provider)
}

pub fn default_llm_user_agent() -> String {
    std::env::var("LLM_USER_AGENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_LLM_USER_AGENT.to_string())
}
