use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetLedger {
    pub agent_id: String,
    pub daily_limit: u32,
    pub period_limit: u32,
    pub task_soft_limit: u32,
    pub used_today: u32,
    pub used_in_period: u32,
    pub reserved: u32,
    pub last_reset_day: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EnergyMode {
    Healthy,
    Constrained,
    Low,
    Exhausted,
}

#[derive(Debug, Clone)]
pub struct BudgetManager {
    path: PathBuf,
}

impl BudgetManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("budgets.json"),
        })
    }

    pub fn ensure_ledger(
        &self,
        agent_id: &str,
        daily_limit: u32,
        period_limit: u32,
        task_soft_limit: u32,
    ) -> anyhow::Result<()> {
        let mut items = self.load_all()?;
        let today = epoch_day();
        if let Some(item) = items.iter_mut().find(|item| item.agent_id == agent_id) {
            reset_if_needed(item, today);
            item.daily_limit = daily_limit;
            item.period_limit = period_limit;
            item.task_soft_limit = task_soft_limit;
        } else {
            items.push(BudgetLedger {
                agent_id: agent_id.to_string(),
                daily_limit,
                period_limit,
                task_soft_limit,
                used_today: 0,
                used_in_period: 0,
                reserved: 0,
                last_reset_day: today,
            });
        }
        items.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        self.save_all(&items)
    }

    pub fn record_usage(&self, agent_id: &str, amount: u32) -> anyhow::Result<()> {
        let mut items = self.load_all()?;
        let today = epoch_day();
        let Some(item) = items.iter_mut().find(|entry| entry.agent_id == agent_id) else {
            anyhow::bail!("missing budget ledger for agent {}", agent_id);
        };
        reset_if_needed(item, today);
        item.used_today = item.used_today.saturating_add(amount);
        item.used_in_period = item.used_in_period.saturating_add(amount);
        self.save_all(&items)
    }

    pub fn snapshot(&self, agent_id: &str) -> anyhow::Result<Option<BudgetLedger>> {
        let mut items = self.load_all()?;
        let today = epoch_day();
        let mut changed = false;
        for item in &mut items {
            let before = item.last_reset_day;
            reset_if_needed(item, today);
            if item.last_reset_day != before {
                changed = true;
            }
        }
        if changed {
            self.save_all(&items)?;
        }
        Ok(items.into_iter().find(|item| item.agent_id == agent_id))
    }

    pub fn energy_mode(&self, agent_id: &str) -> anyhow::Result<Option<EnergyMode>> {
        Ok(self.snapshot(agent_id)?.map(|item| classify_energy(&item)))
    }

    fn load_all(&self) -> anyhow::Result<Vec<BudgetLedger>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_all(&self, items: &[BudgetLedger]) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(items)?)?;
        Ok(())
    }
}

pub fn classify_energy(item: &BudgetLedger) -> EnergyMode {
    let remaining = item.daily_limit.saturating_sub(item.used_today);
    let ratio = if item.daily_limit == 0 {
        0.0
    } else {
        remaining as f32 / item.daily_limit as f32
    };
    if ratio <= 0.10 {
        EnergyMode::Exhausted
    } else if ratio <= 0.20 {
        EnergyMode::Low
    } else if ratio <= 0.50 {
        EnergyMode::Constrained
    } else {
        EnergyMode::Healthy
    }
}

fn reset_if_needed(item: &mut BudgetLedger, today: u64) {
    if item.last_reset_day != today {
        item.used_today = 0;
        item.used_in_period = 0;
        item.reserved = 0;
        item.last_reset_day = today;
    }
}

fn epoch_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}
