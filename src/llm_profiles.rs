use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmApiKind {
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub id: &'static str,
    pub default_base_url: &'static str,
    pub default_model: &'static str,
    pub api_kind: LlmApiKind,
    pub api_key_env_vars: &'static [&'static str],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfileRecord {
    pub profile_id: String,
    pub provider: String,
    pub api_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub source: String,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AuthProfileStore {
    #[serde(default)]
    profiles: BTreeMap<String, AuthProfileRecord>,
    #[serde(default)]
    auth_order: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct ResolvedAuthProfile {
    pub profile_id: String,
    pub provider: String,
    pub api_key: String,
    pub api_base_url: String,
    pub model: String,
    pub api_kind: LlmApiKind,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct LlmProfileManager {
    path: PathBuf,
}

impl LlmProfileManager {
    pub fn new(repo_root: &Path) -> anyhow::Result<Self> {
        let team_dir = repo_root.join(".team");
        fs::create_dir_all(&team_dir)?;
        Ok(Self {
            path: team_dir.join("llm_auth_profiles.json"),
        })
    }

    pub fn resolve_from_env(&self) -> anyhow::Result<ResolvedAuthProfile> {
        self.seed_from_env()?;

        let raw_provider = std::env::var("LLM_PROVIDER").ok();
        let llm_base_url = std::env::var("LLM_API_BASE_URL").ok();
        let anthropic_base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
        let llm_model = std::env::var("LLM_MODEL").ok();
        let anthropic_model = std::env::var("ANTHROPIC_MODEL").ok();
        let provider_hint_base_url = llm_base_url.as_deref().or(anthropic_base_url.as_deref());
        let provider_hint_model = llm_model.as_deref().or(anthropic_model.as_deref());
        let requested_provider = resolve_requested_provider(
            raw_provider.as_deref(),
            provider_hint_base_url,
            provider_hint_model,
        );
        let (raw_base_url, raw_model) = env_hints_for_provider(
            &requested_provider,
            llm_base_url.as_deref(),
            anthropic_base_url.as_deref(),
            llm_model.as_deref(),
            anthropic_model.as_deref(),
        );
        let requested_profile = std::env::var("LLM_AUTH_PROFILE").ok();
        self.resolve(
            &requested_provider,
            requested_profile.as_deref(),
            raw_base_url,
            raw_model,
        )
    }

    pub fn resolve(
        &self,
        provider_hint: &str,
        profile_hint: Option<&str>,
        base_url_hint: Option<&str>,
        model_hint: Option<&str>,
    ) -> anyhow::Result<ResolvedAuthProfile> {
        let provider = normalize_provider(provider_hint, base_url_hint, model_hint);
        let spec = provider_spec(&provider);
        let store = self.load_store()?;

        if let Some(profile_id) = profile_hint.map(str::trim).filter(|item| !item.is_empty()) {
            let profile = store.profiles.get(profile_id).ok_or_else(|| {
                anyhow::anyhow!("LLM auth profile '{}' was not found", profile_id)
            })?;
            if !profile.enabled {
                anyhow::bail!("LLM auth profile '{}' is disabled", profile_id);
            }
            if normalize_provider(&profile.provider, None, None) != provider {
                anyhow::bail!(
                    "LLM auth profile '{}' belongs to provider '{}', not '{}'",
                    profile_id,
                    profile.provider,
                    provider
                );
            }
            return Ok(resolve_profile(profile, spec, base_url_hint, model_hint));
        }

        let ordered = ordered_profiles_for_provider(&store, &provider);
        for profile_id in ordered {
            if let Some(profile) = store.profiles.get(&profile_id)
                && profile.enabled
            {
                return Ok(resolve_profile(profile, spec, base_url_hint, model_hint));
            }
        }

        let env_api_key = resolve_env_api_key(&provider)
            .ok_or_else(|| anyhow::anyhow!("No API key found for provider '{}'", provider))?;
        Ok(ResolvedAuthProfile {
            profile_id: format!("{}:env", provider),
            provider: provider.clone(),
            api_key: env_api_key.0,
            api_base_url: normalize_base_url(&provider, base_url_hint),
            model: normalize_model(&provider, model_hint.unwrap_or(spec.default_model)),
            api_kind: spec.api_kind,
            source: env_api_key.1,
        })
    }

    fn seed_from_env(&self) -> anyhow::Result<()> {
        let raw_provider = std::env::var("LLM_PROVIDER").ok();
        let llm_base_url = std::env::var("LLM_API_BASE_URL").ok();
        let anthropic_base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
        let llm_model = std::env::var("LLM_MODEL").ok();
        let anthropic_model = std::env::var("ANTHROPIC_MODEL").ok();
        let provider_hint_base_url = llm_base_url.as_deref().or(anthropic_base_url.as_deref());
        let provider_hint_model = llm_model.as_deref().or(anthropic_model.as_deref());
        let provider = resolve_requested_provider(
            raw_provider.as_deref(),
            provider_hint_base_url,
            provider_hint_model,
        );
        let (raw_base_url, raw_model) = env_hints_for_provider(
            &provider,
            llm_base_url.as_deref(),
            anthropic_base_url.as_deref(),
            llm_model.as_deref(),
            anthropic_model.as_deref(),
        );
        let Some((api_key, source)) = resolve_env_api_key(&provider) else {
            return Ok(());
        };

        let profile_id = std::env::var("LLM_AUTH_PROFILE")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{}:default", provider));

        let _guard = FileLock::acquire(self.path.clone())?;
        let mut store = self.load_store_unlocked()?;
        let now = now_secs_f64();
        let normalized_base_url = normalize_base_url(&provider, raw_base_url);
        let normalized_model = normalize_model(
            &provider,
            raw_model.unwrap_or(provider_spec(&provider).default_model),
        );

        if let Some(existing) = store.profiles.get_mut(&profile_id) {
            existing.provider = provider.clone();
            existing.api_key = api_key;
            existing.api_base_url = Some(normalized_base_url);
            existing.model = Some(normalized_model);
            existing.enabled = true;
            existing.source = source;
            existing.updated_at = now;
        } else {
            store.profiles.insert(
                profile_id.clone(),
                AuthProfileRecord {
                    profile_id: profile_id.clone(),
                    provider: provider.clone(),
                    api_key,
                    api_base_url: Some(normalized_base_url),
                    model: Some(normalized_model),
                    enabled: true,
                    source,
                    created_at: now,
                    updated_at: now,
                },
            );
        }

        let order = store.auth_order.entry(provider).or_default();
        if !order.iter().any(|item| item == &profile_id) {
            order.push(profile_id);
        }
        self.save_store_unlocked(&store)
    }

    fn load_store(&self) -> anyhow::Result<AuthProfileStore> {
        let _guard = FileLock::acquire(self.path.clone())?;
        self.load_store_unlocked()
    }

    fn load_store_unlocked(&self) -> anyhow::Result<AuthProfileStore> {
        if !self.path.exists() {
            return Ok(AuthProfileStore::default());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(AuthProfileStore::default());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_store_unlocked(&self, store: &AuthProfileStore) -> anyhow::Result<()> {
        atomic_write_json(&self.path, &serde_json::to_string_pretty(store)?)?;
        Ok(())
    }
}

fn env_hints_for_provider<'a>(
    provider: &str,
    llm_base_url: Option<&'a str>,
    anthropic_base_url: Option<&'a str>,
    llm_model: Option<&'a str>,
    anthropic_model: Option<&'a str>,
) -> (Option<&'a str>, Option<&'a str>) {
    match normalize_provider(provider, None, None).as_str() {
        "kimi-coding" => (
            llm_base_url.or(anthropic_base_url),
            llm_model.or(anthropic_model),
        ),
        _ => (
            llm_base_url.or(anthropic_base_url),
            llm_model.or(anthropic_model),
        ),
    }
}

fn ordered_profiles_for_provider(store: &AuthProfileStore, provider: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ordered = Vec::new();

    if let Some(items) = store.auth_order.get(provider) {
        for item in items {
            let trimmed = item.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                ordered.push(trimmed.to_string());
            }
        }
    }

    let mut extras = store
        .profiles
        .iter()
        .filter(|(_, profile)| normalize_provider(&profile.provider, None, None) == provider)
        .map(|(profile_id, _)| profile_id.clone())
        .collect::<Vec<_>>();
    extras.sort();
    for item in extras {
        if seen.insert(item.clone()) {
            ordered.push(item);
        }
    }

    ordered
}

fn resolve_profile(
    profile: &AuthProfileRecord,
    spec: ProviderSpec,
    base_url_hint: Option<&str>,
    model_hint: Option<&str>,
) -> ResolvedAuthProfile {
    let provider = normalize_provider(&profile.provider, base_url_hint, model_hint);
    ResolvedAuthProfile {
        profile_id: profile.profile_id.clone(),
        provider: provider.clone(),
        api_key: profile.api_key.clone(),
        api_base_url: normalize_base_url(
            &provider,
            base_url_hint.or(profile.api_base_url.as_deref()),
        ),
        model: normalize_model(
            &provider,
            model_hint
                .or(profile.model.as_deref())
                .unwrap_or(spec.default_model),
        ),
        api_kind: spec.api_kind,
        source: format!("profile:{}", profile.profile_id),
    }
}

pub fn provider_spec(provider: &str) -> ProviderSpec {
    match normalize_provider(provider, None, None).as_str() {
        "kimi-coding" => ProviderSpec {
            id: "kimi-coding",
            default_base_url: "https://api.kimi.com/coding/v1",
            default_model: "kimi-for-coding",
            api_kind: LlmApiKind::AnthropicMessages,
            api_key_env_vars: &[
                "LLM_API_KEY",
                "KIMI_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_API_KEY",
            ],
        },
        "moonshot" => ProviderSpec {
            id: "moonshot",
            default_base_url: "https://api.moonshot.cn/v1",
            default_model: "kimi-k2.5",
            api_kind: LlmApiKind::OpenAiChatCompletions,
            api_key_env_vars: &["MOONSHOT_API_KEY", "LLM_API_KEY"],
        },
        _ => ProviderSpec {
            id: "minimax",
            default_base_url: "https://api.minimaxi.com/v1",
            default_model: "MiniMax-M2.5",
            api_kind: LlmApiKind::OpenAiChatCompletions,
            api_key_env_vars: &["MINIMAX_API_KEY", "LLM_API_KEY"],
        },
    }
}

pub fn resolve_env_api_key(provider: &str) -> Option<(String, String)> {
    let spec = provider_spec(provider);
    for env_key in spec.api_key_env_vars {
        let Ok(value) = std::env::var(env_key) else {
            continue;
        };
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() || looks_like_placeholder_api_key(&trimmed) {
            continue;
        }
        return Some((trimmed, format!("env:{}", env_key)));
    }
    None
}

fn looks_like_placeholder_api_key(value: &str) -> bool {
    matches!(
        value,
        "your_api_key_here"
            | "your_moonshot_key_here"
            | "your_minimax_key_here"
            | "your_kimi_key_here"
            | "your_kimi_coding_key_here"
    )
}

pub fn normalize_provider(raw: &str, base_url: Option<&str>, model: Option<&str>) -> String {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        if let Some(inferred) = infer_provider(base_url, model) {
            return inferred;
        }
        return "minimax".to_string();
    }
    match value.as_str() {
        "kimi" | "kimi-code" | "kimi-coding" | "kimi-for-coding" | "kimi-latest" => {
            "kimi-coding".to_string()
        }
        "moonshot" => "moonshot".to_string(),
        "minimax" => "minimax".to_string(),
        other if other.starts_with("moonshot-v1") => "moonshot".to_string(),
        other if other.starts_with("kimi-k2") => "moonshot".to_string(),
        _ => value,
    }
}

pub fn normalize_base_url(provider: &str, raw: Option<&str>) -> String {
    let spec = provider_spec(provider);
    let value = raw.unwrap_or_default().trim();
    if value.is_empty() {
        return spec.default_base_url.to_string();
    }
    let trimmed = value.trim_end_matches('/');
    match spec.id {
        "kimi-coding" if trimmed.eq_ignore_ascii_case("https://api.kimi.com/coding") => {
            "https://api.kimi.com/coding/v1".to_string()
        }
        _ => trimmed.to_string(),
    }
}

pub fn default_model_for_provider(provider: &str) -> String {
    provider_spec(provider).default_model.to_string()
}

pub fn normalize_model(provider: &str, raw: &str) -> String {
    let value = raw.trim();
    match provider_spec(provider).id {
        "kimi-coding" => match value {
            "" | "kimi" | "kimi-latest" | "kimi-for-coding" | "k2p5" => {
                "kimi-for-coding".to_string()
            }
            other => other.to_string(),
        },
        "moonshot" => match value {
            "" => "kimi-k2.5".to_string(),
            "moonshot-v1" => "kimi-k2.5".to_string(),
            other => other.to_string(),
        },
        _ => {
            if value.is_empty() {
                "MiniMax-M2.5".to_string()
            } else {
                value.to_string()
            }
        }
    }
}

fn infer_provider(base_url: Option<&str>, model: Option<&str>) -> Option<String> {
    let base = base_url.unwrap_or_default().to_ascii_lowercase();
    if base.contains("api.kimi.com/coding") {
        return Some("kimi-coding".to_string());
    }
    if base.contains("api.moonshot.cn") || base.contains("api.moonshot.ai") {
        return Some("moonshot".to_string());
    }

    let model = model.unwrap_or_default().to_ascii_lowercase();
    if model.contains("kimi-for-coding") || model == "k2p5" {
        return Some("kimi-coding".to_string());
    }
    if model.contains("kimi-k2") || model.starts_with("moonshot-v1") {
        return Some("moonshot".to_string());
    }
    None
}

fn resolve_requested_provider(
    raw_provider: Option<&str>,
    base_url: Option<&str>,
    model: Option<&str>,
) -> String {
    let normalized = normalize_provider(raw_provider.unwrap_or_default(), base_url, model);
    if normalized != "minimax" || raw_provider.is_some() || base_url.is_some() || model.is_some() {
        return normalized;
    }
    if std::env::var("KIMI_API_KEY").is_ok() {
        return "kimi-coding".to_string();
    }
    if std::env::var("MOONSHOT_API_KEY").is_ok() {
        return "moonshot".to_string();
    }
    normalized
}

fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn default_true() -> bool {
    true
}

struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(data_path: PathBuf) -> anyhow::Result<Self> {
        let lock_path = PathBuf::from(format!("{}.lock", data_path.display()));
        let stale_after = Duration::from_secs(5);
        for _ in 0..200 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(Self { path: lock_path }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale_lock(&lock_path, stale_after) {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(err.into()),
            }
        }
        anyhow::bail!("timed out waiting for lock {}", lock_path.display())
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale_lock(path: &Path, stale_after: Duration) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified_at) = metadata.modified() else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(modified_at) else {
        return false;
    };
    age >= stale_after
}

fn atomic_write_json(path: &Path, content: &str) -> anyhow::Result<()> {
    let tmp_path = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&tmp_path, content)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}
