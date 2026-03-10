use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionRecord {
    pub agent_id: String,
    pub trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
    pub summary: String,
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    pub escalate: bool,
    pub created_at: f64,
}

#[derive(Debug, Clone)]
pub struct ReflectionManager {
    path: PathBuf,
}

impl ReflectionManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("reflections.jsonl"),
        })
    }

    pub fn append(
        &self,
        agent_id: &str,
        trigger: &str,
        task_id: Option<u64>,
        summary: &str,
        issues: &[&str],
        next_action: Option<&str>,
        escalate: bool,
    ) -> anyhow::Result<String> {
        let record = ReflectionRecord {
            agent_id: agent_id.trim().to_string(),
            trigger: trigger.trim().to_string(),
            task_id,
            summary: summary.trim().to_string(),
            issues: issues.iter().map(|item| item.to_string()).collect(),
            next_action: next_action
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string),
            escalate,
            created_at: now_secs_f64(),
        };
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(serde_json::to_string_pretty(&record)?)
    }

    pub fn list_recent(&self, limit: usize) -> anyhow::Result<String> {
        if !self.path.exists() {
            return Ok("[]".to_string());
        }
        let content = fs::read_to_string(&self.path)?;
        let mut items = Vec::new();
        for line in content
            .lines()
            .rev()
            .take(limit.clamp(1, 200))
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
