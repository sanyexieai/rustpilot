use crate::prompt_manager::lead_prompt_recovery;
use crate::project_tools::ProjectContext;
use crate::resident_agents::{AgentSupervisor, resident_listen_port};
use crate::runtime::lead::truncate_text;

pub(crate) fn render_team_status(
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
) -> anyhow::Result<String> {
    let latest_prompt_change = project
        .prompt_history()
        .list_recent(1)
        .ok()
        .and_then(|mut items| items.pop())
        .map(|item| {
            format!(
                " latest_prompt={}({}:{}; {})",
                item.agent_id,
                item.strategy,
                truncate_text(&item.trigger, 36),
                truncate_text(&item.diff_summary, 36)
            )
        })
        .unwrap_or_default();
    let configured = project.residents().enabled_agents()?;
    if configured.is_empty() {
        return Ok(format!(
            "team: no enabled resident agents pending={}{}",
            project.tasks().pending_count()?,
            latest_prompt_change
        ));
    }

    let lead_recovery = lead_prompt_recovery(project.repo_root())
        .ok()
        .flatten()
        .map(|info| {
            format!(
                " strategy={} trigger={}",
                info.strategy,
                truncate_text(&info.trigger, 60)
            )
        })
        .unwrap_or_default();
    let mut alerts = Vec::new();
    let states = configured
        .into_iter()
        .map(|item| {
            let agent_state = project.agents().state(&item.agent_id).ok().flatten();
            let runtime = project
                .resident_runtime()
                .snapshot(&item.agent_id)
                .ok()
                .flatten();
            let cursor = project
                .resident_runtime()
                .mailbox_cursor(&item.agent_id)
                .unwrap_or(0);
            let backlog = project
                .mailbox()
                .backlog_count(&item.agent_id, cursor)
                .unwrap_or(0);
            let last_action = project
                .decisions()
                .latest_for_agent(&item.agent_id)
                .ok()
                .flatten()
                .map(|decision| format!("{}:{}", decision.action, decision.summary))
                .unwrap_or_else(|| "none".to_string());
            let loop_ms = runtime
                .as_ref()
                .map(|state| state.last_loop_duration_ms.to_string())
                .unwrap_or_else(|| "-".to_string());
            let running = supervisor.is_running(&item.agent_id);
            let status = agent_state
                .as_ref()
                .map(|state| state.status.as_str())
                .unwrap_or("unknown");
            let note = agent_state
                .as_ref()
                .and_then(|state| state.note.as_deref())
                .unwrap_or("-");
            let last_error = runtime
                .as_ref()
                .and_then(|state| state.last_error.as_deref())
                .unwrap_or("-");
            if !running {
                alerts.push(format!("{} stopped", item.agent_id));
            }
            if status == "blocked" {
                alerts.push(format!("{} blocked", item.agent_id));
            }
            if last_error != "-" {
                alerts.push(format!("{} error={}", item.agent_id, last_error));
            }
            if backlog > 10 {
                alerts.push(format!("{} backlog={}", item.agent_id, backlog));
            }
            let port = resident_listen_port(&item);
            let endpoint = if port > 0 {
                format!(" url=http://127.0.0.1:{}", port)
            } else {
                String::new()
            };
            let prompt_recovery = if item.role == "ui" {
                project.ui_surface().ui_prompt_recovery().ok().flatten()
            } else if item.behavior_mode == "ui_surface_planning" {
                project.ui_surface().planner_prompt_recovery().ok().flatten()
            } else {
                None
            };
            let prompt_suffix = prompt_recovery
                .map(|info| {
                    format!(
                        " prompt={}({})",
                        info.strategy,
                        truncate_text(&info.trigger, 40)
                    )
                })
                .unwrap_or_default();
            format!(
                "{}={} status={} backlog={} loop_ms={} note={}{}{} last={}",
                item.agent_id, running, status, backlog, loop_ms, note, endpoint, prompt_suffix, last_action
            )
        })
        .collect::<Vec<_>>()
        .join(" ");

    Ok(if alerts.is_empty() {
        format!(
            "team: lead{} {} pending={} alerts=none{}",
            lead_recovery,
            states,
            project.tasks().pending_count()?,
            latest_prompt_change
        )
    } else {
        format!(
            "team: lead{} {} pending={} alerts={}{}",
            lead_recovery,
            states,
            project.tasks().pending_count()?,
            alerts.join(" | "),
            latest_prompt_change
        )
    })
}

pub(crate) fn render_residents_status(
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
) -> anyhow::Result<String> {
    let configured = project.residents().list_all()?;
    if configured.is_empty() {
        return Ok("no resident agents configured".to_string());
    }

    Ok(configured
        .into_iter()
        .map(|item| {
            let agent_state = project.agents().state(&item.agent_id).ok().flatten();
            let runtime = project
                .resident_runtime()
                .snapshot(&item.agent_id)
                .ok()
                .flatten();
            let cursor = project
                .resident_runtime()
                .mailbox_cursor(&item.agent_id)
                .unwrap_or(0);
            let backlog = project
                .mailbox()
                .backlog_count(&item.agent_id, cursor)
                .unwrap_or(0);
            let last_action = project
                .decisions()
                .latest_for_agent(&item.agent_id)
                .ok()
                .flatten()
                .map(|decision| format!("{} ({})", decision.action, decision.reason))
                .unwrap_or_else(|| "none".to_string());
            let last_msg = runtime
                .as_ref()
                .and_then(|state| state.last_processed_msg_id.clone())
                .unwrap_or_else(|| "none".to_string());
            let loop_ms = runtime
                .as_ref()
                .map(|state| state.last_loop_duration_ms.to_string())
                .unwrap_or_else(|| "-".to_string());
            let last_error = runtime
                .as_ref()
                .and_then(|state| state.last_error.clone())
                .unwrap_or_else(|| "none".to_string());
            let status = agent_state
                .as_ref()
                .map(|state| state.status.as_str())
                .unwrap_or("unknown");
            let note = agent_state
                .as_ref()
                .and_then(|state| state.note.as_deref())
                .unwrap_or("none");
            let port = resident_listen_port(&item);
            let prompt_recovery = if item.role == "ui" {
                project.ui_surface().ui_prompt_recovery().ok().flatten()
            } else if item.behavior_mode == "ui_surface_planning" {
                project.ui_surface().planner_prompt_recovery().ok().flatten()
            } else {
                None
            };
            let prompt_strategy = prompt_recovery
                .as_ref()
                .map(|info| info.strategy.as_str())
                .unwrap_or("-");
            let prompt_trigger = prompt_recovery
                .as_ref()
                .map(|info| truncate_text(&info.trigger, 80))
                .unwrap_or_else(|| "-".to_string());
            format!(
                "- {} role={} mode={} behavior={} enabled={} running={} status={} backlog={} loop_ms={} port={} prompt={} trigger={} last_msg={} last_error={} note={} last={}",
                item.agent_id,
                item.role,
                item.runtime_mode,
                item.behavior_mode,
                item.enabled,
                supervisor.is_running(&item.agent_id),
                status,
                backlog,
                loop_ms,
                if port > 0 { port.to_string() } else { "-".to_string() },
                prompt_strategy,
                prompt_trigger,
                last_msg,
                last_error,
                note,
                last_action
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
