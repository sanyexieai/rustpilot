use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub id: u64,
    pub source_agent: String,
    pub trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub issues: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_action: Option<String>,
    pub priority: String,
    pub score: i32,
    pub status: String,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone)]
pub struct ProposalManager {
    path: PathBuf,
}

impl ProposalManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("proposals.json"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        source_agent: &str,
        trigger: &str,
        task_id: Option<u64>,
        title: &str,
        summary: &str,
        issues: &[&str],
        suggested_action: Option<&str>,
    ) -> anyhow::Result<String> {
        let mut items = self.load_all()?;
        let now = now_secs_f64();
        let score = score_proposal(source_agent, trigger, task_id, issues);
        let priority = classify_priority(score).to_string();
        if let Some(existing) = items.iter_mut().find(|item| {
            item.status == "open"
                && item.source_agent == source_agent.trim()
                && item.trigger == trigger.trim()
                && item.task_id == task_id
                && item.title == title.trim()
        }) {
            existing.summary = summary.trim().to_string();
            existing.issues = issues.iter().map(|item| item.to_string()).collect();
            existing.suggested_action = suggested_action
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string);
            existing.priority = priority.clone();
            existing.score = score;
            existing.updated_at = now;
            let updated = existing.clone();
            self.save_all(&items)?;
            return Ok(serde_json::to_string_pretty(&updated)?);
        }
        let record = ProposalRecord {
            id: self.next_id(&items),
            source_agent: source_agent.trim().to_string(),
            trigger: trigger.trim().to_string(),
            task_id,
            title: title.trim().to_string(),
            summary: summary.trim().to_string(),
            issues: issues.iter().map(|item| item.to_string()).collect(),
            suggested_action: suggested_action
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string),
            priority,
            score,
            status: "open".to_string(),
            created_at: now,
            updated_at: now,
        };
        items.push(record.clone());
        items.sort_by_key(|item| item.id);
        self.save_all(&items)?;
        Ok(serde_json::to_string_pretty(&record)?)
    }

    pub fn list_recent(&self, limit: usize) -> anyhow::Result<String> {
        let mut items = self.load_all()?;
        if items.is_empty() {
            return Ok("[]".to_string());
        }
        items.sort_by_key(|item| item.id);
        let selected: Vec<ProposalRecord> = items
            .into_iter()
            .rev()
            .take(limit.clamp(1, 200))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(serde_json::to_string_pretty(&selected)?)
    }

    pub fn list_open(&self, limit: usize) -> anyhow::Result<Vec<ProposalRecord>> {
        let mut items: Vec<ProposalRecord> = self
            .load_all()?
            .into_iter()
            .filter(|item| item.status == "open")
            .collect();
        items.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.created_at.total_cmp(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(items.into_iter().take(limit.clamp(1, 200)).collect())
    }

    pub fn update_status(&self, proposal_id: u64, status: &str) -> anyhow::Result<String> {
        let mut items = self.load_all()?;
        let now = now_secs_f64();
        let Some(item) = items.iter_mut().find(|item| item.id == proposal_id) else {
            anyhow::bail!("proposal {} does not exist", proposal_id);
        };
        if !matches!(status, "open" | "converted" | "rejected") {
            anyhow::bail!("invalid proposal status: {}", status);
        }
        item.status = status.to_string();
        item.updated_at = now;
        let updated = item.clone();
        self.save_all(&items)?;
        Ok(serde_json::to_string_pretty(&updated)?)
    }

    fn next_id(&self, items: &[ProposalRecord]) -> u64 {
        items.iter().map(|item| item.id).max().unwrap_or(0) + 1
    }

    fn load_all(&self) -> anyhow::Result<Vec<ProposalRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_all(&self, items: &[ProposalRecord]) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(items)?)?;
        Ok(())
    }
}

fn score_proposal(source_agent: &str, trigger: &str, task_id: Option<u64>, issues: &[&str]) -> i32 {
    let mut score = 0i32;

    score += match trigger.trim() {
        "task.failed" => 90,
        "task.blocked" => 70,
        "critic.pass" => 40,
        value if value.contains("focus") => 10,
        value if value.contains("command") => 15,
        value if value.contains("worker.spawn") => 35,
        value if value.contains("team.start") => 20,
        _ => 25,
    };

    score += match source_agent.trim() {
        "critic" => 40,
        "team-manager" => 30,
        "lead" => 20,
        value if value.starts_with("teammate-") => 35,
        _ => 10,
    };

    if task_id.is_some() {
        score += 15;
    }

    score += (issues.len() as i32).min(5) * 5;
    score
}

fn classify_priority(score: i32) -> &'static str {
    if score >= 120 {
        "critical"
    } else if score >= 80 {
        "high"
    } else if score >= 45 {
        "medium"
    } else {
        "low"
    }
}
