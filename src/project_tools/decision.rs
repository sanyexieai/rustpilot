use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub ts: f64,
    pub agent_id: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<u64>,
    pub summary: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct DecisionManager {
    path: PathBuf,
}

impl DecisionManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("decisions.jsonl"),
        })
    }

    pub fn append(
        &self,
        agent_id: &str,
        action: &str,
        task_id: Option<u64>,
        proposal_id: Option<u64>,
        summary: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        let record = DecisionRecord {
            ts: now_secs_f64(),
            agent_id: agent_id.trim().to_string(),
            action: action.trim().to_string(),
            task_id,
            proposal_id,
            summary: summary.trim().to_string(),
            reason: reason.trim().to_string(),
        };
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }

    pub fn list_recent(&self, limit: usize) -> anyhow::Result<String> {
        let items = self.load_recent(limit)?;
        Ok(serde_json::to_string_pretty(&items)?)
    }

    pub fn list_recent_records(&self, limit: usize) -> anyhow::Result<Vec<DecisionRecord>> {
        self.load_recent(limit)
    }

    pub fn list_related(
        &self,
        task_id: Option<u64>,
        proposal_id: Option<u64>,
        agent_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<DecisionRecord>> {
        let agent_id = agent_id.map(str::trim).filter(|value| !value.is_empty());
        let mut items = self.load_recent(500)?;
        items.retain(|item| {
            (task_id.is_none() || item.task_id == task_id)
                && (proposal_id.is_none() || item.proposal_id == proposal_id)
                && (agent_id.is_none() || Some(item.agent_id.as_str()) == agent_id)
        });
        let count = limit.clamp(1, 50);
        let keep_from = items.len().saturating_sub(count);
        Ok(items.split_off(keep_from))
    }

    pub fn latest_for_agent(&self, agent_id: &str) -> anyhow::Result<Option<DecisionRecord>> {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return Ok(None);
        }
        Ok(self
            .load_recent(500)?
            .into_iter()
            .rev()
            .find(|item| item.agent_id == agent_id))
    }

    fn load_recent(&self, limit: usize) -> anyhow::Result<Vec<DecisionRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let count = limit.clamp(1, 500);
        let content = fs::read_to_string(&self.path)?;
        let mut items = Vec::new();
        for line in content
            .lines()
            .rev()
            .take(count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<DecisionRecord>(line) {
                items.push(value);
            }
        }
        Ok(items)
    }
}
