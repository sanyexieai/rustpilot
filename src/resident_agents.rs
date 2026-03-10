use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::project_tools::{ProjectContext, ResidentAgentConfig};
use crate::team::TeamRuntime;
use crate::ui_server::spawn_ui_server;

pub struct AgentSupervisor {
    repo_root: PathBuf,
    max_parallel: usize,
    children: HashMap<String, Child>,
}

impl AgentSupervisor {
    pub fn start_defaults(repo_root: PathBuf, max_parallel: usize) -> anyhow::Result<Self> {
        let mut supervisor = Self {
            repo_root,
            max_parallel: max_parallel.max(1),
            children: HashMap::new(),
        };
        supervisor.reconcile()?;
        Ok(supervisor)
    }

    pub fn reconcile(&mut self) -> anyhow::Result<()> {
        let project = ProjectContext::new(self.repo_root.clone())?;
        let configs = project.residents().enabled_agents()?;
        let active_ids = configs
            .iter()
            .map(|item| item.agent_id.clone())
            .collect::<Vec<_>>();

        let stale_ids = self
            .children
            .keys()
            .filter(|agent_id| !active_ids.iter().any(|active| active == *agent_id))
            .cloned()
            .collect::<Vec<_>>();
        for agent_id in stale_ids {
            self.stop_agent(&agent_id);
        }

        for config in configs {
            let agent_id = config.agent_id.clone();
            let should_spawn = match self.children.get_mut(&agent_id) {
                Some(child) => child.try_wait()?.is_some(),
                None => true,
            };
            if should_spawn {
                self.children.remove(&agent_id);
                let child = spawn_resident_agent(&self.repo_root, &config, self.max_parallel)?;
                self.children.insert(agent_id, child);
            }
        }
        Ok(())
    }

    pub fn ensure_running(&mut self, agent_id: &str) -> anyhow::Result<()> {
        let project = ProjectContext::new(self.repo_root.clone())?;
        let Some(config) = project.residents().get(agent_id)? else {
            anyhow::bail!("resident agent '{}' is not configured", agent_id);
        };
        if !config.enabled {
            anyhow::bail!("resident agent '{}' is disabled", agent_id);
        }
        let should_spawn = match self.children.get_mut(agent_id) {
            Some(child) => child.try_wait()?.is_some(),
            None => true,
        };
        if should_spawn {
            self.children.remove(agent_id);
            let child = spawn_resident_agent(&self.repo_root, &config, self.max_parallel)?;
            self.children.insert(agent_id.to_string(), child);
        }
        Ok(())
    }

    pub fn stop_agent(&mut self, agent_id: &str) {
        if let Some(mut child) = self.children.remove(agent_id) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn is_running(&mut self, agent_id: &str) -> bool {
        let Some(child) = self.children.get_mut(agent_id) else {
            return false;
        };
        matches!(child.try_wait(), Ok(None))
    }

    pub fn stop_all(&mut self) {
        let ids = self.children.keys().cloned().collect::<Vec<_>>();
        for agent_id in ids {
            self.stop_agent(&agent_id);
        }
    }
}

impl Drop for AgentSupervisor {
    fn drop(&mut self) {
        self.stop_all();
    }
}

fn spawn_resident_agent(
    repo_root: &PathBuf,
    config: &ResidentAgentConfig,
    max_parallel: usize,
) -> anyhow::Result<Child> {
    let exe = std::env::current_exe()?;
    Ok(Command::new(exe)
        .arg("resident-agent-run")
        .arg("--repo-root")
        .arg(repo_root)
        .arg("--agent-id")
        .arg(&config.agent_id)
        .arg("--role")
        .arg(&config.role)
        .arg("--max-parallel")
        .arg(
            config
                .max_parallel_override
                .unwrap_or(max_parallel.max(1))
                .to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

pub fn run_resident_agent(
    repo_root: PathBuf,
    agent_id: String,
    role: String,
    max_parallel: usize,
) -> anyhow::Result<()> {
    let project = ProjectContext::new(repo_root.clone())?;
    let config = configured_agent(&project, &agent_id, role)?;
    run_resident_agent_with_config(repo_root, config, max_parallel)
}

fn run_resident_agent_with_config(
    repo_root: PathBuf,
    config: ResidentAgentConfig,
    max_parallel: usize,
) -> anyhow::Result<()> {
    let handler = resident_handler(&config.runtime_mode).ok_or_else(|| {
        anyhow::anyhow!("unknown resident runtime mode '{}'", config.runtime_mode)
    })?;
    handler(repo_root, config, max_parallel)
}

type ResidentHandler = fn(PathBuf, ResidentAgentConfig, usize) -> anyhow::Result<()>;

fn resident_handler(runtime_mode: &str) -> Option<ResidentHandler> {
    match runtime_mode {
        "scheduler" => Some(run_scheduler_loop),
        "critic" => Some(run_critic_loop),
        "mailbox" => Some(run_mailbox_loop),
        _ => None,
    }
}

fn run_scheduler_loop(
    repo_root: PathBuf,
    config: ResidentAgentConfig,
    max_parallel: usize,
) -> anyhow::Result<()> {
    let agent_id = config.agent_id.clone();
    let mut runtime = TeamRuntime::start(repo_root.clone(), max_parallel.max(1));
    loop {
        let loop_started = Instant::now();
        let mut last_error = None::<String>;
        match (|| -> anyhow::Result<()> {
            let project = ProjectContext::new(repo_root.clone())?;
            ensure_resident_profile(&project, &config)?;
            let pending = project.tasks().pending_count()?;
            let note = if pending == 0 {
                "resident scheduler waiting for tasks"
            } else {
                "resident scheduler supervising task queue"
            };
            let _ = project.agents().set_state(
                &agent_id,
                "running",
                None,
                Some("resident"),
                Some("scheduler-loop"),
                Some(note),
            );
            let _ = project.resident_runtime().update_loop_status(
                &agent_id,
                0,
                None,
                loop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                None,
            );
            Ok(())
        })() {
            Ok(()) => {}
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }
        if let Some(error) = last_error.as_deref() {
            let project = ProjectContext::new(repo_root.clone())?;
            let _ = project.agents().set_state(
                &agent_id,
                "blocked",
                None,
                Some("resident"),
                Some("scheduler-loop"),
                Some(error),
            );
            let _ = project.resident_runtime().update_loop_status(
                &agent_id,
                0,
                None,
                loop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Some(error),
            );
        }
        thread::sleep(Duration::from_millis(config.loop_interval_ms.max(200)));
        if runtime.snapshot().max_parallel == 0 {
            runtime.stop();
            runtime = TeamRuntime::start(repo_root.clone(), max_parallel.max(1));
        }
    }
}

fn run_critic_loop(
    repo_root: PathBuf,
    config: ResidentAgentConfig,
    _max_parallel: usize,
) -> anyhow::Result<()> {
    let agent_id = config.agent_id.clone();
    loop {
        let loop_started = Instant::now();
        let mut last_error = None::<String>;
        match (|| -> anyhow::Result<()> {
            let project = ProjectContext::new(repo_root.clone())?;
            ensure_resident_profile(&project, &config)?;
            run_critic_pass(&project, &agent_id)?;
            let _ = project.resident_runtime().update_loop_status(
                &agent_id,
                0,
                None,
                loop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                None,
            );
            Ok(())
        })() {
            Ok(()) => {}
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }
        if let Some(error) = last_error.as_deref() {
            let project = ProjectContext::new(repo_root.clone())?;
            let _ = project.agents().set_state(
                &agent_id,
                "blocked",
                None,
                Some("resident"),
                Some("critic-loop"),
                Some(error),
            );
            let _ = project.resident_runtime().update_loop_status(
                &agent_id,
                0,
                None,
                loop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Some(error),
            );
        }
        thread::sleep(Duration::from_millis(config.loop_interval_ms.max(200)));
    }
}

fn run_mailbox_loop(
    repo_root: PathBuf,
    config: ResidentAgentConfig,
    _max_parallel: usize,
) -> anyhow::Result<()> {
    let agent_id = config.agent_id.clone();
    let _server_handle = maybe_start_resident_server(repo_root.clone(), &config)?;
    let initial_project = ProjectContext::new(repo_root.clone())?;
    let mut cursor = initial_project
        .resident_runtime()
        .mailbox_cursor(&agent_id)?;
    let mut last_periodic = Instant::now();
    loop {
        let loop_started = Instant::now();
        let mut last_processed_msg_id = None::<String>;
        let mut had_items = false;
        let mut last_error = None::<String>;
        match (|| -> anyhow::Result<()> {
            let project = ProjectContext::new(repo_root.clone())?;
            ensure_resident_profile(&project, &config)?;
            sync_ui_surface(&project, &config)?;
            let raw = project.mailbox().poll(&agent_id, cursor, 20)?;
            let poll: MailPoll = serde_json::from_str(&raw)?;
            had_items = !poll.items.is_empty();
            last_processed_msg_id = poll.items.last().map(|item| item.msg_id.clone());
            cursor = poll.next_cursor;
            for item in poll.items {
                if item.requires_ack {
                    let _ = project.mailbox().ack(
                        &agent_id,
                        &item.msg_id,
                        &format!("{} agent received", config.role),
                    );
                }
                let _ = project.decisions().append(
                    &agent_id,
                    &format!("{}.mail.received", config.role),
                    item.task_id,
                    None,
                    &format!("received {} from {}", item.msg_type, item.from),
                    &format!(
                        "resident {} agent polled mailbox using mode={}",
                        config.role, config.runtime_mode
                    ),
                );
                if let Err(err) = handle_mailbox_behavior(&project, &config, &item) {
                    last_error = Some(err.to_string());
                    let _ = project.decisions().append(
                        &agent_id,
                        "resident.mailbox.error",
                        item.task_id,
                        None,
                        &format!("mailbox behavior failed for {}", item.msg_type),
                        err.to_string().as_str(),
                    );
                }
            }
            if last_periodic.elapsed()
                >= Duration::from_millis(config.loop_interval_ms.max(200).saturating_mul(6))
            {
                if let Err(err) = handle_periodic_behavior(&project, &config) {
                    last_error = Some(err.to_string());
                    let _ = project.decisions().append(
                        &agent_id,
                        "resident.periodic.error",
                        None,
                        None,
                        "periodic resident behavior failed",
                        err.to_string().as_str(),
                    );
                }
                last_periodic = Instant::now();
            }
            let note = if let Some(error) = last_error.as_deref() {
                error.to_string()
            } else if had_items {
                format!("resident {} agent processed mailbox", config.role)
            } else {
                format!("resident {} agent waiting for mailbox work", config.role)
            };
            let _ = project.agents().set_state(
                &agent_id,
                if last_error.is_some() {
                    "blocked"
                } else if had_items {
                    "active"
                } else {
                    "idle"
                },
                None,
                Some("resident"),
                Some("mailbox-loop"),
                Some(&note),
            );
            let _ = project.resident_runtime().update_loop_status(
                &agent_id,
                cursor,
                last_processed_msg_id.as_deref(),
                loop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                last_error.as_deref(),
            );
            Ok(())
        })() {
            Ok(()) => {}
            Err(err) => {
                last_error = Some(err.to_string());
            }
        }
        if let Some(error) = last_error.as_deref() {
            let project = ProjectContext::new(repo_root.clone())?;
            let _ = project.agents().set_state(
                &agent_id,
                "blocked",
                None,
                Some("resident"),
                Some("mailbox-loop"),
                Some(error),
            );
            let _ = project.resident_runtime().update_loop_status(
                &agent_id,
                cursor,
                last_processed_msg_id.as_deref(),
                loop_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Some(error),
            );
        }
        thread::sleep(Duration::from_millis(config.loop_interval_ms.max(200)));
    }
}

fn handle_mailbox_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
    item: &MailItem,
) -> anyhow::Result<()> {
    let handler = mailbox_behavior_handler(&config.behavior_mode)
        .ok_or_else(|| anyhow::anyhow!("unknown mailbox behavior '{}'", config.behavior_mode))?;
    handler(project, config, item)
}

fn handle_periodic_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
) -> anyhow::Result<()> {
    let handler = periodic_behavior_handler(&config.behavior_mode)
        .ok_or_else(|| anyhow::anyhow!("unknown periodic behavior '{}'", config.behavior_mode))?;
    handler(project, config)
}

type MailboxBehaviorHandler =
    fn(&ProjectContext, &ResidentAgentConfig, &MailItem) -> anyhow::Result<()>;
type PeriodicBehaviorHandler = fn(&ProjectContext, &ResidentAgentConfig) -> anyhow::Result<()>;

fn mailbox_behavior_handler(behavior_mode: &str) -> Option<MailboxBehaviorHandler> {
    match behavior_mode {
        "passive" => Some(handle_passive_mailbox_behavior),
        "ui_request" => Some(handle_ui_request_behavior),
        "concierge_request" => Some(handle_concierge_request_behavior),
        "proposal_triage" => Some(handle_proposal_triage_behavior),
        "ui_surface_planning" => Some(handle_passive_mailbox_behavior),
        "scheduled_self_review" => Some(handle_passive_mailbox_behavior),
        _ => None,
    }
}

fn periodic_behavior_handler(behavior_mode: &str) -> Option<PeriodicBehaviorHandler> {
    match behavior_mode {
        "ui_surface_planning" => Some(handle_ui_surface_planning_behavior),
        "scheduled_self_review" => Some(handle_scheduled_self_review_behavior),
        _ => Some(handle_passive_periodic_behavior),
    }
}

fn handle_passive_mailbox_behavior(
    _project: &ProjectContext,
    _config: &ResidentAgentConfig,
    _item: &MailItem,
) -> anyhow::Result<()> {
    Ok(())
}

fn handle_passive_periodic_behavior(
    _project: &ProjectContext,
    _config: &ResidentAgentConfig,
) -> anyhow::Result<()> {
    Ok(())
}

fn handle_ui_request_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
    item: &MailItem,
) -> anyhow::Result<()> {
    if item.msg_type != "ui.request" && item.msg_type != "message" {
        return Ok(());
    }

    let priority = infer_priority_from_mail(&item.message);
    let description = format!(
        "[SOURCE=resident-ui][FROM={}][MSG_TYPE={}][PRIORITY={}]\nGoal:\n{}\n\nExecution notes:\n{}",
        item.from,
        item.msg_type,
        priority,
        item.message,
        priority_execution_notes(&priority)
    );
    let created = project.tasks().create_with_priority_and_role(
        &format!("ui request: {}", summarize_subject(&item.message)),
        &description,
        &priority,
        "ui",
    )?;
    let created_task = serde_json::from_str::<crate::project_tools::TaskRecord>(&created)?;

    let _ = project.decisions().append(
        &config.agent_id,
        "ui.request.task_created",
        Some(created_task.id),
        None,
        &format!("created ui task {} from mailbox request", created_task.id),
        &format!(
            "from={} type={} behavior={}",
            item.from, item.msg_type, config.behavior_mode
        ),
    );
    let _ = project.mailbox().send_typed(
        &config.agent_id,
        &item.from,
        "ui.request.accepted",
        &format!(
            "created ui task #{}: {}",
            created_task.id, created_task.subject
        ),
        Some(created_task.id),
        None,
        false,
        Some(&item.msg_id),
    );
    Ok(())
}

fn handle_concierge_request_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
    item: &MailItem,
) -> anyhow::Result<()> {
    if item.msg_type != "user.request" && item.msg_type != "message" {
        return Ok(());
    }

    let priority = infer_priority_from_mail(&item.message);
    let created = project.tasks().create_with_priority(
        &format!("request: {}", summarize_subject(&item.message)),
        &format!(
            "[SOURCE=resident-concierge][FROM={}][MSG_TYPE={}][PRIORITY={}]\nGoal:\n{}\n\nExecution notes:\n{}",
            item.from,
            item.msg_type,
            priority,
            item.message,
            priority_execution_notes(&priority)
        ),
        &priority,
    )?;
    let created_task = serde_json::from_str::<crate::project_tools::TaskRecord>(&created)?;
    let _ = project.decisions().append(
        &config.agent_id,
        "concierge.request.task_created",
        Some(created_task.id),
        None,
        &format!("created request task {}", created_task.id),
        &format!("from={} type={}", item.from, item.msg_type),
    );
    let _ = project.mailbox().send_typed(
        &config.agent_id,
        &item.from,
        "user.request.accepted",
        &format!(
            "created task #{}: {}",
            created_task.id, created_task.subject
        ),
        Some(created_task.id),
        None,
        false,
        Some(&item.msg_id),
    );
    Ok(())
}

fn handle_proposal_triage_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
    item: &MailItem,
) -> anyhow::Result<()> {
    if !matches!(
        item.msg_type.as_str(),
        "task.blocked" | "task.failed" | "proposal.request" | "message"
    ) {
        return Ok(());
    }

    let created = project.proposals().create(
        &config.agent_id,
        &item.msg_type,
        item.task_id,
        &format!("triage: {}", summarize_subject(&item.message)),
        &item.message,
        &["resident triage captured a new improvement candidate"],
        Some("review proposal and decide whether to convert it into a task"),
    )?;
    let proposal = serde_json::from_str::<serde_json::Value>(&created)?;
    let proposal_id = proposal
        .get("id")
        .and_then(|value| value.as_u64())
        .unwrap_or_default();
    let _ = project.decisions().append(
        &config.agent_id,
        "proposal.triage.created",
        item.task_id,
        Some(proposal_id),
        &format!("created proposal {}", proposal_id),
        &format!("from={} type={}", item.from, item.msg_type),
    );
    Ok(())
}

fn handle_scheduled_self_review_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
) -> anyhow::Result<()> {
    let pending = project.tasks().pending_count()?;
    let summary = format!(
        "resident {} self review: pending_tasks={} role={}",
        config.agent_id, pending, config.role
    );
    let _ = project.reflections().append(
        &config.agent_id,
        "resident.self_review",
        None,
        &summary,
        &["scheduled self review completed"],
        Some("check whether workload, budget, or routing policy should be adjusted"),
        false,
    );
    if pending > 3 {
        let _ = project.proposals().create(
            &config.agent_id,
            "resident.self_review",
            None,
            "consider reducing queue pressure",
            &summary,
            &["pending task queue is building up"],
            Some("review scheduling capacity or break large requests into smaller tasks"),
        );
    }
    let _ = project.decisions().append(
        &config.agent_id,
        "resident.self_review.completed",
        None,
        None,
        "completed scheduled self review",
        &format!("pending_tasks={}", pending),
    );
    Ok(())
}

fn handle_ui_surface_planning_behavior(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
) -> anyhow::Result<()> {
    let model = project.system_model().rebuild(project)?;
    let planner_prompt = project.ui_surface().planner_prompt_text()?;
    let fingerprint = project.ui_surface().collection_fingerprint(&model)?;
    if !project.ui_surface().needs_refresh(&fingerprint)? {
        return Ok(());
    }
    match project
        .ui_surface()
        .generate_with_collector(&model, &planner_prompt)
    {
        Ok(surface) => {
            let mut surface = surface;
            surface.source_fingerprint = fingerprint.clone();
            let _ = project.ui_surface().save(&surface);
            let _ = project.decisions().append(
                &config.agent_id,
                "ui.surface.generated",
                None,
                None,
                "surface collector generated a fresh ui surface spec",
                &format!(
                    "pages={} fingerprint={}",
                    surface.pages.len(),
                    surface.source_fingerprint
                ),
            );
            let _ = project.budgets().record_usage(&config.agent_id, 60);
        }
        Err(err) => {
            let error_text = err.to_string();
            let adaptation = project
                .ui_surface()
                .adapt_planner_prompt_for_error(&error_text)
                .ok();
            if let Some(adaptation) = adaptation.as_ref().filter(|item| item.changed) {
                if let Some(recovery) = adaptation.recovery.as_ref() {
                    let _ = project.prompt_history().append(
                        "ui-surface",
                        &config.agent_id,
                        &adaptation.file_path.display().to_string(),
                        &recovery.strategy,
                        &recovery.trigger,
                        &adaptation.before,
                        &adaptation.after,
                    );
                }
            }
            if adaptation.as_ref().is_some_and(|item| item.changed) {
                let retried_prompt = project.ui_surface().planner_prompt_text()?;
                match project
                    .ui_surface()
                    .generate_with_collector(&model, &retried_prompt)
                {
                    Ok(surface) => {
                        let mut surface = surface;
                        surface.source_fingerprint = fingerprint.clone();
                        let _ = project.ui_surface().save(&surface);
                        let _ = project.decisions().append(
                            &config.agent_id,
                            "ui.surface.recovered",
                            None,
                            None,
                            "surface collector recovered after auto-adjusting planner prompt",
                            &format!(
                                "pages={} fingerprint={} prior_error={}",
                                surface.pages.len(),
                                surface.source_fingerprint,
                                error_text
                            ),
                        );
                        return Ok(());
                    }
                    Err(retry_err) => {
                        let mut fallback = project.ui_surface().rebuild_from_model(&model)?;
                        fallback.source_fingerprint = fingerprint.clone();
                        let _ = project.ui_surface().save(&fallback);
                        let _ = project.decisions().append(
                            &config.agent_id,
                            "ui.surface.fallback",
                            None,
                            None,
                            "surface collector fell back after prompt auto-adjust and retry",
                            &format!(
                                "first_error={} retry_error={} pages={}",
                                error_text,
                                retry_err,
                                fallback.pages.len()
                            ),
                        );
                        return Ok(());
                    }
                }
            }

            let mut fallback = project.ui_surface().rebuild_from_model(&model)?;
            fallback.source_fingerprint = fingerprint.clone();
            let _ = project.ui_surface().save(&fallback);
            let _ = project.decisions().append(
                &config.agent_id,
                "ui.surface.fallback",
                None,
                None,
                "surface collector fell back to model-derived ui surface spec",
                &format!("error={} pages={}", error_text, fallback.pages.len()),
            );
        }
    }
    Ok(())
}

fn infer_priority_from_mail(message: &str) -> String {
    let text = message.to_lowercase();
    if text.contains("[critical]") || text.contains("critical") || text.contains("紧急") {
        "critical".to_string()
    } else if text.contains("[high]") || text.contains("high") || text.contains("高优") {
        "high".to_string()
    } else if text.contains("[low]") || text.contains("low") || text.contains("低优") {
        "low".to_string()
    } else {
        "medium".to_string()
    }
}

fn summarize_subject(message: &str) -> String {
    let single_line = message.lines().next().unwrap_or("").trim();
    let compact = if single_line.is_empty() {
        "untitled ui request"
    } else {
        single_line
    };
    compact.chars().take(60).collect()
}

fn configured_agent(
    project: &ProjectContext,
    agent_id: &str,
    fallback_role: String,
) -> anyhow::Result<ResidentAgentConfig> {
    if let Some(config) = project.residents().get(agent_id)? {
        return Ok(config);
    }
    let fallback_runtime_mode = match fallback_role.as_str() {
        "scheduler" => "scheduler",
        "critic" => "critic",
        _ => "mailbox",
    };
    Ok(ResidentAgentConfig {
        agent_id: agent_id.to_string(),
        role: fallback_role.clone(),
        runtime_mode: fallback_runtime_mode.to_string(),
        behavior_mode: "passive".to_string(),
        enabled: true,
        mission: format!("resident {}", fallback_role),
        scope: Vec::new(),
        forbidden: Vec::new(),
        daily_limit: 80_000,
        period_limit: 20_000,
        task_soft_limit: 8_000,
        loop_interval_ms: 2_500,
        max_parallel_override: None,
        listen_port: None,
    })
}

fn ensure_resident_profile(
    project: &ProjectContext,
    config: &ResidentAgentConfig,
) -> anyhow::Result<()> {
    let scope = config.scope.iter().map(String::as_str).collect::<Vec<_>>();
    let forbidden = config
        .forbidden
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    project.agents().ensure_profile(
        &config.agent_id,
        &config.role,
        &config.mission,
        &scope,
        &forbidden,
    )?;
    project.budgets().ensure_ledger(
        &config.agent_id,
        config.daily_limit,
        config.period_limit,
        config.task_soft_limit,
    )?;
    Ok(())
}

fn maybe_start_resident_server(
    repo_root: PathBuf,
    config: &ResidentAgentConfig,
) -> anyhow::Result<Option<thread::JoinHandle<()>>> {
    if config.role != "ui" && config.behavior_mode != "ui_request" {
        return Ok(None);
    }
    let port = resident_listen_port(config);
    let handle = spawn_ui_server(repo_root, config.agent_id.clone(), port)?;
    Ok(Some(handle))
}

pub fn resident_listen_port(config: &ResidentAgentConfig) -> u16 {
    config
        .listen_port
        .unwrap_or_else(|| if config.role == "ui" { 3847 } else { 0 })
}

fn sync_ui_surface(project: &ProjectContext, config: &ResidentAgentConfig) -> anyhow::Result<()> {
    if config.role != "ui" && config.behavior_mode != "ui_request" {
        return Ok(());
    }
    let model = project.system_model().rebuild(project)?;
    let surface = match project.ui_surface().snapshot()? {
        Some(surface) => surface,
        None => project.ui_surface().rebuild_from_model(&model)?,
    };
    let prompt_text = project.ui_surface().prompt_text()?;
    let fingerprint = format!(
        "{}:{}",
        project.ui_surface().prompt_fingerprint()?,
        surface.source_fingerprint,
    );
    if !project.ui_schema().needs_refresh(&fingerprint)? {
        return Ok(());
    }
    match project
        .ui_schema()
        .generate_with_ui_agent(&model, &surface, &prompt_text, &fingerprint)
    {
        Ok(_) => {
            let _ = project.decisions().append(
                &config.agent_id,
                "ui.schema.generated",
                None,
                None,
                "ui agent generated a fresh ui schema from surface spec and prompt",
                "system model, ui surface or prompt fingerprint changed and llm generation succeeded",
            );
            let _ = project.budgets().record_usage(&config.agent_id, 80);
        }
        Err(err) => {
            let error_text = err.to_string();
            let adaptation = project
                .ui_surface()
                .adapt_ui_prompt_for_error(&error_text)
                .ok();
            if let Some(adaptation) = adaptation.as_ref().filter(|item| item.changed) {
                if let Some(recovery) = adaptation.recovery.as_ref() {
                    let _ = project.prompt_history().append(
                        "ui-schema",
                        &config.agent_id,
                        &adaptation.file_path.display().to_string(),
                        &recovery.strategy,
                        &recovery.trigger,
                        &adaptation.before,
                        &adaptation.after,
                    );
                }
            }
            if adaptation.as_ref().is_some_and(|item| item.changed) {
                let retried_prompt = project.ui_surface().prompt_text()?;
                let retry_fingerprint = format!(
                    "{}:{}",
                    project.ui_surface().prompt_fingerprint()?,
                    surface.source_fingerprint,
                );
                match project.ui_schema().generate_with_ui_agent(
                    &model,
                    &surface,
                    &retried_prompt,
                    &retry_fingerprint,
                ) {
                    Ok(_) => {
                        let _ = project.decisions().append(
                            &config.agent_id,
                            "ui.schema.recovered",
                            None,
                            None,
                            "ui schema generation recovered after auto-adjusting prompt",
                            &format!("prior_error={}", error_text),
                        );
                        return Ok(());
                    }
                    Err(retry_err) => {
                        let _ = project.ui_schema().generate_from_surface(
                            &model,
                            &surface,
                            &retry_fingerprint,
                        )?;
                        let _ = project.decisions().append(
                            &config.agent_id,
                            "ui.schema.fallback",
                            None,
                            None,
                            "ui schema generation fell back after prompt auto-adjust and retry",
                            &format!("first_error={} retry_error={}", error_text, retry_err),
                        );
                        return Ok(());
                    }
                }
            }

            let _ = project
                .ui_schema()
                .generate_from_surface(&model, &surface, &fingerprint)?;
            let _ = project.decisions().append(
                &config.agent_id,
                "ui.schema.fallback",
                None,
                None,
                "ui agent schema generation fell back to surface-based schema",
                &error_text,
            );
        }
    }
    Ok(())
}

fn run_critic_pass(project: &ProjectContext, agent_id: &str) -> anyhow::Result<()> {
    let mode = project.budgets().energy_mode(agent_id)?;
    match mode {
        Some(crate::project_tools::EnergyMode::Exhausted) => {
            let _ = project.agents().set_state(
                agent_id,
                "cooldown",
                None,
                Some("resident"),
                Some("critic-loop"),
                Some("budget exhausted"),
            );
            return Ok(());
        }
        Some(crate::project_tools::EnergyMode::Low) => {
            let _ = project.agents().set_state(
                agent_id,
                "running",
                None,
                Some("resident"),
                Some("critic-loop"),
                Some("low energy, review 1 proposal"),
            );
        }
        _ => {
            let _ = project.agents().set_state(
                agent_id,
                "running",
                None,
                Some("resident"),
                Some("critic-loop"),
                Some("reviewing open proposals"),
            );
        }
    }

    let limit = match mode {
        Some(crate::project_tools::EnergyMode::Low) => 1,
        Some(crate::project_tools::EnergyMode::Constrained) => 2,
        Some(crate::project_tools::EnergyMode::Healthy) | None => 4,
        _ => 2,
    };
    let proposals = project.proposals().list_open(limit)?;
    if proposals.is_empty() {
        let _ = project.agents().set_state(
            agent_id,
            "idle",
            None,
            Some("resident"),
            Some("critic-loop"),
            Some("no open proposals"),
        );
        return Ok(());
    }

    let convert_cap = match mode {
        Some(crate::project_tools::EnergyMode::Low) => 1,
        Some(crate::project_tools::EnergyMode::Constrained) => 1,
        Some(crate::project_tools::EnergyMode::Healthy) | None => 2,
        _ => 1,
    };

    for proposal in proposals.into_iter().take(convert_cap) {
        let subject = format!("proposal: {}", proposal.title);
        if project.tasks().has_active_subject(&subject)? {
            let _ = project.proposals().update_status(proposal.id, "rejected");
            let _ = project.decisions().append(
                agent_id,
                "proposal.rejected_duplicate",
                proposal.task_id,
                Some(proposal.id),
                &format!("rejected duplicate proposal {}", proposal.id),
                &format!("active task with subject '{}' already exists", subject),
            );
            continue;
        }

        let description = format!(
            "[PROPOSAL #{}][from={}][trigger={}][priority={}][score={}]\n{}\n\nIssues:\n- {}\n\nSuggested action:\n{}\n\nExecution notes:\n{}",
            proposal.id,
            proposal.source_agent,
            proposal.trigger,
            proposal.priority,
            proposal.score,
            proposal.summary,
            if proposal.issues.is_empty() {
                String::from("none")
            } else {
                proposal.issues.join("\n- ")
            },
            proposal
                .suggested_action
                .clone()
                .unwrap_or_else(|| "review and define implementation steps".to_string()),
            priority_execution_notes(&proposal.priority)
        );
        let role_hint = infer_execution_role_for_proposal(&proposal.title, &proposal.summary);
        let _ = project.tasks().create_with_priority_and_role(
            &subject,
            &description,
            &proposal.priority,
            role_hint,
        )?;
        let _ = project.proposals().update_status(proposal.id, "converted");
        let _ = project.decisions().append(
            agent_id,
            "proposal.converted",
            proposal.task_id,
            Some(proposal.id),
            &format!("converted proposal {} into task '{}'", proposal.id, subject),
            &format!(
                "priority={} score={} source={} trigger={} role={}",
                proposal.priority,
                proposal.score,
                proposal.source_agent,
                proposal.trigger,
                role_hint
            ),
        );
        let _ = project.budgets().record_usage(agent_id, 30);
    }
    Ok(())
}

fn infer_execution_role_for_proposal(title: &str, summary: &str) -> &'static str {
    let text = format!("{} {}", title, summary).to_lowercase();
    if text.contains("ui")
        || text.contains("interface")
        || text.contains("frontend")
        || text.contains("component")
        || text.contains("page")
        || text.contains("layout")
        || text.contains("screen")
    {
        "ui"
    } else if text.contains("design") || text.contains("ux") || text.contains("style") {
        "design"
    } else {
        "developer"
    }
}

fn priority_execution_notes(priority: &str) -> &'static str {
    match priority {
        "critical" => {
            "- Confirm scope and impact first\n- Prefer minimal-risk changes\n- Verify behavior explicitly before marking complete\n- Escalate blockers immediately"
        }
        "high" => {
            "- Keep implementation focused\n- Call out assumptions clearly\n- Run at least one concrete verification step\n- Avoid unrelated refactors"
        }
        "medium" => "- Complete the requested work directly\n- Keep changes scoped to the task",
        "low" => "- Favor low-cost, low-risk cleanup\n- Do not expand scope unless necessary",
        _ => "- Complete the requested work directly\n- Keep changes scoped to the task",
    }
}

#[derive(Debug, Deserialize)]
struct MailPoll {
    next_cursor: usize,
    items: Vec<MailItem>,
}

#[derive(Debug, Deserialize)]
struct MailItem {
    msg_id: String,
    msg_type: String,
    from: String,
    message: String,
    task_id: Option<u64>,
    #[serde(default)]
    requires_ack: bool,
}
