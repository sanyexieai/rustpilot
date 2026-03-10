use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResidentRuntimeState {
    pub agent_id: String,
    #[serde(default)]
    pub mailbox_cursor: usize,
    #[serde(default)]
    pub last_seen_at: f64,
    #[serde(default)]
    pub last_loop_duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_processed_msg_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResidentRuntimeManager {
    path: PathBuf,
}

impl ResidentRuntimeManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("resident_runtime.json"),
        })
    }

    pub fn mailbox_cursor(&self, agent_id: &str) -> anyhow::Result<usize> {
        Ok(self
            .load_all()?
            .into_iter()
            .find(|item| item.agent_id == agent_id)
            .map(|item| item.mailbox_cursor)
            .unwrap_or(0))
    }

    pub fn set_mailbox_cursor(&self, agent_id: &str, cursor: usize) -> anyhow::Result<()> {
        let mut items = self.load_all()?;
        if let Some(existing) = items.iter_mut().find(|item| item.agent_id == agent_id) {
            existing.mailbox_cursor = cursor;
        } else {
            items.push(ResidentRuntimeState {
                agent_id: agent_id.to_string(),
                mailbox_cursor: cursor,
                last_seen_at: 0.0,
                last_loop_duration_ms: 0,
                last_processed_msg_id: None,
                last_error: None,
            });
        }
        items.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        self.save_all(&items)
    }

    pub fn snapshot(&self, agent_id: &str) -> anyhow::Result<Option<ResidentRuntimeState>> {
        Ok(self
            .load_all()?
            .into_iter()
            .find(|item| item.agent_id == agent_id))
    }

    pub fn update_loop_status(
        &self,
        agent_id: &str,
        mailbox_cursor: usize,
        last_processed_msg_id: Option<&str>,
        last_loop_duration_ms: u64,
        last_error: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut items = self.load_all()?;
        if let Some(existing) = items.iter_mut().find(|item| item.agent_id == agent_id) {
            existing.mailbox_cursor = mailbox_cursor;
            existing.last_seen_at = now_secs_f64();
            existing.last_loop_duration_ms = last_loop_duration_ms;
            if let Some(msg_id) = last_processed_msg_id {
                existing.last_processed_msg_id = Some(msg_id.to_string());
            }
            existing.last_error = last_error.map(|value| value.to_string());
        } else {
            items.push(ResidentRuntimeState {
                agent_id: agent_id.to_string(),
                mailbox_cursor,
                last_seen_at: now_secs_f64(),
                last_loop_duration_ms,
                last_processed_msg_id: last_processed_msg_id.map(|value| value.to_string()),
                last_error: last_error.map(|value| value.to_string()),
            });
        }
        items.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        self.save_all(&items)
    }

    fn load_all(&self) -> anyhow::Result<Vec<ResidentRuntimeState>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_all(&self, items: &[ResidentRuntimeState]) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(items)?)?;
        Ok(())
    }
}
