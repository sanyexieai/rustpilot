use crate::constants::LLM_TIMEOUT_SECS;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvGuideUpdate {
    pub created: bool,
    pub added_keys: Vec<String>,
}

impl EnvGuideUpdate {
    pub fn unchanged() -> Self {
        Self {
            created: false,
            added_keys: Vec::new(),
        }
    }
}

pub fn detect_repo_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    path.exists().then_some(path)
}

pub fn llm_timeout_secs() -> u64 {
    std::env::var("LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(LLM_TIMEOUT_SECS)
}

pub fn llm_timeout_secs_for_provider(provider: &str) -> u64 {
    std::env::var("LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| match provider {
            "kimi-coding" => 300,
            _ => LLM_TIMEOUT_SECS,
        })
}

pub fn ensure_env_guidance(cwd: &Path) -> anyhow::Result<EnvGuideUpdate> {
    let env_path = cwd.join(".env");
    if !env_path.exists() {
        fs::write(&env_path, env_template())?;
        return Ok(EnvGuideUpdate {
            created: true,
            added_keys: required_env_entries()
                .iter()
                .map(|(key, _)| (*key).to_string())
                .collect(),
        });
    }

    let existing = fs::read_to_string(&env_path)?;
    let keys = parse_defined_env_keys(&existing);
    let missing: Vec<(&str, &str)> = required_env_entries()
        .iter()
        .copied()
        .filter(|(key, _)| !keys.contains(*key))
        .collect();

    if missing.is_empty() {
        return Ok(EnvGuideUpdate::unchanged());
    }

    let mut updated = existing;
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str("\n# Added by rustpilot startup guidance\n");
    for (key, value) in &missing {
        updated.push_str(&format!("{}={}\n", key, value));
    }
    fs::write(&env_path, updated)?;

    Ok(EnvGuideUpdate {
        created: false,
        added_keys: missing
            .into_iter()
            .map(|(key, _)| key.to_string())
            .collect(),
    })
}

pub fn prompt_and_store_llm_api_key(cwd: &Path) -> anyhow::Result<bool> {
    print!("请输入 LLM_API_KEY（直接回车取消）: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let api_key = input.trim();
    if api_key.is_empty() {
        return Ok(false);
    }

    upsert_env_var(&cwd.join(".env"), "LLM_API_KEY", api_key)?;
    Ok(true)
}

fn required_env_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("LLM_API_KEY", "your_api_key_here"),
        ("LLM_PROVIDER", "kimi-coding"),
        ("ANTHROPIC_AUTH_TOKEN", "your_kimi_coding_key_here"),
        ("ANTHROPIC_BASE_URL", "https://api.kimi.com/coding/"),
        ("ANTHROPIC_MODEL", "kimi-for-coding"),
        ("LLM_TIMEOUT_SECS", "300"),
        ("LLM_USER_AGENT", "openclaw"),
    ]
}

fn env_template() -> String {
    let mut content = String::from(
        "# Required\n\
LLM_API_KEY=your_api_key_here\n\
\n\
# Optional (defaults shown)\n",
    );
    for (key, value) in required_env_entries().iter().skip(1) {
        content.push_str(&format!("{}={}\n", key, value));
    }
    content.push_str(
        "\n# Optional provider-specific keys\n# MOONSHOT_API_KEY=your_moonshot_key_here\n# KIMI_API_KEY=your_kimi_key_here\n# MINIMAX_API_KEY=your_minimax_key_here\n\
\n# Optional auth profile override\n# LLM_AUTH_PROFILE=kimi-coding:default\n\
\n# Optional: custom skills directory\n# SKILLS_DIR=./skills\n",
    );
    content
}

fn parse_defined_env_keys(content: &str) -> HashSet<String> {
    let mut keys = HashSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, _value)) = trimmed.split_once('=') {
            let key = key.trim();
            if !key.is_empty() {
                keys.insert(key.to_string());
            }
        }
    }
    keys
}

fn upsert_env_var(path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut replaced = false;
    let mut output = String::new();

    for line in existing.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            if let Some((candidate, _)) = line.split_once('=') {
                if candidate.trim() == key {
                    output.push_str(&format!("{}={}\n", key, value));
                    replaced = true;
                    continue;
                }
            }
        }
        output.push_str(line);
        output.push('\n');
    }

    if !replaced {
        output.push_str(&format!("{}={}\n", key, value));
    }

    fs::write(path, output)?;
    Ok(())
}
