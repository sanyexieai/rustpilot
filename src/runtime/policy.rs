use crate::project_tools::ProjectContext;
use crate::team::{render_agent_policy, render_policy_overview, render_task_policy};

pub(crate) fn render_policy_overview_text(
    project: &ProjectContext,
    auto_team_max_parallel: usize,
) -> String {
    render_policy_overview(project, auto_team_max_parallel)
}

pub(crate) fn render_policy_task_text(
    project: &ProjectContext,
    task_id: u64,
    auto_team_max_parallel: usize,
) -> anyhow::Result<String> {
    render_task_policy(project, task_id, auto_team_max_parallel)
}

pub(crate) fn render_policy_agent_text(
    project: &ProjectContext,
    agent_id: &str,
) -> anyhow::Result<String> {
    render_agent_policy(project, agent_id)
}
