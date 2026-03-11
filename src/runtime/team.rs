use crate::app_support::{InteractionMode, build_priority_task_description};
use crate::project_tools::ProjectContext;
use crate::resident_agents::AgentSupervisor;
use crate::runtime::lead::maybe_reflect_energy;
use crate::team::get_worker_endpoint;
use std::path::Path;

pub(crate) fn focus_lead(project: &ProjectContext, interaction_mode: &mut InteractionMode) {
    *interaction_mode = InteractionMode::Lead;
    let _ = project.budgets().record_usage("lead", 10);
    maybe_reflect_energy(
        project,
        "lead",
        "focus.lead",
        None,
        "switched focus to lead",
    );
    let _ = project.agents().set_state(
        "lead",
        "active",
        None,
        Some("cli"),
        Some("main"),
        Some("lead focus"),
    );
}

pub(crate) fn focus_shell(project: &ProjectContext, interaction_mode: &mut InteractionMode) {
    *interaction_mode = InteractionMode::Shell;
    let _ = project.budgets().record_usage("lead", 5);
    maybe_reflect_energy(
        project,
        "lead",
        "focus.shell",
        None,
        "switched focus to shell",
    );
    let _ = project.agents().set_state(
        "lead",
        "active",
        None,
        Some("cli"),
        Some("main"),
        Some("shell focus"),
    );
}

pub(crate) fn focus_team(project: &ProjectContext, interaction_mode: &mut InteractionMode) {
    *interaction_mode = InteractionMode::TeamQueue;
    let _ = project.budgets().record_usage("lead", 5);
    maybe_reflect_energy(
        project,
        "lead",
        "focus.team",
        None,
        "switched focus to team",
    );
    let _ = project.agents().set_state(
        "lead",
        "idle",
        None,
        Some("cli"),
        Some("main"),
        Some("team queue focus"),
    );
}

pub(crate) fn focus_worker(
    repo_root: &Path,
    project: &ProjectContext,
    interaction_mode: &mut InteractionMode,
    task_id: u64,
) -> anyhow::Result<String> {
    Ok(match get_worker_endpoint(repo_root, task_id)? {
        Some(endpoint) if endpoint.status == "running" => {
            *interaction_mode = InteractionMode::Worker { task_id };
            let _ = project.budgets().record_usage("lead", 5);
            maybe_reflect_energy(
                project,
                "lead",
                "focus.worker",
                Some(task_id),
                "switched focus to worker",
            );
            format!(
                "focus: worker task={} channel={} target={}",
                task_id, endpoint.channel, endpoint.target
            )
        }
        Some(endpoint) => format!("worker for task {} is {}", task_id, endpoint.status),
        None => format!("worker for task {} not found", task_id),
    })
}

pub(crate) fn reply_task(
    repo_root: &Path,
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
    task_id: u64,
    content: &str,
) -> anyhow::Result<String> {
    let worker_running = matches!(
        get_worker_endpoint(repo_root, task_id)?,
        Some(endpoint) if endpoint.status == "running"
    );
    let updated = project.tasks().append_user_reply(
        task_id,
        content,
        if worker_running {
            "in_progress"
        } else {
            "pending"
        },
    )?;
    let trace_id = format!("task-{}", task_id);
    let target = format!("teammate-{}", task_id);
    let message = format!("user clarification: {}", content);
    let _ = project.mailbox().send_typed(
        "lead",
        &target,
        "task.clarification",
        &message,
        Some(task_id),
        Some(&trace_id),
        false,
        None,
    );
    if worker_running {
        Ok(format!(
            "clarification sent to running worker:\n{}",
            updated
        ))
    } else {
        let _ = supervisor.ensure_running("scheduler");
        Ok(format!(
            "clarification appended and task re-queued:\n{}",
            updated
        ))
    }
}

pub(crate) fn team_run(
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
    goal: &str,
    priority: &str,
) -> anyhow::Result<String> {
    let task = project.tasks().create_with_priority(
        goal,
        &build_priority_task_description("/team run", priority, goal),
        priority,
    )?;
    let _ = supervisor.ensure_running("scheduler");
    Ok(format!("task created:\n{}", task))
}

pub(crate) fn team_start(supervisor: &mut AgentSupervisor) -> anyhow::Result<String> {
    supervisor.ensure_running("scheduler")?;
    Ok("resident scheduler ensured".to_string())
}

pub(crate) fn team_stop(supervisor: &mut AgentSupervisor) -> String {
    supervisor.stop_agent("scheduler");
    "resident scheduler stopped".to_string()
}

pub(crate) fn resident_send(
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
    agent_id: &str,
    msg_type: &str,
    content: &str,
) -> anyhow::Result<String> {
    let _ = project
        .mailbox()
        .send_typed("lead", agent_id, msg_type, content, None, None, false, None)?;
    let _ = project.decisions().append(
        "lead",
        "resident.message.sent",
        None,
        None,
        &format!("sent {} to {}", msg_type, agent_id),
        "manual resident dispatch from cli",
    );
    let _ = supervisor.ensure_running(agent_id);
    Ok(format!(
        "resident message sent: {} -> {}",
        msg_type, agent_id
    ))
}
