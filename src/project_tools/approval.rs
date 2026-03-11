use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Auto,
    ReadOnly,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_block: Option<ApprovalBlockRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalBlockRecord {
    pub ts: u64,
    pub actor_id: String,
    pub tool_name: String,
    pub command: String,
    pub reason_code: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ApprovalManager {
    path: PathBuf,
    history_path: PathBuf,
}

impl ApprovalManager {
    pub fn new(team_dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&team_dir)?;
        Ok(Self {
            path: team_dir.join("approval_policy.json"),
            history_path: team_dir.join("approval_blocks.jsonl"),
        })
    }

    pub fn get_policy(&self) -> anyhow::Result<ApprovalPolicy> {
        if !self.path.exists() {
            let policy = ApprovalPolicy {
                mode: ApprovalMode::Auto,
                last_block: None,
            };
            self.save_policy(&policy)?;
            return Ok(policy);
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            let policy = ApprovalPolicy {
                mode: ApprovalMode::Auto,
                last_block: None,
            };
            self.save_policy(&policy)?;
            return Ok(policy);
        }
        Ok(serde_json::from_str(&content)?)
    }

    pub fn set_mode(&self, mode: ApprovalMode) -> anyhow::Result<ApprovalPolicy> {
        let mut policy = self.get_policy().unwrap_or(ApprovalPolicy {
            mode: ApprovalMode::Auto,
            last_block: None,
        });
        policy.mode = mode;
        self.save_policy(&policy)?;
        Ok(policy)
    }

    pub fn record_block(
        &self,
        actor_id: &str,
        tool_name: &str,
        command: &str,
        reason_code: &str,
        message: &str,
    ) -> anyhow::Result<ApprovalPolicy> {
        let mut policy = self.get_policy()?;
        let block = ApprovalBlockRecord {
            ts: now_secs(),
            actor_id: actor_id.trim().to_string(),
            tool_name: tool_name.trim().to_string(),
            command: command.trim().to_string(),
            reason_code: reason_code.trim().to_string(),
            message: message.trim().to_string(),
        };
        policy.last_block = Some(block.clone());
        self.save_policy(&policy)?;
        self.append_history(&block)?;
        Ok(policy)
    }

    pub fn list_recent_blocks(
        &self,
        limit: usize,
        reason_filter: Option<&str>,
    ) -> anyhow::Result<Vec<ApprovalBlockRecord>> {
        if !self.history_path.exists() {
            return Ok(Vec::new());
        }
        let count = limit.clamp(1, 100);
        let reason_filter = reason_filter
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        let content = fs::read_to_string(&self.history_path)?;
        let mut items = Vec::new();
        for line in content
            .lines()
            .rev()
            .take(count.saturating_mul(5))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(item) = serde_json::from_str::<ApprovalBlockRecord>(line)
                && reason_filter
                    .as_ref()
                    .is_none_or(|filter| item.reason_code == *filter)
            {
                items.push(item);
            }
        }
        if items.len() > count {
            let keep_from = items.len().saturating_sub(count);
            items = items.split_off(keep_from);
        }
        Ok(items)
    }

    fn save_policy(&self, policy: &ApprovalPolicy) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(policy)?)?;
        Ok(())
    }

    fn append_history(&self, block: &ApprovalBlockRecord) -> anyhow::Result<()> {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.history_path)?;
        writeln!(file, "{}", serde_json::to_string(block)?)?;
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
