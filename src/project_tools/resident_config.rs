use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResidentAgentConfig {
    pub agent_id: String,
    pub role: String,
    #[serde(default = "default_runtime_mode")]
    pub runtime_mode: String,
    #[serde(default = "default_behavior_mode")]
    pub behavior_mode: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_mission")]
    pub mission: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub forbidden: Vec<String>,
    #[serde(default = "default_daily_limit")]
    pub daily_limit: u32,
    #[serde(default = "default_period_limit")]
    pub period_limit: u32,
    #[serde(default = "default_task_soft_limit")]
    pub task_soft_limit: u32,
    #[serde(default = "default_loop_interval_ms")]
    pub loop_interval_ms: u64,
    #[serde(default)]
    pub max_parallel_override: Option<usize>,
    #[serde(default)]
    pub listen_port: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct ResidentConfigManager {
    path: PathBuf,
}

impl ResidentConfigManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        let manager = Self {
            path: dir.join("resident_agents.json"),
        };
        manager.ensure_defaults()?;
        Ok(manager)
    }

    pub fn list_all(&self) -> anyhow::Result<Vec<ResidentAgentConfig>> {
        self.load_all()
    }

    pub fn get(&self, agent_id: &str) -> anyhow::Result<Option<ResidentAgentConfig>> {
        Ok(self
            .load_all()?
            .into_iter()
            .find(|item| item.agent_id == agent_id))
    }

    pub fn enabled_agents(&self) -> anyhow::Result<Vec<ResidentAgentConfig>> {
        Ok(self
            .load_all()?
            .into_iter()
            .filter(|item| item.enabled)
            .collect())
    }

    pub fn render_summary(&self) -> anyhow::Result<String> {
        let items = self.load_all()?;
        if items.is_empty() {
            return Ok("no resident agents configured".to_string());
        }
        let mut lines = Vec::new();
        for item in items {
            lines.push(format!(
                "- {} role={} mode={} behavior={} enabled={} loop={}ms budget={}/{}/{} max_parallel_override={} port={}",
                item.agent_id,
                item.role,
                item.runtime_mode,
                item.behavior_mode,
                item.enabled,
                item.loop_interval_ms,
                item.daily_limit,
                item.period_limit,
                item.task_soft_limit,
                item.max_parallel_override
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                item.listen_port
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ));
        }
        Ok(lines.join("\n"))
    }

    fn ensure_defaults(&self) -> anyhow::Result<()> {
        let defaults = default_resident_agents();
        let mut existing = self.load_all()?;
        let mut changed = false;

        if existing.is_empty() {
            self.save_all(&defaults)?;
            return Ok(());
        }

        for default in defaults {
            if !existing
                .iter()
                .any(|item| item.agent_id == default.agent_id)
            {
                existing.push(default);
                changed = true;
            }
        }

        if changed {
            existing.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
            self.save_all(&existing)?;
        }
        Ok(())
    }

    fn load_all(&self) -> anyhow::Result<Vec<ResidentAgentConfig>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_all(&self, items: &[ResidentAgentConfig]) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(items)?)?;
        Ok(())
    }
}

fn default_enabled() -> bool {
    true
}

fn default_runtime_mode() -> String {
    "mailbox".to_string()
}

fn default_behavior_mode() -> String {
    "passive".to_string()
}

fn default_mission() -> String {
    "resident agent".to_string()
}

fn default_daily_limit() -> u32 {
    80_000
}

fn default_period_limit() -> u32 {
    20_000
}

fn default_task_soft_limit() -> u32 {
    8_000
}

fn default_loop_interval_ms() -> u64 {
    2_500
}

fn default_resident_agents() -> Vec<ResidentAgentConfig> {
    vec![
        ResidentAgentConfig {
            agent_id: "launcher".to_string(),
            role: "launcher".to_string(),
            runtime_mode: "launcher".to_string(),
            behavior_mode: "passive".to_string(),
            enabled: true,
            mission: "Launch and reconcile agent processes in dedicated command windows."
                .to_string(),
            scope: vec![
                "launch resident agents".to_string(),
                "launch worker agents".to_string(),
                "reconcile window-backed processes".to_string(),
            ],
            forbidden: vec![
                "do not execute task content".to_string(),
                "do not bypass launch registry".to_string(),
            ],
            daily_limit: 60_000,
            period_limit: 15_000,
            task_soft_limit: 6_000,
            loop_interval_ms: 800,
            max_parallel_override: None,
            listen_port: None,
        },
        ResidentAgentConfig {
            agent_id: "scheduler".to_string(),
            role: "scheduler".to_string(),
            runtime_mode: "scheduler".to_string(),
            behavior_mode: "passive".to_string(),
            enabled: true,
            mission: "Keep the task scheduler alive and supervise execution throughput."
                .to_string(),
            scope: vec![
                "supervise scheduler".to_string(),
                "watch task queue".to_string(),
                "keep orchestration alive".to_string(),
            ],
            forbidden: vec![
                "do not directly edit code".to_string(),
                "do not bypass task routing".to_string(),
            ],
            daily_limit: 90_000,
            period_limit: 20_000,
            task_soft_limit: 8_000,
            loop_interval_ms: 1_200,
            max_parallel_override: None,
            listen_port: None,
        },
        ResidentAgentConfig {
            agent_id: "critic".to_string(),
            role: "critic".to_string(),
            runtime_mode: "critic".to_string(),
            behavior_mode: "passive".to_string(),
            enabled: true,
            mission: "Continuously review proposals and convert valid improvements into tasks."
                .to_string(),
            scope: vec![
                "review proposals".to_string(),
                "convert improvements".to_string(),
                "avoid duplicates".to_string(),
            ],
            forbidden: vec![
                "do not directly edit code".to_string(),
                "do not bypass proposal workflow".to_string(),
            ],
            daily_limit: 80_000,
            period_limit: 20_000,
            task_soft_limit: 8_000,
            loop_interval_ms: 2_500,
            max_parallel_override: None,
            listen_port: None,
        },
        ResidentAgentConfig {
            agent_id: "surface-collector".to_string(),
            role: "ui_planner".to_string(),
            runtime_mode: "mailbox".to_string(),
            behavior_mode: "ui_surface_planning".to_string(),
            enabled: true,
            mission: "Collect system capabilities and consolidate them into a durable ui surface specification."
                .to_string(),
            scope: vec![
                "collect system capabilities".to_string(),
                "maintain ui surface spec".to_string(),
                "track protocol-backed page information".to_string(),
            ],
            forbidden: vec![
                "do not render final pages".to_string(),
                "do not invent unsupported protocol actions".to_string(),
            ],
            daily_limit: 90_000,
            period_limit: 20_000,
            task_soft_limit: 10_000,
            loop_interval_ms: 2_500,
            max_parallel_override: None,
            listen_port: None,
        },
        ResidentAgentConfig {
            agent_id: "ui".to_string(),
            role: "ui".to_string(),
            runtime_mode: "mailbox".to_string(),
            behavior_mode: "ui_request".to_string(),
            enabled: true,
            mission: "Own UI generation requests and remain available for interface-focused work."
                .to_string(),
            scope: vec![
                "accept ui work".to_string(),
                "track ui-oriented requests".to_string(),
                "coordinate with developer/design workers".to_string(),
            ],
            forbidden: vec![
                "do not claim backend-only work".to_string(),
                "do not bypass task routing".to_string(),
            ],
            daily_limit: 110_000,
            period_limit: 30_000,
            task_soft_limit: 14_000,
            loop_interval_ms: 2_500,
            max_parallel_override: None,
            listen_port: Some(3847),
        },
        ResidentAgentConfig {
            agent_id: "concierge".to_string(),
            role: "concierge".to_string(),
            runtime_mode: "mailbox".to_string(),
            behavior_mode: "concierge_request".to_string(),
            enabled: true,
            mission: "Translate inbound user or operator requests into structured tasks."
                .to_string(),
            scope: vec![
                "accept user requests".to_string(),
                "turn requests into tasks".to_string(),
                "acknowledge intake".to_string(),
            ],
            forbidden: vec![
                "do not implement code directly".to_string(),
                "do not bypass task routing".to_string(),
            ],
            daily_limit: 100_000,
            period_limit: 25_000,
            task_soft_limit: 10_000,
            loop_interval_ms: 2_000,
            max_parallel_override: None,
            listen_port: None,
        },
        ResidentAgentConfig {
            agent_id: "reviewer".to_string(),
            role: "reviewer".to_string(),
            runtime_mode: "mailbox".to_string(),
            behavior_mode: "proposal_triage".to_string(),
            enabled: true,
            mission: "Triages blocked or failed work into structured proposal candidates."
                .to_string(),
            scope: vec![
                "watch blocked work".to_string(),
                "capture proposal candidates".to_string(),
                "feed critic queue".to_string(),
            ],
            forbidden: vec![
                "do not edit code".to_string(),
                "do not bypass proposal workflow".to_string(),
            ],
            daily_limit: 70_000,
            period_limit: 18_000,
            task_soft_limit: 7_000,
            loop_interval_ms: 2_500,
            max_parallel_override: None,
            listen_port: None,
        },
    ]
}
