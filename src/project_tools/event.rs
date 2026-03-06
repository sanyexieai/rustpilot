use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    event: String,
    ts: f64,
    task: serde_json::Value,
    worktree: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventBus {
    path: PathBuf,
}

impl EventBus {
    pub fn new(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(&path, "")?;
        }
        Ok(Self { path })
    }

    pub fn emit(
        &self,
        event: &str,
        task: serde_json::Value,
        worktree: serde_json::Value,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let payload = EventRecord {
            event: event.to_string(),
            ts: now_secs_f64(),
            task,
            worktree,
            error,
        };
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&payload)?)?;
        Ok(())
    }

    pub fn list_recent(&self, limit: usize) -> anyhow::Result<String> {
        let count = limit.clamp(1, 200);
        let content = fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = content.lines().collect();
        let mut items = Vec::new();
        for line in lines
            .into_iter()
            .rev()
            .take(count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => items.push(value),
                Err(_) => items.push(json!({ "event": "parse_error", "raw": line })),
            }
        }
        Ok(serde_json::to_string_pretty(&items)?)
    }
}
