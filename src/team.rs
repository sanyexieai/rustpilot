use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::agent::{AgentProgressReport, run_agent_loop, tool_definitions};
use crate::config::LlmConfig;
use crate::config::default_llm_user_agent;
use crate::openai_compat::Message;
use crate::openai_compat::Tool;
use crate::project_tools::{
    EnergyMode, ProjectContext, TaskRecord, classify_energy, task_priority_rank,
};
use crate::prompt_manager::{adapt_worker_prompt_detailed, render_worker_system_prompt};
use crate::runtime_env::llm_timeout_secs_for_provider;
use crate::terminal_session::{SessionState, TerminalCreateRequest, TerminalManager};
use serde::{Deserialize, Serialize};

pub struct TeamRuntime {
    max_parallel: usize,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
    launched: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

enum SpawnMode {
    Inherit,
    Tmux,
    Terminal,
}

enum WorkerHandle {
    Child { task_id: u64, child: Child },
    TmuxWindow { task_id: u64, window_name: String },
    TerminalSession { task_id: u64, session_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerEndpoint {
    pub task_id: u64,
    pub owner: String,
    pub channel: String,
    pub target: String,
    pub status: String,
}

pub struct TeamSnapshot {
    pub max_parallel: usize,
    pub running: usize,
    pub launched: usize,
    pub completed: usize,
    pub failed: usize,
}

impl TeamRuntime {
    pub fn start(repo_root: PathBuf, max_parallel: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicUsize::new(0));
        let launched = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicUsize::new(0));

        let stop_thread = stop.clone();
        let running_thread = running.clone();
        let launched_thread = launched.clone();
        let completed_thread = completed.clone();
        let failed_thread = failed.clone();
        let handle = thread::spawn(move || {
            scheduler_loop(
                repo_root,
                max_parallel.max(1),
                stop_thread,
                running_thread,
                launched_thread,
                completed_thread,
                failed_thread,
            );
        });

        Self {
            max_parallel: max_parallel.max(1),
            stop,
            running,
            launched,
            completed,
            failed,
            handle: Some(handle),
        }
    }

    pub fn snapshot(&self) -> TeamSnapshot {
        TeamSnapshot {
            max_parallel: self.max_parallel,
            running: self.running.load(Ordering::Relaxed),
            launched: self.launched.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for TeamRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

fn scheduler_loop(
    repo_root: PathBuf,
    max_parallel: usize,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicUsize>,
    launched: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    failed: Arc<AtomicUsize>,
) {
    let mut workers: HashMap<u64, WorkerHandle> = HashMap::new();
    let spawn_mode = choose_spawn_mode();
    let terminal_manager = TerminalManager::with_log_dir(repo_root.join(".team").join("sessions"));
    if let Ok(project) = ProjectContext::new(repo_root.clone()) {
        let _ = project.agents().ensure_profile(
            "team-manager",
            "scheduler",
            "调度待处理任务并拉起对应 worker。",
            &["领取任务", "启动和停止 worker", "维护并发"],
            &["不要直接替代 worker 执行任务"],
        );
        let _ = project.agents().set_state(
            "team-manager",
            "running",
            None,
            Some("scheduler"),
            Some("team-loop"),
            Some("team 调度中"),
        );
        let _ = project
            .budgets()
            .ensure_ledger("team-manager", 90_000, 20_000, 8_000);
        maybe_reflect_energy(
            &project,
            "team-manager",
            "team.start",
            None,
            "team 调度器启动后检查预算状态。",
        );
    }

    while !stop.load(Ordering::Relaxed) {
        let project = match ProjectContext::new(repo_root.clone()) {
            Ok(project) => project,
            Err(err) => {
                eprintln!("team scheduler: init project failed: {err}");
                thread::sleep(Duration::from_millis(800));
                continue;
            }
        };

        let mut finished = Vec::new();
        for (task_id, worker) in &mut workers {
            match worker {
                WorkerHandle::Child { child, .. } => match child.try_wait() {
                    Ok(Some(status)) => {
                        finished.push(*task_id);
                        if status.success() {
                            let current = load_task_status(&project, *task_id)
                                .unwrap_or_else(|_| "failed".to_string());
                            match current.as_str() {
                                "completed" => {
                                    completed.fetch_add(1, Ordering::Relaxed);
                                }
                                "blocked" => {
                                    // 等待用户补充信息，不自动改状态。
                                }
                                "failed" => {
                                    failed.fetch_add(1, Ordering::Relaxed);
                                }
                                _ => {
                                    let _ =
                                        project.tasks().update(*task_id, Some("completed"), None);
                                    completed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        } else {
                            let _ = project.tasks().update(*task_id, Some("failed"), None);
                            failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        eprintln!("team scheduler: wait child failed for task {task_id}: {err}");
                        finished.push(*task_id);
                        let _ = project.tasks().update(*task_id, Some("failed"), None);
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                },
                WorkerHandle::TmuxWindow { task_id, .. } => {
                    let status = match load_task_status(&project, *task_id) {
                        Ok(status) => status,
                        Err(err) => {
                            eprintln!(
                                "team scheduler: read task status failed for task {task_id}: {err}"
                            );
                            continue;
                        }
                    };
                    if status == "completed" {
                        completed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "failed" {
                        failed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "blocked" {
                        finished.push(*task_id);
                    }
                }
                WorkerHandle::TerminalSession {
                    task_id,
                    session_id,
                } => {
                    let status = match load_task_status(&project, *task_id) {
                        Ok(status) => status,
                        Err(err) => {
                            eprintln!(
                                "team scheduler: read task status failed for task {task_id}: {err}"
                            );
                            continue;
                        }
                    };
                    if status == "completed" {
                        completed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "failed" {
                        failed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "blocked" {
                        finished.push(*task_id);
                    } else if let Ok(info) = terminal_manager.status(session_id) {
                        if !matches!(info.state, SessionState::Running) {
                            let _ = project.tasks().update(*task_id, Some("failed"), None);
                            failed.fetch_add(1, Ordering::Relaxed);
                            finished.push(*task_id);
                        }
                    }
                }
            }
        }
        for task_id in finished {
            if let Some(worker) = workers.remove(&task_id) {
                let _ = mark_worker_stopped(&repo_root, worker_task_id(&worker));
                let _ = project.agents().set_state(
                    &worker_owner(&worker),
                    "idle",
                    None,
                    None,
                    None,
                    Some("任务结束"),
                );
                cleanup_worker(worker, &terminal_manager);
            }
        }

        let allowed_parallel = effective_parallel_for_team_manager(&project, max_parallel);
        while workers.len() < allowed_parallel {
            let min_priority = minimum_priority_for_team_manager(&project);
            let Some(task) = (match project
                .tasks()
                .claim_next_pending_with_min_priority("team-manager", min_priority)
            {
                Ok(task) => task,
                Err(err) => {
                    eprintln!("team scheduler: claim task failed: {err}");
                    None
                }
            }) else {
                break;
            };
            let _ = project.decisions().append(
                "team-manager",
                "task.assigned",
                Some(task.id),
                None,
                &format!("assigned task {} to {}", task.id, task.role_hint),
                &format!(
                    "priority={} met scheduler threshold {} with allowed_parallel={}",
                    task.priority, min_priority, allowed_parallel
                ),
            );

            let worktree_name = if task.worktree.is_empty() {
                format!("team-{}", task.id)
            } else {
                task.worktree.clone()
            };
            let owner = format!("teammate-{}", task.id);

            if task.worktree.is_empty() {
                match project
                    .worktrees()
                    .create(&worktree_name, Some(task.id), "HEAD")
                {
                    Ok(_) => {}
                    Err(err) => {
                        let text = err.to_string();
                        if !text.contains("已存在 worktree") {
                            eprintln!(
                                "team scheduler: create worktree failed for task {}: {}",
                                task.id, err
                            );
                            let _ = project.tasks().update(task.id, Some("failed"), None);
                            failed.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        let _ = project
                            .tasks()
                            .bind_worktree(task.id, &worktree_name, &owner);
                    }
                }
            } else {
                let _ = project
                    .tasks()
                    .bind_worktree(task.id, &worktree_name, &owner);
            }

            match spawn_teammate_process(
                &repo_root,
                task.id,
                &owner,
                &task.role_hint,
                &spawn_mode,
                &terminal_manager,
            ) {
                Ok(worker) => {
                    let _ = register_worker_endpoint(&repo_root, &owner, &worker);
                    let _ = register_worker_agent(&project, &owner, &task.role_hint, &worker);
                    let _ = project.budgets().record_usage("team-manager", 25);
                    maybe_reflect_energy(
                        &project,
                        "team-manager",
                        "worker.spawn",
                        Some(task.id),
                        "拉起 worker 后检查调度器预算状态。",
                    );
                    launched.fetch_add(1, Ordering::Relaxed);
                    workers.insert(task.id, worker);
                }
                Err(err) => {
                    eprintln!(
                        "team scheduler: spawn teammate failed for task {}: {}",
                        task.id, err
                    );
                    let _ = project.tasks().update(task.id, Some("failed"), None);
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        running.store(workers.len(), Ordering::Relaxed);
        thread::sleep(Duration::from_millis(600));
    }

    for (_, worker) in workers {
        let _ = mark_worker_stopped(&repo_root, worker_task_id(&worker));
        if let Ok(project) = ProjectContext::new(repo_root.clone()) {
            let _ = project.agents().set_state(
                &worker_owner(&worker),
                "idle",
                None,
                None,
                None,
                Some("team 停止"),
            );
        }
        cleanup_worker(worker, &terminal_manager);
    }
    if let Ok(project) = ProjectContext::new(repo_root) {
        let _ = project.agents().set_state(
            "team-manager",
            "idle",
            None,
            Some("scheduler"),
            Some("team-loop"),
            Some("team 已停止"),
        );
    }
}

fn spawn_teammate_process(
    repo_root: &Path,
    task_id: u64,
    owner: &str,
    role_hint: &str,
    spawn_mode: &SpawnMode,
    terminal_manager: &TerminalManager,
) -> anyhow::Result<WorkerHandle> {
    if matches!(spawn_mode, SpawnMode::Tmux) {
        return spawn_teammate_in_tmux(repo_root, task_id, owner, role_hint);
    }
    if matches!(spawn_mode, SpawnMode::Terminal) {
        return spawn_teammate_in_terminal(repo_root, task_id, owner, role_hint, terminal_manager);
    }

    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
        .arg("teammate-run")
        .arg("--repo-root")
        .arg(repo_root)
        .arg("--task-id")
        .arg(task_id.to_string())
        .arg("--owner")
        .arg(owner)
        .arg("--role-hint")
        .arg(role_hint)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(WorkerHandle::Child { task_id, child })
}

fn spawn_teammate_in_tmux(
    repo_root: &Path,
    task_id: u64,
    owner: &str,
    role_hint: &str,
) -> anyhow::Result<WorkerHandle> {
    ensure_tmux_session("rustpilot-team")?;
    let exe = std::env::current_exe()?;
    let window_name = format!("rustpilot-team:teammate-{}", task_id);
    let pane_name = format!("teammate-{}", task_id);
    let command = format!(
        "{} teammate-run --repo-root {} --task-id {} --owner {} --role-hint {}",
        shell_quote(exe.to_string_lossy().as_ref()),
        shell_quote(repo_root.to_string_lossy().as_ref()),
        task_id,
        shell_quote(owner),
        shell_quote(role_hint)
    );
    let output = Command::new("tmux")
        .args([
            "new-window",
            "-d",
            "-t",
            "rustpilot-team",
            "-n",
            &pane_name,
            &command,
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-window failed: {}", stderr.trim());
    }
    Ok(WorkerHandle::TmuxWindow {
        task_id,
        window_name,
    })
}

fn spawn_teammate_in_terminal(
    repo_root: &Path,
    task_id: u64,
    owner: &str,
    role_hint: &str,
    terminal_manager: &TerminalManager,
) -> anyhow::Result<WorkerHandle> {
    let exe = std::env::current_exe()?;
    let info = terminal_manager.create(TerminalCreateRequest {
        cwd: Some(repo_root.to_path_buf()),
        shell: None,
        env: Vec::new(),
    })?;
    let command = format!(
        "{} teammate-run --repo-root {} --task-id {} --owner {} --role-hint {}\n",
        shell_quote(exe.to_string_lossy().as_ref()),
        shell_quote(repo_root.to_string_lossy().as_ref()),
        task_id,
        shell_quote(owner),
        shell_quote(role_hint)
    );
    terminal_manager.write(&info.id, &command)?;
    println!(
        "team scheduler: task {} started in terminal session {}",
        task_id, info.id
    );
    Ok(WorkerHandle::TerminalSession {
        task_id,
        session_id: info.id,
    })
}

pub async fn run_teammate_once(
    repo_root: PathBuf,
    task_id: u64,
    owner: String,
    role_hint: String,
) -> anyhow::Result<()> {
    dotenvy::from_path(repo_root.join(".env")).ok();
    let llm = LlmConfig::from_repo_root(&repo_root)?;
    let project = ProjectContext::new(repo_root.clone())?;
    let client = reqwest::Client::builder()
        .user_agent(default_llm_user_agent())
        .timeout(Duration::from_secs(llm_timeout_secs_for_provider(
            &llm.provider,
        )))
        .build()?;

    let raw_task = project.tasks().get(task_id)?;
    let task: TaskRecord = serde_json::from_str(&raw_task)?;
    let trace_id = format!("task-{}", task_id);
    let task_subject = task.subject.clone();
    if task.worktree.is_empty() {
        anyhow::bail!("任务 {} 没有绑定 worktree", task_id);
    }

    project
        .tasks()
        .update(task_id, Some("in_progress"), Some(&owner))?;
    let _ = project.mailbox().send_typed(
        &owner,
        "lead",
        "task.started",
        &format!("任务 #{} 已启动：{}", task_id, task_subject),
        Some(task_id),
        Some(&trace_id),
        false,
        None,
    );

    let config = worker_role_config(&role_hint);
    let _system_prompt = format!(
        "你是团队成员 {}，role={}，task_priority={}。{} 只完成当前任务并汇报结果；任务是控制面，worktree 是执行面。需要协作时使用 team_send 和 team_inbox。仓库: {}",
        owner,
        config.role,
        task.priority,
        config.prompt_focus,
        repo_root.display()
    );
    let system_prompt = render_worker_system_prompt(
        &repo_root,
        &owner,
        config.role,
        &task.priority,
        config.prompt_focus,
    )?;
    let task_prompt = if task.description.trim().is_empty() {
        task_subject.clone()
    } else {
        format!("{}\n\n{}", task_subject, task.description)
    };
    let _ = project.budgets().record_usage(
        &owner,
        estimate_text_tokens(&task_prompt).saturating_add(60),
    );
    maybe_reflect_energy(
        &project,
        &owner,
        "task.start",
        Some(task_id),
        "worker 接手任务后检查预算状态。",
    );

    if let Some(seconds) = detect_timer_seconds(&task_prompt) {
        let _ = project.mailbox().send_typed(
            &owner,
            "lead",
            "task.progress",
            &format!("定时器任务 #{} 已进入长运行，间隔={}s", task_id, seconds),
            Some(task_id),
            Some(&trace_id),
            false,
            None,
        );
        run_timer_agent(task_id, &owner, seconds);
        return Ok(());
    }

    let mut messages = vec![
        Message {
            role: "system".to_string(),
            content: Some(system_prompt),
            tool_call_id: None,
            tool_calls: None,
        },
        Message {
            role: "user".to_string(),
            content: Some(task_prompt),
            tool_call_id: None,
            tool_calls: None,
        },
    ];

    let tools = tools_for_role_and_priority(&role_hint, &task.priority);
    let progress = crate::activity::new_activity_handle();
    let report = AgentProgressReport {
        from: owner.clone(),
        to: "lead".to_string(),
        task_id: Some(task_id),
        trace_id: Some(trace_id.clone()),
    };
    let mut clarification_cursor = 0usize;
    loop {
        let result = run_agent_loop(
            &client,
            &llm,
            &project,
            &mut messages,
            &tools,
            progress.clone(),
            Some(&report),
        )
        .await;
        match result {
            Ok(()) => {
                if let Some(question) = detect_user_clarification_need(&messages) {
                    let _ = project.budgets().record_usage(&owner, 30);
                    let _ = project.reflections().append(
                        &owner,
                        "task.blocked",
                        Some(task_id),
                        "worker 需要用户澄清后才能继续。",
                        &["当前信息不足", "任务进入 blocked 状态"],
                        Some("等待用户补充信息后继续执行"),
                        true,
                    );
                    let _ = project.proposals().create(
                        &owner,
                        "task.blocked",
                        Some(task_id),
                        "补充任务上下文以解除 blocked",
                        "worker 需要用户澄清后才能继续。",
                        &["当前信息不足", "任务进入 blocked 状态"],
                        Some("等待用户补充信息后继续执行"),
                    );
                    maybe_reflect_energy(
                        &project,
                        &owner,
                        "task.blocked",
                        Some(task_id),
                        "worker 进入 blocked 状态后检查预算状态。",
                    );
                    let _ = project
                        .tasks()
                        .update(task_id, Some("blocked"), Some(&owner));
                    let _ = project.mailbox().send_typed(
                        &owner,
                        "lead",
                        "task.request_clarification",
                        &question,
                        Some(task_id),
                        Some(&trace_id),
                        true,
                        None,
                    );
                    let clarification = wait_for_clarification(
                        &project,
                        &owner,
                        task_id,
                        &trace_id,
                        &mut clarification_cursor,
                    )
                    .await?;
                    let _ =
                        project
                            .tasks()
                            .append_user_reply(task_id, &clarification, "in_progress");
                    messages.push(Message {
                        role: "user".to_string(),
                        content: Some(format!("用户补充信息：{}\n请继续完成任务。", clarification)),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                    continue;
                }
                let _ = project
                    .tasks()
                    .update(task_id, Some("completed"), Some(&owner));
                let _ = project.budgets().record_usage(&owner, 80);
                let _ = project.reflections().append(
                    &owner,
                    "task.completed",
                    Some(task_id),
                    "worker 完成任务并回传结果。",
                    &["任务已完成"],
                    Some("回到 idle 并等待下一项工作"),
                    false,
                );
                maybe_reflect_energy(
                    &project,
                    &owner,
                    "task.completed",
                    Some(task_id),
                    "worker 完成任务后检查预算状态。",
                );
                let _ = project.mailbox().send_typed(
                    &owner,
                    "lead",
                    "task.result",
                    &format!("任务 #{} 已完成：{}", task_id, task_subject),
                    Some(task_id),
                    Some(&trace_id),
                    true,
                    None,
                );
                return Ok(());
            }
            Err(err) => {
                let error_text = format!("{err:#}");
                let adaptation = adapt_worker_prompt_detailed(&repo_root, &error_text)
                    .unwrap_or_else(|_| crate::prompt_manager::PromptAdaptation {
                        changed: false,
                        file_path: repo_root.join(".team").join("worker_agent_prompt.md"),
                        before: String::new(),
                        after: String::new(),
                        recovery: None,
                    });
                if adaptation.changed {
                    if let Some(recovery) = adaptation.recovery.as_ref() {
                        let _ = project.prompt_history().append(
                            "worker",
                            &owner,
                            &adaptation.file_path.display().to_string(),
                            &recovery.strategy,
                            &recovery.trigger,
                            &adaptation.before,
                            &adaptation.after,
                        );
                    }
                    refresh_worker_system_prompt(
                        &repo_root,
                        &mut messages,
                        &owner,
                        config.role,
                        &task.priority,
                        config.prompt_focus,
                    )?;
                    if run_agent_loop(
                        &client,
                        &llm,
                        &project,
                        &mut messages,
                        &tools,
                        progress.clone(),
                        Some(&report),
                    )
                    .await
                    .is_ok()
                    {
                        let _ = project.decisions().append(
                            &owner,
                            "task.recovered",
                            Some(task_id),
                            None,
                            "worker recovered after auto-adjusting prompt",
                            &error_text,
                        );
                        continue;
                    }
                }
                let _ = project
                    .tasks()
                    .update(task_id, Some("failed"), Some(&owner));
                let _ = project.budgets().record_usage(&owner, 50);
                let _ = project.reflections().append(
                    &owner,
                    "task.failed",
                    Some(task_id),
                    "worker 执行任务失败。",
                    &["任务执行失败", "需要检查错误原因"],
                    Some("分析失败原因并决定是否重试"),
                    true,
                );
                let _ = project.proposals().create(
                    &owner,
                    "task.failed",
                    Some(task_id),
                    "分析失败原因并形成修复任务",
                    "worker 执行任务失败。",
                    &["任务执行失败", "需要检查错误原因"],
                    Some("分析失败原因并决定是否重试"),
                );
                maybe_reflect_energy(
                    &project,
                    &owner,
                    "task.failed",
                    Some(task_id),
                    "worker 失败后检查预算状态。",
                );
                let _ = project.mailbox().send_typed(
                    &owner,
                    "lead",
                    "task.failed",
                    &format!("任务 #{} 失败：{}", task_id, err),
                    Some(task_id),
                    Some(&trace_id),
                    true,
                    None,
                );
                return Err(err);
            }
        }
    }
}

fn refresh_worker_system_prompt(
    repo_root: &Path,
    messages: &mut Vec<Message>,
    owner: &str,
    role: &str,
    task_priority: &str,
    prompt_focus: &str,
) -> anyhow::Result<()> {
    let prompt = render_worker_system_prompt(repo_root, owner, role, task_priority, prompt_focus)?;
    if let Some(system) = messages
        .first_mut()
        .filter(|message| message.role == "system")
    {
        system.content = Some(prompt);
        return Ok(());
    }
    messages.insert(
        0,
        Message {
            role: "system".to_string(),
            content: Some(prompt),
            tool_call_id: None,
            tool_calls: None,
        },
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct WorkerMailPoll {
    next_cursor: usize,
    items: Vec<WorkerMailItem>,
}

#[derive(Debug, Deserialize)]
struct WorkerMailItem {
    msg_type: String,
    trace_id: String,
    message: String,
    task_id: Option<u64>,
}

async fn wait_for_clarification(
    project: &ProjectContext,
    owner: &str,
    task_id: u64,
    trace_id: &str,
    cursor: &mut usize,
) -> anyhow::Result<String> {
    loop {
        let raw = project.mailbox().poll(owner, *cursor, 20)?;
        let polled: WorkerMailPoll = serde_json::from_str(&raw)?;
        *cursor = polled.next_cursor;
        for item in polled.items {
            if item.msg_type == "task.clarification"
                && item.task_id == Some(task_id)
                && item.trace_id == trace_id
            {
                return Ok(item.message);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn detect_user_clarification_need(messages: &[Message]) -> Option<String> {
    let text = messages
        .iter()
        .rev()
        .find(|item| item.role == "assistant")
        .and_then(|item| item.content.as_ref())?
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let looks_like_question = text.contains('？') || text.contains('?');
    let asks_input = lower.contains("please provide")
        || text.contains("请提供")
        || text.contains("请问")
        || text.contains("用户名")
        || text.contains("邮箱")
        || text.contains("需要你提供");
    if looks_like_question && asks_input {
        return Some(text);
    }
    None
}

fn detect_timer_seconds(text: &str) -> Option<u64> {
    let lower = text.to_lowercase();
    let is_timer_task =
        lower.contains("timer") || (text.contains("定时器") && text.contains("打印"));
    if !is_timer_task {
        return None;
    }

    let mut value: Option<u64> = None;
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(parsed) = current.parse::<u64>() {
                value = Some(parsed);
                break;
            }
            current.clear();
        }
    }
    if value.is_none() && !current.is_empty() {
        value = current.parse::<u64>().ok();
    }

    let seconds = value.unwrap_or(5).max(1);
    Some(seconds)
}

fn run_timer_agent(task_id: u64, owner: &str, seconds: u64) {
    println!(
        "[timer-agent] task#{} owner={} started, interval={}s",
        task_id, owner, seconds
    );
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        println!(
            "[timer-agent] task#{} owner={} ts_unix={} interval={}s",
            task_id, owner, now, seconds
        );
        thread::sleep(Duration::from_secs(seconds));
    }
}

fn choose_spawn_mode() -> SpawnMode {
    let value = std::env::var("RUSTPILOT_TEAM_SPAWN")
        .unwrap_or_else(|_| "auto".to_string())
        .to_lowercase();
    match value.as_str() {
        "inherit" => SpawnMode::Inherit,
        "terminal" => SpawnMode::Terminal,
        "tmux" => {
            if has_tmux() {
                SpawnMode::Tmux
            } else {
                SpawnMode::Terminal
            }
        }
        _ => {
            if has_tmux() {
                SpawnMode::Tmux
            } else {
                SpawnMode::Terminal
            }
        }
    }
}

fn has_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn ensure_tmux_session(name: &str) -> anyhow::Result<()> {
    let has = Command::new("tmux")
        .args(["has-session", "-t", name])
        .output();
    if let Ok(output) = has
        && output.status.success()
    {
        return Ok(());
    }

    let output = Command::new("tmux")
        .args(["new-session", "-d", "-s", name])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-session failed: {}", stderr.trim());
    }
    Ok(())
}

fn load_task_status(project: &ProjectContext, task_id: u64) -> anyhow::Result<String> {
    let raw = project.tasks().get(task_id)?;
    let task: TaskRecord = serde_json::from_str(&raw)?;
    Ok(task.status)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn cleanup_worker(worker: WorkerHandle, terminal_manager: &TerminalManager) {
    match worker {
        WorkerHandle::Child { mut child, .. } => {
            let _ = child.kill();
            let _ = child.wait();
        }
        WorkerHandle::TmuxWindow { window_name, .. } => {
            let _ = Command::new("tmux")
                .args(["kill-window", "-t", &window_name])
                .output();
        }
        WorkerHandle::TerminalSession { session_id, .. } => {
            let _ = terminal_manager.kill(&session_id);
        }
    }
}

fn worker_task_id(worker: &WorkerHandle) -> u64 {
    match worker {
        WorkerHandle::Child { task_id, .. } => *task_id,
        WorkerHandle::TmuxWindow { task_id, .. } => *task_id,
        WorkerHandle::TerminalSession { task_id, .. } => *task_id,
    }
}

fn register_worker_endpoint(
    repo_root: &Path,
    owner: &str,
    worker: &WorkerHandle,
) -> anyhow::Result<()> {
    let endpoint = match worker {
        WorkerHandle::Child { task_id, .. } => WorkerEndpoint {
            task_id: *task_id,
            owner: owner.to_string(),
            channel: "inherit".to_string(),
            target: "stdout".to_string(),
            status: "running".to_string(),
        },
        WorkerHandle::TmuxWindow {
            task_id,
            window_name,
        } => WorkerEndpoint {
            task_id: *task_id,
            owner: owner.to_string(),
            channel: "tmux".to_string(),
            target: window_name.clone(),
            status: "running".to_string(),
        },
        WorkerHandle::TerminalSession {
            task_id,
            session_id,
        } => WorkerEndpoint {
            task_id: *task_id,
            owner: owner.to_string(),
            channel: "terminal".to_string(),
            target: session_id.clone(),
            status: "running".to_string(),
        },
    };
    let mut items = load_worker_endpoints(repo_root)?;
    upsert_worker_endpoint(&mut items, endpoint);
    save_worker_endpoints(repo_root, &items)
}

fn register_worker_agent(
    project: &ProjectContext,
    owner: &str,
    role_hint: &str,
    worker: &WorkerHandle,
) -> anyhow::Result<()> {
    let config = worker_role_config(role_hint);
    project.agents().ensure_profile(
        owner,
        config.role,
        config.mission,
        &["执行单个任务", "回报进度", "请求澄清"],
        &["不要脱离当前任务范围自行扩张目标"],
    )?;
    project.budgets().ensure_ledger(
        owner,
        config.daily_limit,
        config.period_limit,
        config.task_soft_limit,
    )?;
    match worker {
        WorkerHandle::Child { task_id, .. } => project.agents().set_state(
            owner,
            "running",
            Some(*task_id),
            Some("inherit"),
            Some("stdout"),
            Some("worker 运行中"),
        )?,
        WorkerHandle::TmuxWindow {
            task_id,
            window_name,
        } => project.agents().set_state(
            owner,
            "running",
            Some(*task_id),
            Some("tmux"),
            Some(window_name),
            Some("worker 运行中"),
        )?,
        WorkerHandle::TerminalSession {
            task_id,
            session_id,
        } => project.agents().set_state(
            owner,
            "running",
            Some(*task_id),
            Some("terminal"),
            Some(session_id),
            Some("worker 运行中"),
        )?,
    }
    Ok(())
}

fn mark_worker_stopped(repo_root: &Path, task_id: u64) -> anyhow::Result<()> {
    let mut items = load_worker_endpoints(repo_root)?;
    for item in &mut items {
        if item.task_id == task_id {
            item.status = "stopped".to_string();
        }
    }
    save_worker_endpoints(repo_root, &items)
}

pub fn get_worker_endpoint(
    repo_root: &Path,
    task_id: u64,
) -> anyhow::Result<Option<WorkerEndpoint>> {
    Ok(load_worker_endpoints(repo_root)?
        .into_iter()
        .find(|item| item.task_id == task_id))
}

pub fn send_input_to_worker(repo_root: &Path, task_id: u64, input: &str) -> anyhow::Result<String> {
    let endpoint = get_worker_endpoint(repo_root, task_id)?
        .ok_or_else(|| anyhow::anyhow!("未找到 task {} 的 worker 映射", task_id))?;
    if endpoint.status != "running" {
        anyhow::bail!("task {} 的 worker 已停止", task_id);
    }

    match endpoint.channel.as_str() {
        "terminal" => {
            let manager = TerminalManager::with_log_dir(repo_root.join(".team").join("sessions"));
            manager.write(&endpoint.target, &format!("{}\n", input))?;
            Ok(format!(
                "已发送到 worker(task={}) terminal session {}",
                task_id, endpoint.target
            ))
        }
        "tmux" => {
            let output = Command::new("tmux")
                .args(["send-keys", "-t", &endpoint.target, input, "Enter"])
                .output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("tmux send-keys 失败: {}", stderr.trim());
            }
            Ok(format!(
                "已发送到 worker(task={}) tmux window {}",
                task_id, endpoint.target
            ))
        }
        "inherit" => {
            anyhow::bail!("inherit 模式下不支持路由输入给子 agent，请使用 tmux/terminal 模式")
        }
        other => anyhow::bail!("未知 worker 通道: {}", other),
    }
}

fn load_worker_endpoints(repo_root: &Path) -> anyhow::Result<Vec<WorkerEndpoint>> {
    let path = worker_index_path(repo_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path)?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(&content)?)
}

fn save_worker_endpoints(repo_root: &Path, items: &[WorkerEndpoint]) -> anyhow::Result<()> {
    let path = worker_index_path(repo_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(items)?)?;
    Ok(())
}

fn worker_index_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".team").join("agents.json")
}

fn upsert_worker_endpoint(items: &mut Vec<WorkerEndpoint>, endpoint: WorkerEndpoint) {
    if let Some(existing) = items
        .iter_mut()
        .find(|item| item.task_id == endpoint.task_id)
    {
        *existing = endpoint;
    } else {
        items.push(endpoint);
    }
}

fn worker_owner(worker: &WorkerHandle) -> String {
    match worker {
        WorkerHandle::Child { task_id, .. } => format!("teammate-{task_id}"),
        WorkerHandle::TmuxWindow { task_id, .. } => format!("teammate-{task_id}"),
        WorkerHandle::TerminalSession { task_id, .. } => format!("teammate-{task_id}"),
    }
}

fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    chars.saturating_div(4).saturating_add(1)
}

struct WorkerRoleConfig {
    role: &'static str,
    mission: &'static str,
    prompt_focus: &'static str,
    daily_limit: u32,
    period_limit: u32,
    task_soft_limit: u32,
}

fn team_manager_parallel_for_mode(mode: Option<EnergyMode>, max_parallel: usize) -> usize {
    match mode {
        Some(EnergyMode::Healthy) | None => max_parallel,
        Some(EnergyMode::Constrained) => max_parallel.max(1).div_ceil(2),
        Some(EnergyMode::Low) => 1,
        Some(EnergyMode::Exhausted) => 0,
    }
}

fn team_manager_min_priority_for_mode(mode: Option<EnergyMode>) -> &'static str {
    match mode {
        Some(EnergyMode::Healthy) | None => "low",
        Some(EnergyMode::Constrained) => "medium",
        Some(EnergyMode::Low) => "high",
        Some(EnergyMode::Exhausted) => "critical",
    }
}

fn worker_role_config(role_hint: &str) -> WorkerRoleConfig {
    match role_hint {
        "critic" => WorkerRoleConfig {
            role: "critic",
            mission: "审阅、整理或分析任务信息，并产出结构化结论。",
            prompt_focus: "优先做审阅、归纳、风险判断和结构化输出，避免直接扩散为实现任务。",
            daily_limit: 90_000,
            period_limit: 20_000,
            task_soft_limit: 8_000,
        },
        "design" => WorkerRoleConfig {
            role: "design",
            mission: "处理页面、设计、样式和交互相关任务，并回报结果。",
            prompt_focus: "优先关注界面、文案、交互和视觉一致性，避免进入无关实现细节。",
            daily_limit: 120_000,
            period_limit: 30_000,
            task_soft_limit: 14_000,
        },
        _ => WorkerRoleConfig {
            role: "developer",
            mission: "根据调度器分配的任务执行实现、验证并汇报结果。",
            prompt_focus: "优先关注实现、验证和最小改动完成任务，避免偏离需求做额外设计。",
            daily_limit: 160_000,
            period_limit: 40_000,
            task_soft_limit: 20_000,
        },
    }
}

fn tools_for_role_and_priority(role_hint: &str, priority: &str) -> Vec<Tool> {
    let all = tool_definitions();
    let allowed: Option<&[&str]> = match role_hint {
        "critic" => Some(&[
            "read_file",
            "terminal_list",
            "terminal_status",
            "terminal_read",
            "team_send",
            "team_ack",
            "team_poll",
            "team_inbox",
            "task_list",
            "task_get",
            "worktree_list",
            "worktree_status",
            "worktree_events",
        ]),
        "design" if priority == "low" => Some(&[
            "read_file",
            "write_file",
            "edit_file",
            "team_send",
            "team_ack",
            "team_poll",
            "team_inbox",
            "task_list",
            "task_get",
            "task_update",
            "worktree_list",
            "worktree_status",
            "worktree_events",
        ]),
        "design" => Some(&[
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "team_send",
            "team_ack",
            "team_poll",
            "team_inbox",
            "task_list",
            "task_get",
            "task_update",
            "worktree_list",
            "worktree_status",
            "worktree_run",
            "worktree_events",
        ]),
        "developer" if priority == "low" => Some(&[
            "read_file",
            "write_file",
            "edit_file",
            "team_send",
            "team_ack",
            "team_poll",
            "team_inbox",
            "task_list",
            "task_get",
            "task_update",
            "worktree_list",
            "worktree_status",
            "worktree_run",
            "worktree_events",
        ]),
        "developer" if priority == "medium" => Some(&[
            "bash",
            "read_file",
            "write_file",
            "edit_file",
            "team_send",
            "team_ack",
            "team_poll",
            "team_inbox",
            "task_list",
            "task_get",
            "task_update",
            "worktree_list",
            "worktree_status",
            "worktree_run",
            "worktree_events",
        ]),
        _ => None,
    };

    let Some(allowed) = allowed else {
        return all;
    };

    all.into_iter()
        .filter(|tool| allowed.contains(&tool.function.name.as_str()))
        .collect()
}

fn tool_names_for_role_and_priority(role_hint: &str, priority: &str) -> Vec<String> {
    tools_for_role_and_priority(role_hint, priority)
        .into_iter()
        .map(|tool| tool.function.name)
        .collect()
}

pub fn render_policy_overview(project: &ProjectContext, max_parallel: usize) -> String {
    let manager_budget = project.budgets().snapshot("team-manager").ok().flatten();
    let manager_mode = manager_budget.as_ref().map(classify_energy);
    let allowed_parallel = team_manager_parallel_for_mode(manager_mode, max_parallel);
    let min_priority = team_manager_min_priority_for_mode(manager_mode);
    let manager_energy = match manager_mode {
        Some(mode) => format!("{mode:?}"),
        None => "Unknown".to_string(),
    };
    let manager_budget_line = match manager_budget {
        Some(item) => format!(
            "budget={}/{} period_limit={} task_soft_limit={}",
            item.used_today, item.daily_limit, item.period_limit, item.task_soft_limit
        ),
        None => "budget=unknown".to_string(),
    };

    let mut lines = vec![
        "policy overview".to_string(),
        format!(
            "- dispatch: energy={} allowed_parallel={}/{} min_priority={} {}",
            manager_energy, allowed_parallel, max_parallel, min_priority, manager_budget_line
        ),
        "- role budgets:".to_string(),
    ];

    for role_hint in ["developer", "design", "critic"] {
        let config = worker_role_config(role_hint);
        lines.push(format!(
            "  {}: role={} daily={} period={} task_soft={} focus={}",
            role_hint,
            config.role,
            config.daily_limit,
            config.period_limit,
            config.task_soft_limit,
            config.prompt_focus
        ));
    }

    lines.push("- tool access:".to_string());
    lines.push(format!(
        "  critic any: {}",
        tool_names_for_role_and_priority("critic", "critical").join(", ")
    ));
    lines.push(format!(
        "  design low: {}",
        tool_names_for_role_and_priority("design", "low").join(", ")
    ));
    lines.push(format!(
        "  design medium+: {}",
        tool_names_for_role_and_priority("design", "medium").join(", ")
    ));
    lines.push(format!(
        "  developer low: {}",
        tool_names_for_role_and_priority("developer", "low").join(", ")
    ));
    lines.push(format!(
        "  developer medium: {}",
        tool_names_for_role_and_priority("developer", "medium").join(", ")
    ));
    lines.push(format!(
        "  developer high+: {}",
        tool_names_for_role_and_priority("developer", "high").join(", ")
    ));

    lines.join("\n")
}

pub fn render_task_policy(
    project: &ProjectContext,
    task_id: u64,
    max_parallel: usize,
) -> anyhow::Result<String> {
    let task = project.tasks().get_record(task_id)?;
    let manager_mode = project
        .budgets()
        .snapshot("team-manager")?
        .map(|item| classify_energy(&item));
    let min_priority = team_manager_min_priority_for_mode(manager_mode);
    let can_dispatch_now = task.status == "pending"
        && task_priority_rank(&task.priority) >= task_priority_rank(min_priority);
    let dispatch_reason = if task.status != "pending" {
        format!(
            "status={} so it is not eligible for a new claim",
            task.status
        )
    } else if can_dispatch_now {
        format!(
            "priority={} meets current scheduler threshold {}",
            task.priority, min_priority
        )
    } else {
        format!(
            "priority={} is below current scheduler threshold {}",
            task.priority, min_priority
        )
    };
    let configured_tools = tool_names_for_role_and_priority(&task.role_hint, &task.priority);
    let decisions = project
        .decisions()
        .list_related(Some(task.id), None, None, 3)?;
    let recent_decisions = if decisions.is_empty() {
        "none".to_string()
    } else {
        decisions
            .into_iter()
            .map(|item| format!("{}:{} ({})", item.agent_id, item.action, item.reason))
            .collect::<Vec<_>>()
            .join(" | ")
    };

    Ok([
        format!("task policy #{}", task.id),
        format!("- subject: {}", task.subject),
        format!(
            "- routing: role_hint={} priority={} status={} owner={}",
            task.role_hint,
            task.priority,
            task.status,
            if task.owner.is_empty() {
                "unassigned"
            } else {
                &task.owner
            }
        ),
        format!(
            "- scheduler: allowed_parallel={}/{} min_priority={} decision={}",
            team_manager_parallel_for_mode(manager_mode, max_parallel),
            max_parallel,
            min_priority,
            dispatch_reason
        ),
        format!(
            "- tools: {}",
            if configured_tools.is_empty() {
                "none".to_string()
            } else {
                configured_tools.join(", ")
            }
        ),
        format!("- recent decisions: {}", recent_decisions),
    ]
    .join("\n"))
}

pub fn render_agent_policy(project: &ProjectContext, agent_id: &str) -> anyhow::Result<String> {
    let profile = project.agents().profile(agent_id)?;
    let state = project.agents().state(agent_id)?;
    let budget = project.budgets().snapshot(agent_id)?;

    let role = profile
        .as_ref()
        .map(|item| item.role.as_str())
        .unwrap_or("unknown");
    let mission = profile
        .as_ref()
        .map(|item| item.mission.as_str())
        .unwrap_or("unknown");
    let status = state
        .as_ref()
        .map(|item| item.status.as_str())
        .unwrap_or("unknown");
    let current_task_id = state.as_ref().and_then(|item| item.current_task_id);
    let effective_role_hint = match role {
        "design" => "design",
        "critic" => "critic",
        _ => "developer",
    };
    let task_priority = current_task_id
        .and_then(|task_id| {
            project
                .tasks()
                .get_record(task_id)
                .ok()
                .map(|task| task.priority)
        })
        .unwrap_or_else(|| "medium".to_string());
    let configured_tools = tool_names_for_role_and_priority(effective_role_hint, &task_priority);
    let decisions = project
        .decisions()
        .list_related(None, None, Some(agent_id), 3)?;
    let recent_decisions = if decisions.is_empty() {
        "none".to_string()
    } else {
        decisions
            .into_iter()
            .map(|item| {
                let target = item
                    .task_id
                    .map(|id| format!("task={id}"))
                    .or_else(|| item.proposal_id.map(|id| format!("proposal={id}")))
                    .unwrap_or_else(|| "global".to_string());
                format!("{} {} ({})", item.action, target, item.reason)
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let budget_line = match budget {
        Some(item) => format!(
            "energy={:?} budget={}/{} period_limit={} task_soft_limit={}",
            classify_energy(&item),
            item.used_today,
            item.daily_limit,
            item.period_limit,
            item.task_soft_limit
        ),
        None => "energy=Unknown budget=unknown".to_string(),
    };
    let scope = profile
        .as_ref()
        .map(|item| item.scope.join(", "))
        .filter(|item| !item.is_empty())
        .unwrap_or_else(|| "none".to_string());
    let forbidden = profile
        .as_ref()
        .map(|item| item.forbidden.join(", "))
        .filter(|item| !item.is_empty())
        .unwrap_or_else(|| "none".to_string());

    Ok([
        format!("agent policy {}", agent_id),
        format!("- role={} status={} {}", role, status, budget_line),
        format!("- mission: {}", mission),
        format!("- scope: {}", scope),
        format!("- forbidden: {}", forbidden),
        format!(
            "- task context: current_task={} effective_priority={}",
            current_task_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_string()),
            task_priority
        ),
        format!(
            "- tools: {}",
            if configured_tools.is_empty() {
                "none".to_string()
            } else {
                configured_tools.join(", ")
            }
        ),
        format!("- recent decisions: {}", recent_decisions),
    ]
    .join("\n"))
}

fn effective_parallel_for_team_manager(project: &ProjectContext, max_parallel: usize) -> usize {
    match project.budgets().energy_mode("team-manager") {
        Ok(Some(EnergyMode::Healthy)) | Ok(None) => {
            let _ = project.agents().set_state(
                "team-manager",
                "running",
                None,
                Some("scheduler"),
                Some("team-loop"),
                Some("正常并发"),
            );
            team_manager_parallel_for_mode(Some(EnergyMode::Healthy), max_parallel)
        }
        Ok(Some(EnergyMode::Constrained)) => {
            let limited =
                team_manager_parallel_for_mode(Some(EnergyMode::Constrained), max_parallel);
            let _ = project.agents().set_state(
                "team-manager",
                "running",
                None,
                Some("scheduler"),
                Some("team-loop"),
                Some(&format!("受限并发: {}", limited)),
            );
            limited
        }
        Ok(Some(EnergyMode::Low)) => {
            let _ = project.agents().set_state(
                "team-manager",
                "running",
                None,
                Some("scheduler"),
                Some("team-loop"),
                Some("低预算: 仅允许单任务并发"),
            );
            team_manager_parallel_for_mode(Some(EnergyMode::Low), max_parallel)
        }
        Ok(Some(EnergyMode::Exhausted)) => {
            let _ = project.agents().set_state(
                "team-manager",
                "cooldown",
                None,
                Some("scheduler"),
                Some("team-loop"),
                Some("预算耗尽: 暂停领取新任务"),
            );
            team_manager_parallel_for_mode(Some(EnergyMode::Exhausted), max_parallel)
        }
        Err(_) => max_parallel,
    }
}

fn minimum_priority_for_team_manager(project: &ProjectContext) -> &'static str {
    match project.budgets().energy_mode("team-manager") {
        Ok(mode) => team_manager_min_priority_for_mode(mode),
        Err(_) => "low",
    }
}

fn maybe_reflect_energy(
    project: &ProjectContext,
    agent_id: &str,
    trigger: &str,
    task_id: Option<u64>,
    summary: &str,
) {
    let Ok(mode) = project.budgets().energy_mode(agent_id) else {
        return;
    };
    match mode {
        Some(EnergyMode::Low) => {
            let _ = project.reflections().append(
                agent_id,
                trigger,
                task_id,
                summary,
                &["预算进入低水位", "应压缩后续执行范围"],
                Some("减少探索、优先完成收尾动作"),
                true,
            );
            let _ = project.proposals().create(
                agent_id,
                trigger,
                task_id,
                "压缩执行范围并优先收尾",
                summary,
                &["预算进入低水位", "应压缩后续执行范围"],
                Some("减少探索、优先完成收尾动作"),
            );
        }
        Some(EnergyMode::Exhausted) => {
            let _ = project.reflections().append(
                agent_id,
                trigger,
                task_id,
                summary,
                &["预算接近耗尽", "应暂停非关键动作"],
                Some("只保留必要汇报并等待预算恢复"),
                true,
            );
            let _ = project.proposals().create(
                agent_id,
                trigger,
                task_id,
                "暂停非关键动作并等待预算恢复",
                summary,
                &["预算接近耗尽", "应暂停非关键动作"],
                Some("只保留必要汇报并等待预算恢复"),
            );
        }
        _ => {}
    }
}
