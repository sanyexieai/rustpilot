use crate::project_tools::{ProjectContext, classify_energy};

pub(crate) fn render_usage_text(project: &ProjectContext) -> anyhow::Result<String> {
    let mut ledgers = project.budgets().list_all()?;
    if ledgers.is_empty() {
        return Ok("usage: no budget ledgers".to_string());
    }
    ledgers.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
    Ok(ledgers
        .into_iter()
        .map(|item| {
            format!(
                "- {} energy={:?} used_today={}/{} used_in_period={} reserved={} task_soft_limit={}",
                item.agent_id,
                classify_energy(&item),
                item.used_today,
                item.daily_limit,
                item.used_in_period,
                item.reserved,
                item.task_soft_limit
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}
