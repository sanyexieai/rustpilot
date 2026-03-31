use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRuntimeRecord {
    pub tenant_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub pid: u32,
    pub status: String,
    pub started_at: f64,
    pub last_seen_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone)]
pub struct TenantRuntimeRegistryManager {
    path: PathBuf,
}

impl TenantRuntimeRegistryManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("tenant_runtime_registry.json"),
        })
    }

    pub fn get(
        &self,
        tenant_id: &str,
        user_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<TenantRuntimeRecord>> {
        Ok(self.load_all()?.into_iter().find(|item| {
            item.tenant_id == tenant_id && item.user_id == user_id && item.agent_id == agent_id
        }))
    }

    pub fn upsert(
        &self,
        tenant_id: &str,
        user_id: &str,
        agent_id: &str,
        pid: u32,
        status: &str,
    ) -> anyhow::Result<TenantRuntimeRecord> {
        let mut items = self.load_all()?;
        let now = now_secs_f64();
        if let Some(existing) = items.iter_mut().find(|item| {
            item.tenant_id == tenant_id && item.user_id == user_id && item.agent_id == agent_id
        }) {
            if existing.pid != pid {
                existing.started_at = now;
            }
            existing.pid = pid;
            existing.status = status.to_string();
            existing.last_seen_at = now;
            existing.updated_at = now;
            let record = existing.clone();
            self.save_all(&items)?;
            return Ok(record);
        }
        let record = TenantRuntimeRecord {
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            agent_id: agent_id.to_string(),
            pid,
            status: status.to_string(),
            started_at: now,
            last_seen_at: now,
            updated_at: now,
        };
        items.push(record.clone());
        items.sort_by(|a, b| {
            a.tenant_id
                .cmp(&b.tenant_id)
                .then_with(|| a.user_id.cmp(&b.user_id))
                .then_with(|| a.agent_id.cmp(&b.agent_id))
        });
        self.save_all(&items)?;
        Ok(record)
    }

    fn load_all(&self) -> anyhow::Result<Vec<TenantRuntimeRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        match serde_json::from_str(&content) {
            Ok(items) => Ok(items),
            Err(_) => {
                let repaired = repair_registry_json(&content)?;
                let items = serde_json::from_str(&repaired)?;
                fs::write(&self.path, &repaired)?;
                Ok(items)
            }
        }
    }

    fn save_all(&self, items: &[TenantRuntimeRecord]) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(items)?)?;
        Ok(())
    }
}

fn repair_registry_json(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with('[') {
        anyhow::bail!("tenant runtime registry is not a JSON array");
    }
    let end = trimmed
        .rfind(']')
        .ok_or_else(|| anyhow::anyhow!("tenant runtime registry is missing closing bracket"))?;
    let candidate = trimmed[..=end].trim();
    serde_json::from_str::<Vec<TenantRuntimeRecord>>(candidate)?;
    Ok(candidate.to_string())
}
