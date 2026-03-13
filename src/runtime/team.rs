use crate::app_support::{InteractionMode, build_priority_task_description, current_agent_id};
use crate::project_tools::{ProjectContext, TaskRecord};
use crate::resident_agents::AgentSupervisor;
use crate::runtime::lead::maybe_reflect_energy;
use crate::team::get_worker_endpoint;
use std::collections::HashMap;
use std::path::Path;

const MAX_DIRECT_CHILDREN: usize = 10;
const MAX_TASK_DEPTH: u32 = 10;

pub(crate) fn focus_lead(project: &ProjectContext, interaction_mode: &mut InteractionMode) {
    let actor_id = current_agent_id();
    *interaction_mode = InteractionMode::Lead;
    let _ = project.budgets().record_usage(&actor_id, 10);
    maybe_reflect_energy(
        project,
        &actor_id,
        "focus.lead",
        None,
        "switched focus to lead",
    );
    let _ = project.agents().set_state(
        &actor_id,
        "active",
        None,
        Some("cli"),
        Some("main"),
        Some("lead focus"),
    );
}

pub(crate) fn focus_shell(project: &ProjectContext, interaction_mode: &mut InteractionMode) {
    let actor_id = current_agent_id();
    *interaction_mode = InteractionMode::Shell;
    let _ = project.budgets().record_usage(&actor_id, 5);
    maybe_reflect_energy(
        project,
        &actor_id,
        "focus.shell",
        None,
        "switched focus to shell",
    );
    let _ = project.agents().set_state(
        &actor_id,
        "active",
        None,
        Some("cli"),
        Some("main"),
        Some("shell focus"),
    );
}

pub(crate) fn focus_team(project: &ProjectContext, interaction_mode: &mut InteractionMode) {
    let actor_id = current_agent_id();
    *interaction_mode = InteractionMode::TeamQueue;
    let _ = project.budgets().record_usage(&actor_id, 5);
    maybe_reflect_energy(
        project,
        &actor_id,
        "focus.team",
        None,
        "switched focus to team",
    );
    let _ = project.agents().set_state(
        &actor_id,
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
            let actor_id = current_agent_id();
            *interaction_mode = InteractionMode::Worker { task_id };
            let _ = project.budgets().record_usage(&actor_id, 5);
            maybe_reflect_energy(
                project,
                &actor_id,
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
    let actor_id = current_agent_id();
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
        &actor_id,
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

pub(crate) fn control_task(
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
    task_id: u64,
    action: &str,
    priority: Option<&str>,
) -> anyhow::Result<String> {
    let actor_id = current_agent_id();
    let before = project.tasks().get_record(task_id)?;
    let (next_status, next_priority, summary) = match action {
        "pause" => (
            Some("paused"),
            None,
            format!("paused task {}", task_id),
        ),
        "resume" => (
            Some("pending"),
            None,
            format!("resumed task {}", task_id),
        ),
        "cancel" => (
            Some("cancelled"),
            None,
            format!("cancelled task {}", task_id),
        ),
        "priority" => (
            None,
            priority,
            format!(
                "raised task {} priority to {}",
                task_id,
                priority.unwrap_or("medium")
            ),
        ),
        _ => anyhow::bail!("unsupported task action: {}", action),
    };

    let updated = project
        .tasks()
        .update(task_id, next_status, None, next_priority)?;
    let reason = match action {
        "pause" => "parent agent paused delegated work for replanning",
        "resume" => "parent agent resumed delegated work",
        "cancel" => "parent agent cancelled delegated work",
        "priority" => "parent agent reprioritized delegated work",
        _ => "task control action",
    };
    let _ = project
        .decisions()
        .append(&actor_id, "task.control", Some(task_id), None, &summary, reason);

    if action == "resume" {
        let _ = supervisor.ensure_running("scheduler");
    }

    let parent_hint = before
        .parent_task_id
        .map(|id| format!(" parent={id}"))
        .unwrap_or_default();
    Ok(format!("{}\n{}{}", summary, updated, parent_hint))
}

pub(crate) fn render_task_tree(project: &ProjectContext) -> anyhow::Result<String> {
    let tasks = project.tasks().list_records()?;
    if tasks.is_empty() {
        return Ok("no tasks".to_string());
    }

    let mut by_parent: HashMap<Option<u64>, Vec<TaskRecord>> = HashMap::new();
    let mut child_counts: HashMap<u64, usize> = HashMap::new();
    for task in tasks.iter().cloned() {
        if let Some(parent) = task.parent_task_id {
            *child_counts.entry(parent).or_insert(0) += 1;
        }
        by_parent.entry(task.parent_task_id).or_default().push(task);
    }
    for children in by_parent.values_mut() {
        children.sort_by(|a, b| a.id.cmp(&b.id));
    }

    let mut lines = Vec::new();
    let mut roots = by_parent.remove(&None).unwrap_or_default();
    roots.sort_by(|a, b| a.id.cmp(&b.id));
    for task in &roots {
        append_task_tree_lines(task, 0, &by_parent, &child_counts, &mut lines);
    }

    let mut warnings = Vec::new();
    for task in tasks {
        let direct_children = child_counts.get(&task.id).copied().unwrap_or(0);
        if direct_children > MAX_DIRECT_CHILDREN {
            warnings.push(format!(
                "- task #{} has {} direct children, exceeds limit {}",
                task.id, direct_children, MAX_DIRECT_CHILDREN
            ));
        }
        if task.depth > MAX_TASK_DEPTH {
            warnings.push(format!(
                "- task #{} depth={} exceeds limit {}",
                task.id, task.depth, MAX_TASK_DEPTH
            ));
        }
    }

    if warnings.is_empty() {
        Ok(format!(
            "task tree:\n{}\n\nthresholds: direct_children<={} depth<={}",
            lines.join("\n"),
            MAX_DIRECT_CHILDREN,
            MAX_TASK_DEPTH
        ))
    } else {
        Ok(format!(
            "task tree:\n{}\n\nthresholds: direct_children<={} depth<={}\nalerts:\n{}",
            lines.join("\n"),
            MAX_DIRECT_CHILDREN,
            MAX_TASK_DEPTH,
            warnings.join("\n")
        ))
    }
}

pub(crate) fn resident_send(
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
    agent_id: &str,
    msg_type: &str,
    content: &str,
) -> anyhow::Result<String> {
    let actor_id = current_agent_id();
    let _ = project
        .mailbox()
        .send_typed(&actor_id, agent_id, msg_type, content, None, None, false, None)?;
    let _ = project.decisions().append(
        &actor_id,
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

fn append_task_tree_lines(
    task: &TaskRecord,
    indent: usize,
    by_parent: &HashMap<Option<u64>, Vec<TaskRecord>>,
    child_counts: &HashMap<u64, usize>,
    lines: &mut Vec<String>,
) {
    let prefix = "  ".repeat(indent);
    let child_count = child_counts.get(&task.id).copied().unwrap_or(0);
    lines.push(format!(
        "{}- #{} [{}] priority={} role={} children={} {}",
        prefix,
        task.id,
        task.status,
        task.priority,
        task.role_hint,
        child_count,
        task.subject
    ));
    if let Some(children) = by_parent.get(&Some(task.id)) {
        for child in children {
            append_task_tree_lines(child, indent + 1, by_parent, child_counts, lines);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{control_task, reply_task};
    use crate::project_tools::{ProjectContext, TaskRecord};
    use crate::resident_agents::AgentSupervisor;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "{}_{}_{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("time")
                    .as_nanos()
            );
            let path = std::env::temp_dir().join("rustpilot-tests").join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn global_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_global() -> MutexGuard<'static, ()> {
        global_lock().lock().unwrap_or_else(|err| err.into_inner())
    }

    fn run(repo: &Path, program: &str, args: &[&str]) {
        let output = Command::new(program)
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run command");
        assert!(
            output.status.success(),
            "{} {:?} failed: {}{}",
            program,
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo(path: &Path) {
        run(path, "git", &["init"]);
        run(path, "git", &["config", "user.name", "Codex"]);
        run(path, "git", &["config", "user.email", "codex@example.com"]);
        fs::write(path.join("README.md"), "hello\n").expect("write readme");
        run(path, "git", &["add", "."]);
        run(path, "git", &["commit", "-m", "init"]);
    }

    fn project_context(repo_root: &Path) -> ProjectContext {
        ProjectContext::new(repo_root.to_path_buf()).expect("project context")
    }

    #[test]
    fn task_control_records_current_actor() {
        let _guard = lock_global();
        let temp = TestDir::new("runtime_team_task_control_actor");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        let created = project
            .tasks()
            .create("parent task", "pause task")
            .expect("create task");
        let task: TaskRecord = serde_json::from_str(&created).expect("parse task");
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        project
            .budgets()
            .ensure_ledger("teammate-parent", 120_000, 30_000, 12_000)
            .expect("budget");

        unsafe {
            std::env::set_var("RUSTPILOT_AGENT_ID", "teammate-parent");
        }

        let message = control_task(&project, &mut supervisor, task.id, "pause", None)
            .expect("pause task");
        assert!(message.contains("paused task"));

        let decision = project
            .decisions()
            .latest_for_agent("teammate-parent")
            .expect("latest decision")
            .expect("decision record");
        assert_eq!(decision.action, "task.control");
        assert_eq!(decision.task_id, Some(task.id));

        unsafe {
            std::env::remove_var("RUSTPILOT_AGENT_ID");
        }
    }

    #[test]
    fn reply_task_uses_current_actor_as_sender() {
        let _guard = lock_global();
        let temp = TestDir::new("runtime_team_reply_task_actor");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        let created = project
            .tasks()
            .create("child task", "needs clarification")
            .expect("create task");
        let task: TaskRecord = serde_json::from_str(&created).expect("parse task");
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        project
            .budgets()
            .ensure_ledger("teammate-parent", 120_000, 30_000, 12_000)
            .expect("budget");

        unsafe {
            std::env::set_var("RUSTPILOT_AGENT_ID", "teammate-parent");
        }

        let response = reply_task(
            temp.path(),
            &project,
            &mut supervisor,
            task.id,
            "please use vite dev server",
        )
        .expect("reply task");
        assert!(response.contains("clarification"));

        let inbox = project
            .mailbox()
            .poll(&format!("teammate-{}", task.id), 0, 10)
            .expect("poll mailbox");
        let payload: serde_json::Value = serde_json::from_str(&inbox).expect("parse mailbox");
        let item = payload["items"]
            .as_array()
            .and_then(|items| items.first())
            .expect("mail item");
        assert_eq!(item["from"].as_str(), Some("teammate-parent"));

        unsafe {
            std::env::remove_var("RUSTPILOT_AGENT_ID");
        }
    }
}
