use crate::constants::LLM_TIMEOUT_SECS;
use std::path::{Path, PathBuf};
use std::process::Command;

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
