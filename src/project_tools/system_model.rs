use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use super::{ProjectContext, classify_energy};
use crate::resident_agents::resident_listen_port;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemModel {
    pub generated_at: f64,
    pub summary: SystemSummary,
    pub alerts: Vec<SystemAlert>,
    pub protocols: Vec<SystemProtocol>,
    pub residents: Vec<SystemResident>,
    pub recent_prompt_changes: Vec<SystemPromptChange>,
    pub tasks: Vec<SystemTask>,
    pub proposals: Vec<SystemProposal>,
    pub decisions: Vec<SystemDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSummary {
    pub resident_count: usize,
    pub pending_tasks: usize,
    pub running_tasks: usize,
    pub blocked_tasks: usize,
    pub completed_tasks: usize,
    pub open_proposals: usize,
    pub recent_decisions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemAlert {
    pub severity: String,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemProtocol {
    pub id: String,
    pub transport: String,
    pub method: String,
    pub path: String,
    pub purpose: String,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub requires_confirmation: bool,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub request_fields: Vec<SystemProtocolField>,
    #[serde(default)]
    pub response_fields: Vec<SystemProtocolField>,
    #[serde(default)]
    pub supported_sections: Vec<String>,
    #[serde(default)]
    pub supported_sources: Vec<String>,
    #[serde(default)]
    pub event_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemProtocolField {
    pub name: String,
    pub required: bool,
    pub field_type: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemResident {
    pub agent_id: String,
    pub role: String,
    pub runtime_mode: String,
    pub behavior_mode: String,
    pub status: String,
    pub note: String,
    pub backlog: usize,
    pub loop_ms: u64,
    pub last_error: String,
    pub last_msg: String,
    pub port: u16,
    pub energy: String,
    pub budget_used: u32,
    pub budget_limit: u32,
    pub last_action: String,
    pub last_summary: String,
    pub prompt_strategy: String,
    pub prompt_trigger: String,
    pub prompt_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPromptChange {
    pub recorded_at: f64,
    pub agent_scope: String,
    pub agent_id: String,
    pub file_path: String,
    pub strategy: String,
    pub trigger: String,
    pub summary: String,
    pub diff_summary: String,
    pub added_lines: Vec<String>,
    pub removed_lines: Vec<String>,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTask {
    pub id: u64,
    pub subject: String,
    pub status: String,
    pub priority: String,
    pub role: String,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemProposal {
    pub id: u64,
    pub title: String,
    pub priority: String,
    pub score: i32,
    pub source: String,
    pub trigger: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemDecision {
    pub agent_id: String,
    pub action: String,
    pub summary: String,
    pub reason: String,
    pub task_id: Option<u64>,
    pub proposal_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct SystemModelManager {
    path: PathBuf,
}

impl SystemModelManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("system_model.json"),
        })
    }

    pub fn snapshot(&self) -> anyhow::Result<Option<SystemModel>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&content)?))
    }

    pub fn rebuild(&self, project: &ProjectContext) -> anyhow::Result<SystemModel> {
        let tasks = project.tasks().list_records()?;
        let proposals = project.proposals().list_open(8)?;
        let decisions = project.decisions().list_recent_records(12)?;
        let prompt_changes = project.prompt_history().list_recent(8)?;
        let enabled_residents = project.residents().enabled_agents()?;

        let pending_tasks = tasks.iter().filter(|item| item.status == "pending").count();
        let running_tasks = tasks
            .iter()
            .filter(|item| item.status == "in_progress")
            .count();
        let blocked_tasks = tasks.iter().filter(|item| item.status == "blocked").count();
        let completed_tasks = tasks
            .iter()
            .filter(|item| item.status == "completed")
            .count();

        let residents = enabled_residents
            .into_iter()
            .map(|resident| {
                let port = resident_listen_port(&resident);
                let agent_state = project.agents().state(&resident.agent_id).ok().flatten();
                let runtime = project
                    .resident_runtime()
                    .snapshot(&resident.agent_id)
                    .ok()
                    .flatten();
                let cursor = project
                    .resident_runtime()
                    .mailbox_cursor(&resident.agent_id)
                    .unwrap_or(0);
                let backlog = project
                    .mailbox()
                    .backlog_count(&resident.agent_id, cursor)
                    .unwrap_or(0);
                let budget = project
                    .budgets()
                    .snapshot(&resident.agent_id)
                    .ok()
                    .flatten();
                let latest = project
                    .decisions()
                    .latest_for_agent(&resident.agent_id)
                    .ok()
                    .flatten();
                let prompt_recovery = if resident.role == "ui" {
                    project.ui_surface().ui_prompt_recovery().ok().flatten()
                } else if resident.behavior_mode == "ui_surface_planning" {
                    project
                        .ui_surface()
                        .planner_prompt_recovery()
                        .ok()
                        .flatten()
                } else {
                    None
                };
                SystemResident {
                    agent_id: resident.agent_id,
                    role: resident.role,
                    runtime_mode: resident.runtime_mode,
                    behavior_mode: resident.behavior_mode,
                    status: agent_state
                        .as_ref()
                        .map(|item| item.status.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    note: agent_state
                        .and_then(|item| item.note)
                        .unwrap_or_else(|| "-".to_string()),
                    backlog,
                    loop_ms: runtime
                        .as_ref()
                        .map(|item| item.last_loop_duration_ms)
                        .unwrap_or(0),
                    last_error: runtime
                        .as_ref()
                        .and_then(|item| item.last_error.clone())
                        .unwrap_or_default(),
                    last_msg: runtime
                        .as_ref()
                        .and_then(|item| item.last_processed_msg_id.clone())
                        .unwrap_or_default(),
                    port,
                    energy: budget
                        .as_ref()
                        .map(classify_energy)
                        .map(|mode| format!("{:?}", mode))
                        .unwrap_or_else(|| "Unknown".to_string()),
                    budget_used: budget.as_ref().map(|item| item.used_today).unwrap_or(0),
                    budget_limit: budget.as_ref().map(|item| item.daily_limit).unwrap_or(0),
                    last_action: latest
                        .as_ref()
                        .map(|item| item.action.clone())
                        .unwrap_or_default(),
                    last_summary: latest
                        .as_ref()
                        .map(|item| item.summary.clone())
                        .unwrap_or_default(),
                    prompt_strategy: prompt_recovery
                        .as_ref()
                        .map(|item| item.strategy.clone())
                        .unwrap_or_default(),
                    prompt_trigger: prompt_recovery
                        .as_ref()
                        .map(|item| item.trigger.clone())
                        .unwrap_or_default(),
                    prompt_note: prompt_recovery
                        .as_ref()
                        .map(|item| item.note.clone())
                        .unwrap_or_default(),
                }
            })
            .collect::<Vec<_>>();

        let task_feed = tasks
            .iter()
            .rev()
            .take(12)
            .map(|item| SystemTask {
                id: item.id,
                subject: item.subject.clone(),
                status: item.status.clone(),
                priority: item.priority.clone(),
                role: item.role_hint.clone(),
                owner: item.owner.clone(),
            })
            .collect::<Vec<_>>();

        let proposal_feed = proposals
            .into_iter()
            .map(|item| SystemProposal {
                id: item.id,
                title: item.title,
                priority: item.priority,
                score: item.score,
                source: item.source_agent,
                trigger: item.trigger,
            })
            .collect::<Vec<_>>();

        let decision_feed = decisions
            .into_iter()
            .rev()
            .map(|item| SystemDecision {
                agent_id: item.agent_id,
                action: item.action,
                summary: item.summary,
                reason: item.reason,
                task_id: item.task_id,
                proposal_id: item.proposal_id,
            })
            .collect::<Vec<_>>();

        let prompt_change_feed = prompt_changes
            .into_iter()
            .rev()
            .map(|item| SystemPromptChange {
                recorded_at: item.recorded_at,
                agent_scope: item.agent_scope,
                agent_id: item.agent_id,
                file_path: item.file_path,
                strategy: item.strategy,
                trigger: item.trigger,
                summary: item.summary,
                diff_summary: item.diff_summary,
                added_lines: item.added_lines,
                removed_lines: item.removed_lines,
                before: item.before,
                after: item.after,
            })
            .collect::<Vec<_>>();

        let mut alerts = Vec::new();
        for resident in &residents {
            if resident.status == "blocked" {
                alerts.push(SystemAlert {
                    severity: "high".to_string(),
                    summary: format!("{} is blocked", resident.agent_id),
                    detail: resident.note.clone(),
                });
            }
            if !resident.last_error.is_empty() {
                alerts.push(SystemAlert {
                    severity: "high".to_string(),
                    summary: format!("{} has runtime errors", resident.agent_id),
                    detail: resident.last_error.clone(),
                });
            }
            if resident.backlog > 10 {
                alerts.push(SystemAlert {
                    severity: "medium".to_string(),
                    summary: format!("{} backlog is growing", resident.agent_id),
                    detail: format!("backlog={}", resident.backlog),
                });
            }
        }
        if blocked_tasks > 0 {
            alerts.push(SystemAlert {
                severity: "high".to_string(),
                summary: "blocked tasks detected".to_string(),
                detail: format!("blocked_tasks={}", blocked_tasks),
            });
        }

        let model = SystemModel {
            generated_at: crate::project_tools::util::now_secs_f64(),
            summary: SystemSummary {
                resident_count: residents.len(),
                pending_tasks,
                running_tasks,
                blocked_tasks,
                completed_tasks,
                open_proposals: proposal_feed.len(),
                recent_decisions: decision_feed.len(),
            },
            alerts,
            protocols: vec![
                SystemProtocol {
                    id: "ui.status".to_string(),
                    transport: "http".to_string(),
                    method: "GET".to_string(),
                    path: "/api/status".to_string(),
                    purpose: "load the current system model and ui schema for the dashboard"
                        .to_string(),
                    readonly: true,
                    requires_confirmation: false,
                    targets: Vec::new(),
                    request_fields: Vec::new(),
                    response_fields: vec![
                        SystemProtocolField {
                            name: "schema".to_string(),
                            required: true,
                            field_type: "object".to_string(),
                            description: "current ui schema generated for the control surface"
                                .to_string(),
                        },
                        SystemProtocolField {
                            name: "model.summary".to_string(),
                            required: true,
                            field_type: "object".to_string(),
                            description: "system level summary metrics".to_string(),
                        },
                        SystemProtocolField {
                            name: "model.alerts".to_string(),
                            required: false,
                            field_type: "array".to_string(),
                            description: "current system alerts".to_string(),
                        },
                        SystemProtocolField {
                            name: "model.residents".to_string(),
                            required: true,
                            field_type: "array".to_string(),
                            description: "resident agent runtime state".to_string(),
                        },
                        SystemProtocolField {
                            name: "model.recent_prompt_changes".to_string(),
                            required: false,
                            field_type: "array".to_string(),
                            description: "recent automatic prompt recovery changes".to_string(),
                        },
                        SystemProtocolField {
                            name: "model.tasks".to_string(),
                            required: false,
                            field_type: "array".to_string(),
                            description: "recent task feed".to_string(),
                        },
                        SystemProtocolField {
                            name: "model.proposals".to_string(),
                            required: false,
                            field_type: "array".to_string(),
                            description: "open proposal feed".to_string(),
                        },
                        SystemProtocolField {
                            name: "model.decisions".to_string(),
                            required: false,
                            field_type: "array".to_string(),
                            description: "recent governance decisions".to_string(),
                        },
                    ],
                    supported_sections: vec![
                        "metrics".to_string(),
                        "alerts".to_string(),
                        "residents".to_string(),
                        "tasks".to_string(),
                        "proposals".to_string(),
                        "decisions".to_string(),
                    ],
                    supported_sources: vec![
                        "summary".to_string(),
                        "alerts".to_string(),
                        "residents".to_string(),
                        "tasks".to_string(),
                        "proposals".to_string(),
                        "decisions".to_string(),
                    ],
                    event_types: vec!["system.snapshot".to_string()],
                },
                SystemProtocol {
                    id: "ui.request.dispatch".to_string(),
                    transport: "http".to_string(),
                    method: "POST".to_string(),
                    path: "/api/request".to_string(),
                    purpose: "dispatch a request from the control surface into the agent system"
                        .to_string(),
                    readonly: false,
                    requires_confirmation: false,
                    targets: vec![
                        "ui".to_string(),
                        "concierge".to_string(),
                        "reviewer".to_string(),
                    ],
                    request_fields: vec![
                        SystemProtocolField {
                            name: "message".to_string(),
                            required: true,
                            field_type: "string".to_string(),
                            description: "request content entered by the user".to_string(),
                        },
                        SystemProtocolField {
                            name: "priority".to_string(),
                            required: false,
                            field_type: "enum(critical|high|medium|low)".to_string(),
                            description: "dispatch priority".to_string(),
                        },
                        SystemProtocolField {
                            name: "target".to_string(),
                            required: true,
                            field_type: "enum(ui|concierge|reviewer)".to_string(),
                            description: "resident agent target".to_string(),
                        },
                    ],
                    response_fields: vec![
                        SystemProtocolField {
                            name: "ok".to_string(),
                            required: true,
                            field_type: "bool".to_string(),
                            description: "whether the request was accepted".to_string(),
                        },
                        SystemProtocolField {
                            name: "message".to_string(),
                            required: true,
                            field_type: "string".to_string(),
                            description: "acceptance or error summary".to_string(),
                        },
                    ],
                    supported_sections: vec!["composer".to_string()],
                    supported_sources: vec!["composer".to_string()],
                    event_types: vec![
                        "request.accepted".to_string(),
                        "request.rejected".to_string(),
                    ],
                },
            ],
            residents,
            recent_prompt_changes: prompt_change_feed,
            tasks: task_feed,
            proposals: proposal_feed,
            decisions: decision_feed,
        };
        self.save(&model)?;
        Ok(model)
    }

    pub fn save(&self, model: &SystemModel) -> anyhow::Result<()> {
        fs::write(&self.path, serde_json::to_string_pretty(model)?)?;
        Ok(())
    }
}
