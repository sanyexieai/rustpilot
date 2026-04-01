use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::agent::{AgentProgressReport, run_agent_loop, tool_definitions};
use crate::app_support::{current_tenant_id, current_user_id, root_actor_id};
use crate::config::LlmConfig;
use crate::config::default_llm_user_agent;
use crate::constants::WORKER_STUCK_NOTIFY_SECS;
use crate::launch_log;
use crate::openai_compat::Message;
use crate::openai_compat::Tool;
use crate::project_tools::{EnergyMode, LaunchRecord, ProjectContext, TaskRecord, classify_energy};
use crate::prompt_manager::{adapt_worker_prompt_detailed, render_worker_system_prompt};
use crate::resident_agents::{request_worker_launch, stop_launch, wait_for_launch_running};
use crate::runtime_env::llm_timeout_secs_for_provider;
use crate::terminal_session::{SessionState, TerminalCreateRequest, TerminalManager};
use crate::workflow_defaults::task_priority_rank;
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
    Child {
        task_id: u64,
        child: Child,
    },
    TmuxWindow {
        task_id: u64,
        window_name: String,
    },
    TerminalSession {
        task_id: u64,
        session_id: String,
    },
    LaunchManaged {
        task_id: u64,
        owner: String,
        launch_id: String,
    },
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
    let mut spawn_times: HashMap<u64, Instant> = HashMap::new();
    let spawn_mode = choose_spawn_mode();
    let terminal_manager =
        TerminalManager::with_log_dir(ProjectContext::scoped_team_dir_for(&repo_root).join("sessions"));
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
            let task_status = match load_task_status(&project, *task_id) {
                Ok(status) => status,
                Err(err) => {
                    eprintln!("team scheduler: read task status failed for task {task_id}: {err}");
                    continue;
                }
            };
            if matches!(task_status.as_str(), "paused" | "cancelled") {
                finished.push(*task_id);
                continue;
            }
            match worker {
                WorkerHandle::Child { child, .. } => match child.try_wait() {
                    Ok(Some(status)) => {
                        finished.push(*task_id);
                        if status.success() {
                            match task_status.as_str() {
                                "completed" => {
                                    completed.fetch_add(1, Ordering::Relaxed);
                                }
                                "blocked" => {
                                    // 等待用户补充信息，不自动改状态。
                                }
                                "paused" | "cancelled" => {}
                                "failed" => {
                                    failed.fetch_add(1, Ordering::Relaxed);
                                }
                                _ => {
                                    let _ = project.tasks().update(
                                        *task_id,
                                        Some("completed"),
                                        None,
                                        None,
                                    );
                                    completed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        } else {
                            let _ = project.tasks().update(*task_id, Some("failed"), None, None);
                            failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        eprintln!("team scheduler: wait child failed for task {task_id}: {err}");
                        finished.push(*task_id);
                        let _ = project.tasks().update(*task_id, Some("failed"), None, None);
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                },
                WorkerHandle::TmuxWindow { task_id, .. } => {
                    let status = &task_status;
                    if status == "completed" {
                        completed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "failed" {
                        failed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if matches!(status.as_str(), "blocked" | "paused" | "cancelled") {
                        finished.push(*task_id);
                    }
                }
                WorkerHandle::TerminalSession {
                    task_id,
                    session_id,
                } => {
                    let status = &task_status;
                    if status == "completed" {
                        completed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "failed" {
                        failed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if matches!(status.as_str(), "blocked" | "paused" | "cancelled") {
                        finished.push(*task_id);
                    } else if let Ok(info) = terminal_manager.status(session_id)
                        && !matches!(info.state, SessionState::Running)
                    {
                        let _ = project.tasks().update(*task_id, Some("failed"), None, None);
                        failed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    }
                }
                WorkerHandle::LaunchManaged {
                    task_id, launch_id, ..
                } => {
                    let status = &task_status;
                    if status == "completed" {
                        completed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if status == "failed" {
                        failed.fetch_add(1, Ordering::Relaxed);
                        finished.push(*task_id);
                    } else if matches!(status.as_str(), "blocked" | "paused" | "cancelled") {
                        finished.push(*task_id);
                    } else {
                        match project.launches().get(launch_id) {
                            Ok(Some(launch)) if launch.status == "running" => {}
                            Ok(Some(launch))
                                if matches!(launch.status.as_str(), "failed" | "stopped") =>
                            {
                                let _ =
                                    project.tasks().update(*task_id, Some("failed"), None, None);
                                failed.fetch_add(1, Ordering::Relaxed);
                                finished.push(*task_id);
                            }
                            Ok(_) => {}
                            Err(err) => {
                                eprintln!(
                                    "team scheduler: read launch status failed for task {task_id}: {err}"
                                );
                            }
                        }
                    }
                }
            }
        }
        for task_id in finished {
            spawn_times.remove(&task_id);
            if let Some(worker) = workers.remove(&task_id) {
                let owner = worker_owner(&worker);
                let _ = mark_worker_stopped(&repo_root, worker_task_id(&worker));
                let _ =
                    project
                        .agents()
                        .set_state(&owner, "idle", None, None, None, Some("任务结束"));
                cleanup_worker(worker, &terminal_manager, &repo_root);
                let _ = reconcile_parent_after_child_exit(&project, task_id);
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

            let owner = format!("teammate-{}", task.id);
            let prepared = match prepare_task_worktree_for_spawn(&project, task.id, &owner) {
                Ok(prepared) => prepared,
                Err(err) => {
                    eprintln!(
                        "team scheduler: prepare worktree failed for task {}: {}",
                        task.id, err
                    );
                    let _ = project.tasks().update(task.id, Some("failed"), None, None);
                    failed.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

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
                    spawn_times.insert(task.id, Instant::now());
                    workers.insert(task.id, worker);
                }
                Err(err) => {
                    if prepared.borrowed_active {
                        let fallback_worktree = format!("team-{}", task.id);
                        match create_and_bind_task_worktree(
                            &project,
                            task.id,
                            &fallback_worktree,
                            &owner,
                        ) {
                            Ok(()) => match spawn_teammate_process(
                                &repo_root,
                                task.id,
                                &owner,
                                &task.role_hint,
                                &spawn_mode,
                                &terminal_manager,
                            ) {
                                Ok(worker) => {
                                    let _ = register_worker_endpoint(&repo_root, &owner, &worker);
                                    let _ = register_worker_agent(
                                        &project,
                                        &owner,
                                        &task.role_hint,
                                        &worker,
                                    );
                                    let _ = project.budgets().record_usage("team-manager", 25);
                                    maybe_reflect_energy(
                                        &project,
                                        "team-manager",
                                        "worker.spawn",
                                        Some(task.id),
                                        "worker spawn succeeded after dedicated worktree fallback",
                                    );
                                    launched.fetch_add(1, Ordering::Relaxed);
                                    spawn_times.insert(task.id, Instant::now());
                                    workers.insert(task.id, worker);
                                    continue;
                                }
                                Err(retry_err) => {
                                    eprintln!(
                                        "team scheduler: spawn teammate failed for task {} after dedicated worktree fallback: {}",
                                        task.id, retry_err
                                    );
                                }
                            },
                            Err(fallback_err) => {
                                eprintln!(
                                    "team scheduler: dedicated worktree fallback failed for task {}: {}",
                                    task.id, fallback_err
                                );
                            }
                        }
                    }
                    eprintln!(
                        "team scheduler: spawn teammate failed for task {}: {}",
                        task.id, err
                    );
                    let _ = project.tasks().update(task.id, Some("failed"), None, None);
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // 卡住检测：超过阈值的 worker 直接杀掉并重新排队 → 调度器下轮立即弹新窗口
        let stuck_ids: Vec<u64> = spawn_times
            .iter()
            .filter(|(_, t)| t.elapsed().as_secs() > WORKER_STUCK_NOTIFY_SECS)
            .map(|(id, _)| *id)
            .collect();
        for task_id in stuck_ids {
            let elapsed = spawn_times
                .remove(&task_id)
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);
            let Some(worker) = workers.remove(&task_id) else {
                continue;
            };
            let owner = worker_owner(&worker);
            let root = root_actor_id();

            let attempts = project
                .tasks()
                .increment_recovery_attempts(task_id)
                .unwrap_or(MAX_RECOVERY_ATTEMPTS + 1);

            launch_log::emit(format!(
                "[scheduler-watchdog] task={task_id} stuck for {elapsed}s, attempt={attempts}/{MAX_RECOVERY_ATTEMPTS}, killing and requeueing"
            ));

            let _ = mark_worker_stopped(&repo_root, task_id);
            let _ = project.agents().set_state(
                &owner,
                "idle",
                None,
                None,
                None,
                Some("watchdog 强制终止"),
            );
            cleanup_worker(worker, &terminal_manager, &repo_root);

            if attempts <= MAX_RECOVERY_ATTEMPTS {
                let _ = project.tasks().update(task_id, Some("pending"), None, None);
                let msg = format!(
                    "[!] worker 卡住 {elapsed}s，已自动重启（第 {attempts}/{MAX_RECOVERY_ATTEMPTS} 次）：任务 #{task_id}"
                );
                let _ = project.mailbox().send_typed(
                    "team-manager",
                    &root,
                    "task.stuck",
                    &msg,
                    Some(task_id),
                    None,
                    true,
                    None,
                );
            } else {
                let _ = project.tasks().update(task_id, Some("failed"), None, None);
                failed.fetch_add(1, Ordering::Relaxed);
                let msg = format!(
                    "[!] worker 卡住 {elapsed}s，已超过最大重启次数，任务 #{task_id} 标记为失败。请手动检查。"
                );
                let _ = project.mailbox().send_typed(
                    "team-manager",
                    &root,
                    "task.stuck",
                    &msg,
                    Some(task_id),
                    None,
                    true,
                    None,
                );
                let _ = reconcile_parent_after_child_exit(&project, task_id);
            }
        }

        running.store(workers.len(), Ordering::Relaxed);
        thread::sleep(Duration::from_millis(600));
    }

    for (_, worker) in workers {
        let owner = worker_owner(&worker);
        let _ = mark_worker_stopped(&repo_root, worker_task_id(&worker));
        if let Ok(project) = ProjectContext::new(repo_root.clone()) {
            let _ = project
                .agents()
                .set_state(&owner, "idle", None, None, None, Some("team 停止"));
            let _ = reconcile_parent_after_child_exit(&project, worker_task_id(&worker));
        }
        cleanup_worker(worker, &terminal_manager, &repo_root);
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

#[derive(Debug, Clone, Copy)]
struct PreparedTaskWorktree {
    borrowed_active: bool,
}

fn prepare_task_worktree_for_spawn(
    project: &ProjectContext,
    task_id: u64,
    owner: &str,
) -> anyhow::Result<PreparedTaskWorktree> {
    let task = project.tasks().get_record(task_id)?;
    if !project.worktrees().git_available {
        project.tasks().update(task.id, None, Some(owner), None)?;
        return Ok(PreparedTaskWorktree {
            borrowed_active: false,
        });
    }
    if !task.worktree.is_empty() {
        project
            .tasks()
            .bind_worktree(task.id, &task.worktree, owner)?;
        return Ok(PreparedTaskWorktree {
            borrowed_active: false,
        });
    }

    if let Some(active) = project.worktrees().find_active()? {
        project
            .tasks()
            .bind_worktree(task.id, &active.name, owner)?;
        return Ok(PreparedTaskWorktree {
            borrowed_active: true,
        });
    }

    let dedicated = format!("team-{}", task.id);
    create_and_bind_task_worktree(project, task.id, &dedicated, owner)?;
    Ok(PreparedTaskWorktree {
        borrowed_active: false,
    })
}

fn create_and_bind_task_worktree(
    project: &ProjectContext,
    task_id: u64,
    worktree_name: &str,
    owner: &str,
) -> anyhow::Result<()> {
    match project
        .worktrees()
        .create(worktree_name, Some(task_id), "HEAD")
    {
        Ok(_) => Ok(()),
        Err(err) => {
            let text = err.to_string();
            if text.contains("??? worktree")
                || (text.contains("worktree") && text.to_ascii_lowercase().contains("exists"))
            {
                project
                    .tasks()
                    .bind_worktree(task_id, worktree_name, owner)?;
                Ok(())
            } else {
                Err(err)
            }
        }
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
    if let Ok(project) = ProjectContext::new(repo_root.to_path_buf()) {
        let parent_task_id = project
            .tasks()
            .get_record(task_id)
            .ok()
            .and_then(|task| task.parent_task_id);
        let launch_id = request_worker_launch(
            &project,
            owner,
            role_hint,
            task_id,
            parent_task_id,
            Some("team-manager".to_string()),
        )?;
        let _ = wait_for_launch_running(&project, &launch_id, Duration::from_secs(8))?;
        return Ok(WorkerHandle::LaunchManaged {
            task_id,
            owner: owner.to_string(),
            launch_id,
        });
    }

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
        .args(
            current_tenant_id()
                .map(|tenant_id| vec!["--tenant-id".to_string(), tenant_id])
                .unwrap_or_default(),
        )
        .args(
            current_user_id()
                .map(|user_id| vec!["--user-id".to_string(), user_id])
                .unwrap_or_default(),
        )
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
    let tenant_part = current_tenant_id()
        .map(|tenant_id| format!(" --tenant-id {}", shell_quote(&tenant_id)))
        .unwrap_or_default();
    let user_part = current_user_id()
        .map(|user_id| format!(" --user-id {}", shell_quote(&user_id)))
        .unwrap_or_default();
    let command = format!(
        "{} teammate-run --repo-root {} --task-id {} --owner {} --role-hint {}{}{}",
        shell_quote(exe.to_string_lossy().as_ref()),
        shell_quote(repo_root.to_string_lossy().as_ref()),
        task_id,
        shell_quote(owner),
        shell_quote(role_hint),
        tenant_part,
        user_part
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
        env: current_tenant_id()
            .map(|tenant_id| {
                let mut env = vec![("RUSTPILOT_TENANT_ID".to_string(), tenant_id)];
                if let Some(user_id) = current_user_id() {
                    env.push(("RUSTPILOT_USER_ID".to_string(), user_id));
                }
                env
            })
            .unwrap_or_else(|| {
                current_user_id()
                    .map(|user_id| vec![("RUSTPILOT_USER_ID".to_string(), user_id)])
                    .unwrap_or_default()
            }),
    })?;
    let tenant_part = current_tenant_id()
        .map(|tenant_id| format!(" --tenant-id {}", shell_quote(&tenant_id)))
        .unwrap_or_default();
    let user_part = current_user_id()
        .map(|user_id| format!(" --user-id {}", shell_quote(&user_id)))
        .unwrap_or_default();
    let command = format!(
        "{} teammate-run --repo-root {} --task-id {} --owner {} --role-hint {}{}{}\n",
        shell_quote(exe.to_string_lossy().as_ref()),
        shell_quote(repo_root.to_string_lossy().as_ref()),
        task_id,
        shell_quote(owner),
        shell_quote(role_hint),
        tenant_part,
        user_part
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
    launch_log::emit(format!(
        "[worker] agent={} task={} role_hint={} repo={}",
        owner,
        task_id,
        role_hint,
        repo_root.display()
    ));
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
    let notification_targets = task_notification_targets(&project, &task, &owner);
    let primary_notification_target = notification_targets
        .first()
        .cloned()
        .unwrap_or_else(root_actor_id);
    if project.worktrees().git_available && task.worktree.is_empty() {
        anyhow::bail!("任务 {} 没有绑定 worktree", task_id);
    }
    launch_log::emit(format!(
        "[worker] task={} priority={} worktree={} subject={}",
        task_id, task.priority, task.worktree, task_subject
    ));

    project
        .tasks()
        .update(task_id, Some("in_progress"), Some(&owner), None)?;
    let _ = send_task_notifications(
        &project,
        &notification_targets,
        &owner,
        "task.started",
        &format!("任务 #{} 已启动：{}", task_id, task_subject),
        Some(task_id),
        Some(&trace_id),
        false,
    );

    let config = worker_role_config(&role_hint);
    let _ = project.sessions().ensure_session(
        &owner,
        Some(&format!("worker {}", task_id)),
        &format!("worker({task_id})"),
        "active",
    )?;
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
        launch_log::emit(format!(
            "[worker] task={} entering timer mode interval={}s",
            task_id, seconds
        ));
        let _ = send_task_notifications(
            &project,
            &notification_targets,
            &owner,
            "task.progress",
            &format!("定时器任务 #{} 已进入长运行，间隔={}s", task_id, seconds),
            Some(task_id),
            Some(&trace_id),
            false,
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
    let _ = project.sessions().save_messages(&owner, &messages);

    let tools = tools_for_role_and_priority(&role_hint, &task.priority);
    let progress = crate::activity::new_activity_handle();
    let heartbeat =
        WorkerHeartbeat::start(repo_root.clone(), owner.clone(), task_id, progress.clone());
    let report = AgentProgressReport {
        from: owner.clone(),
        to: primary_notification_target.clone(),
        task_id: Some(task_id),
        trace_id: Some(trace_id.clone()),
    };
    let mut clarification_cursor = 0usize;
    loop {
        launch_log::emit(format!("[worker] task={} running model/tool loop", task_id));
        let result = run_agent_loop(
            &client,
            &llm,
            &project,
            &mut messages,
            &tools,
            progress.clone(),
            Some(&report),
            None,
        )
        .await;
        match result {
            Ok(()) => {
                if let Some(question) = detect_user_clarification_need(&messages) {
                    launch_log::emit(format!(
                        "[worker] task={} blocked waiting for clarification",
                        task_id
                    ));
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
                        .update(task_id, Some("blocked"), Some(&owner), None);
                    let _ = send_task_notifications(
                        &project,
                        &notification_targets,
                        &owner,
                        "task.request_clarification",
                        &question,
                        Some(task_id),
                        Some(&trace_id),
                        true,
                    );
                    let clarification = wait_for_clarification(
                        &project,
                        &owner,
                        task_id,
                        &trace_id,
                        &mut clarification_cursor,
                    )
                    .await?;
                    launch_log::emit(format!(
                        "[worker] task={} clarification received; resuming",
                        task_id
                    ));
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
                    let _ = project.sessions().save_messages(&owner, &messages);
                    continue;
                }
                let _ = project.sessions().save_messages(&owner, &messages);
                let _ = project.sessions().update_state(
                    &owner,
                    Some(&format!("worker {}", task_id)),
                    &format!("worker({task_id})"),
                    "idle",
                );
                let _ = project
                    .tasks()
                    .update(task_id, Some("completed"), Some(&owner), None);
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
                let _ = send_task_notifications(
                    &project,
                    &notification_targets,
                    &owner,
                    "task.completed",
                    &format!("任务 #{} 已完成：{}", task_id, task_subject),
                    Some(task_id),
                    Some(&trace_id),
                    true,
                );
                let _ = send_task_notifications(
                    &project,
                    &notification_targets,
                    &owner,
                    "task.result",
                    &format!("任务 #{} 已完成：{}", task_id, task_subject),
                    Some(task_id),
                    Some(&trace_id),
                    true,
                );
                drop(heartbeat);
                launch_log::emit(format!("[worker] task={} completed", task_id));
                return Ok(());
            }
            Err(err) => {
                launch_log::emit(format!("[worker] task={} loop error: {}", task_id, err));
                let error_text = format!("{err:#}");
                let adaptation = adapt_worker_prompt_detailed(&repo_root, &error_text)
                    .unwrap_or_else(|_| crate::prompt_manager::PromptAdaptation {
                        changed: false,
                        file_path: ProjectContext::scoped_team_dir_for(&repo_root)
                            .join("worker_agent_prompt.md"),
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
                        None,
                    )
                    .await
                    .is_ok()
                    {
                        launch_log::emit(format!(
                            "[worker] task={} recovered after prompt adaptation",
                            task_id
                        ));
                        let _ = project.decisions().append(
                            &owner,
                            "task.recovered",
                            Some(task_id),
                            None,
                            "worker recovered after auto-adjusting prompt",
                            &error_text,
                        );
                        let _ = project.sessions().save_messages(&owner, &messages);
                        continue;
                    }
                }
                let _ = project.sessions().save_messages(&owner, &messages);
                let _ = project.sessions().update_state(
                    &owner,
                    Some(&format!("worker {}", task_id)),
                    &format!("worker({task_id})"),
                    "failed",
                );
                let _ = project
                    .tasks()
                    .update(task_id, Some("failed"), Some(&owner), None);
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
                // proposal 类型的任务失败时不再递归创建恢复 proposal，避免无限循环
                if !task_subject.starts_with("proposal: ") {
                    let _ = project.proposals().create(
                        &owner,
                        "task.failed",
                        Some(task_id),
                        "分析失败原因并形成修复任务",
                        "worker 执行任务失败。",
                        &["任务执行失败", "需要检查错误原因"],
                        Some("分析失败原因并决定是否重试"),
                    );
                }
                maybe_reflect_energy(
                    &project,
                    &owner,
                    "task.failed",
                    Some(task_id),
                    "worker 失败后检查预算状态。",
                );
                // 尝试自愈：让 worker 自己决定是否创建子任务/子团队来恢复
                let recovered = attempt_worker_self_recovery(
                    &client,
                    &llm,
                    &project,
                    &mut messages,
                    &tools,
                    progress.clone(),
                    &report,
                    &owner,
                    task_id,
                    &task_subject,
                    &error_text,
                )
                .await;

                if recovered {
                    // worker 创建了子任务，进入 blocked 状态等待子任务完成
                    launch_log::emit(format!(
                        "[worker] task={} self-recovery: child tasks created, entering blocked",
                        task_id
                    ));
                    let _ = project
                        .tasks()
                        .update(task_id, Some("blocked"), Some(&owner), None);
                    let _ = send_task_notifications(
                        &project,
                        &notification_targets,
                        &owner,
                        "task.progress",
                        &format!(
                            "任务 #{} 自愈中：已创建子任务/子团队，等待子任务完成后继续",
                            task_id
                        ),
                        Some(task_id),
                        Some(&trace_id),
                        false,
                    );
                    drop(heartbeat);
                    return Ok(());
                }

                // 无法自愈，才真正标记为失败
                let _ = send_task_notifications(
                    &project,
                    &notification_targets,
                    &owner,
                    "task.failed",
                    &format!("任务 #{} 失败：{}", task_id, err),
                    Some(task_id),
                    Some(&trace_id),
                    true,
                );
                drop(heartbeat);
                launch_log::emit(format!("[worker] task={} failed", task_id));
                return Err(err);
            }
        }
    }
}

/// 自愈最大尝试次数。超过此限制的任务不再自愈，直接标记为 failed。
const MAX_RECOVERY_ATTEMPTS: u32 = 2;

/// 当 worker 的 agent loop 失败后，向 worker 注入一条恢复提示，
/// 让 worker 自己决定是否创建子任务或子团队来解决问题。
/// 返回 true 表示 worker 创建了恢复子任务（进入 blocked 等待）；
/// 返回 false 表示 worker 放弃，应标记为 failed。
async fn attempt_worker_self_recovery(
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    tools: &[crate::openai_compat::Tool],
    progress: crate::activity::ActivityHandle,
    report: &AgentProgressReport,
    _owner: &str,
    task_id: u64,
    task_subject: &str,
    error_text: &str,
) -> bool {
    // 超过次数上限，不再自愈
    let attempts = project
        .tasks()
        .increment_recovery_attempts(task_id)
        .unwrap_or(MAX_RECOVERY_ATTEMPTS + 1);
    if attempts > MAX_RECOVERY_ATTEMPTS {
        launch_log::emit(format!(
            "[worker] task={} recovery_attempts={} exceeded limit={}, giving up",
            task_id, attempts, MAX_RECOVERY_ATTEMPTS
        ));
        return false;
    }
    launch_log::emit(format!(
        "[worker] task={} starting self-recovery attempt {}/{}",
        task_id, attempts, MAX_RECOVERY_ATTEMPTS
    ));

    let child_count_before = project
        .tasks()
        .active_child_count(Some(task_id))
        .unwrap_or(0);

    let recovery_msg = format!(
        "你的任务执行遇到了问题。\n\
         错误信息：{}\n\
         任务：{}\n\
         \n\
         请分析失败原因，然后选择以下恢复路径：\n\
         方案 A — 单子任务：如果问题可以由一个子 agent 独立解决，\
         用 task_create 创建一个子任务，描述清楚目标、方法和成功条件。\n\
         方案 B — 子团队：如果问题需要多个角色配合（如规划+执行+验证），\
         先创建一个规划子任务，由它分析需要哪些功能 agent 并逐一创建；\
         各 agent 之间通过 team_send 协调。\n\
         \n\
         如果你判断问题根本无法通过子任务解决（例如缺少用户凭据、外部系统无法访问），\
         则直接说明原因，不要创建子任务。",
        error_text, task_subject
    );
    messages.push(Message {
        role: "user".to_string(),
        content: Some(recovery_msg),
        tool_call_id: None,
        tool_calls: None,
    });

    let recovery_ok = crate::agent::run_agent_loop(
        client,
        llm,
        project,
        messages,
        tools,
        progress,
        Some(report),
        None,
    )
    .await
    .is_ok();

    if !recovery_ok {
        return false;
    }

    // 检查是否创建了新的子任务
    let child_count_after = project
        .tasks()
        .active_child_count(Some(task_id))
        .unwrap_or(0);
    child_count_after > child_count_before
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
    launch_log::emit(format!(
        "[worker] task={} timer started agent={} interval={}s",
        task_id, owner, seconds
    ));
    launch_log::emit(format!(
        "[timer-agent] task#{} owner={} started, interval={}s",
        task_id, owner, seconds
    ));
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        launch_log::emit(format!(
            "[timer-agent] task#{} owner={} ts_unix={} interval={}s",
            task_id, owner, now, seconds
        ));
        thread::sleep(Duration::from_secs(seconds));
    }
}

fn task_notification_targets(
    project: &ProjectContext,
    task: &TaskRecord,
    owner: &str,
) -> Vec<String> {
    let mut targets = Vec::new();
    if let Some(parent_task_id) = task.parent_task_id
        && let Ok(parent_task) = project.tasks().get_record(parent_task_id)
        && !parent_task.owner.trim().is_empty()
        && parent_task.owner != owner
    {
        targets.push(parent_task.owner);
    }

    let root_actor = root_actor_id();
    if root_actor != owner && !targets.iter().any(|item| item == &root_actor) {
        targets.push(root_actor);
    }
    targets
}

fn send_task_notifications(
    project: &ProjectContext,
    recipients: &[String],
    from: &str,
    msg_type: &str,
    message: &str,
    task_id: Option<u64>,
    trace_id: Option<&str>,
    requires_ack: bool,
) -> anyhow::Result<()> {
    for recipient in recipients {
        let _ = project.mailbox().send_typed(
            from,
            recipient,
            msg_type,
            message,
            task_id,
            trace_id,
            requires_ack,
            None,
        )?;
    }
    Ok(())
}

struct WorkerHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerHeartbeat {
    fn start(
        repo_root: PathBuf,
        owner: String,
        task_id: u64,
        progress: crate::activity::ActivityHandle,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = thread::spawn(move || {
            let started = Instant::now();
            let mut mailbox_cursor = ProjectContext::new(repo_root.clone())
                .ok()
                .and_then(|project| project.mailbox().pending_count(&owner).ok())
                .unwrap_or(0);
            while !stop_flag.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(5));
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                launch_log::emit(format!(
                    "[worker-heartbeat] agent={} task={} alive_for={:.1}s\n{}",
                    owner,
                    task_id,
                    started.elapsed().as_secs_f64(),
                    crate::activity::render_activity(&progress)
                ));
                emit_parent_mailbox_updates(&repo_root, &owner, &mut mailbox_cursor);
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for WorkerHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
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

#[derive(Debug, Deserialize)]
struct WorkerParentMailPoll {
    next_cursor: usize,
    items: Vec<WorkerParentMailItem>,
}

#[derive(Debug, Deserialize)]
struct WorkerParentMailItem {
    msg_id: String,
    msg_type: String,
    from: String,
    message: String,
    task_id: Option<u64>,
    #[serde(default)]
    requires_ack: bool,
}

fn emit_parent_mailbox_updates(repo_root: &Path, owner: &str, cursor: &mut usize) {
    let Ok(project) = ProjectContext::new(repo_root.to_path_buf()) else {
        return;
    };
    let Ok(raw) = project.mailbox().poll(owner, *cursor, 20) else {
        return;
    };
    let Ok(polled) = serde_json::from_str::<WorkerParentMailPoll>(&raw) else {
        return;
    };
    *cursor = polled.next_cursor;
    for item in polled.items {
        if !matches!(
            item.msg_type.as_str(),
            "task.started"
                | "task.progress"
                | "task.completed"
                | "task.result"
                | "task.failed"
                | "task.request_clarification"
        ) {
            continue;
        }
        launch_log::emit(format!(
            "[parent-mail][task={}][type={}][from={}] {}",
            item.task_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            item.msg_type,
            item.from,
            item.message
        ));
        if item.requires_ack {
            let _ = project
                .mailbox()
                .ack(owner, &item.msg_id, "received by parent worker");
        }
    }
}

pub(crate) fn reconcile_parent_after_child_exit(
    project: &ProjectContext,
    child_task_id: u64,
) -> anyhow::Result<()> {
    let child = project.tasks().get_record(child_task_id)?;
    let Some(parent_task_id) = child.parent_task_id else {
        return Ok(());
    };

    let parent = project.tasks().get_record(parent_task_id)?;
    if matches!(
        parent.status.as_str(),
        "completed" | "failed" | "cancelled" | "paused"
    ) {
        return Ok(());
    }

    let children: Vec<TaskRecord> = project
        .tasks()
        .list_records()?
        .into_iter()
        .filter(|task| task.parent_task_id == Some(parent_task_id))
        .collect();
    if children.is_empty() {
        return Ok(());
    }

    let has_blocked = children.iter().any(|task| task.status == "blocked");
    let has_failed = children.iter().any(|task| task.status == "failed");
    let has_active = children
        .iter()
        .any(|task| matches!(task.status.as_str(), "pending" | "in_progress" | "paused"));
    let all_children_done_and_failed = !has_active && !has_blocked && has_failed;

    let next_status = if all_children_done_and_failed {
        // 所有子任务都失败了：检查 parent 的恢复次数，决定是重新调度还是放弃
        let attempts = project
            .tasks()
            .increment_recovery_attempts(parent_task_id)
            .unwrap_or(MAX_RECOVERY_ATTEMPTS + 1);

        if attempts <= MAX_RECOVERY_ATTEMPTS {
            // 还有恢复机会：把失败摘要追加进 parent 任务描述，然后重置为 pending
            // scheduler 会重新分配一个新 worker，新 worker 看到失败上下文后换思路
            let failed_summary = children
                .iter()
                .filter(|t| t.status == "failed")
                .map(|t| format!("  - #{} {}: failed", t.id, t.subject))
                .collect::<Vec<_>>()
                .join("\n");
            let recovery_note = format!(
                "\n\n[CHILD_FAILURE_RECOVERY attempt={}/{}]\n\
                 以下子任务全部失败，请分析失败原因并采用不同方案重试：\n{}",
                attempts, MAX_RECOVERY_ATTEMPTS, failed_summary
            );
            let _ = project
                .tasks()
                .append_user_reply(parent_task_id, &recovery_note, "pending");
            launch_log::emit(format!(
                "[reconcile] parent task={} all children failed, re-queuing for recovery attempt {}/{}",
                parent_task_id, attempts, MAX_RECOVERY_ATTEMPTS
            ));
            "pending"
        } else {
            // 恢复次数耗尽，真正失败
            launch_log::emit(format!(
                "[reconcile] parent task={} all children failed and recovery attempts exhausted, marking failed",
                parent_task_id
            ));
            "failed"
        }
    } else if has_failed || has_blocked {
        "blocked"
    } else if has_active {
        "in_progress"
    } else {
        "pending"
    };
    let _ = project
        .tasks()
        .update(parent_task_id, Some(next_status), None, None)?;

    if !parent.owner.trim().is_empty() {
        let existing_state = project.agents().state(&parent.owner)?;
        let (agent_status, note) = if has_failed || has_blocked {
            ("blocked", "子任务已结束，但存在失败或阻塞，等待父任务处理")
        } else if has_active {
            ("running", "子任务仍在运行，父任务保持调度中")
        } else {
            ("idle", "子任务已回收，等待父任务继续")
        };
        let channel = existing_state
            .as_ref()
            .and_then(|state| state.channel.as_deref());
        let target = existing_state
            .as_ref()
            .and_then(|state| state.target.as_deref());
        let _ = project.agents().set_state(
            &parent.owner,
            agent_status,
            Some(parent_task_id),
            channel,
            target,
            Some(note),
        );
    }

    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn cleanup_worker(worker: WorkerHandle, terminal_manager: &TerminalManager, repo_root: &Path) {
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
        WorkerHandle::LaunchManaged { launch_id, .. } => {
            if let Ok(project) = ProjectContext::new(repo_root.to_path_buf()) {
                let _ = stop_launch(&project, &launch_id);
            }
        }
    }
}

fn worker_task_id(worker: &WorkerHandle) -> u64 {
    match worker {
        WorkerHandle::Child { task_id, .. } => *task_id,
        WorkerHandle::TmuxWindow { task_id, .. } => *task_id,
        WorkerHandle::TerminalSession { task_id, .. } => *task_id,
        WorkerHandle::LaunchManaged { task_id, .. } => *task_id,
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
        WorkerHandle::LaunchManaged {
            task_id,
            owner,
            launch_id,
        } => {
            let launch = ProjectContext::new(repo_root.to_path_buf())
                .ok()
                .and_then(|project| project.launches().get(launch_id).ok().flatten());
            WorkerEndpoint {
                task_id: *task_id,
                owner: owner.clone(),
                channel: launch
                    .as_ref()
                    .map(|item| item.channel.clone())
                    .unwrap_or_else(|| "window".to_string()),
                target: launch
                    .as_ref()
                    .map(|item| {
                        if item.target.is_empty() {
                            format!("launch:{launch_id}")
                        } else {
                            item.target.clone()
                        }
                    })
                    .unwrap_or_else(|| format!("launch:{launch_id}")),
                status: "running".to_string(),
            }
        }
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
        WorkerHandle::LaunchManaged {
            task_id, launch_id, ..
        } => {
            let launch = project.launches().get(launch_id)?.unwrap_or(LaunchRecord {
                launch_id: launch_id.clone(),
                agent_id: owner.to_string(),
                role: role_hint.to_string(),
                kind: "worker".to_string(),
                owner: owner.to_string(),
                task_id: Some(*task_id),
                parent_task_id: None,
                parent_agent_id: None,
                max_parallel: None,
                tenant_id: current_tenant_id(),
                user_id: current_user_id(),
                status: "running".to_string(),
                pid: None,
                process_started_at: None,
                window_title: String::new(),
                log_path: String::new(),
                channel: "window".to_string(),
                target: format!("launch:{launch_id}"),
                error: None,
                exit_code: None,
                created_at: 0.0,
                updated_at: 0.0,
                started_at: None,
                stopped_at: None,
            });
            let target = if launch.target.is_empty() {
                format!("launch:{launch_id}")
            } else {
                launch.target.clone()
            };
            project.agents().set_state(
                owner,
                "running",
                Some(*task_id),
                Some(&launch.channel),
                Some(&target),
                Some("worker 窗口运行中"),
            )?
        }
    }
    Ok(())
}

pub(crate) fn mark_worker_stopped(repo_root: &Path, task_id: u64) -> anyhow::Result<()> {
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

pub fn list_worker_endpoints(repo_root: &Path) -> anyhow::Result<Vec<WorkerEndpoint>> {
    load_worker_endpoints(repo_root)
}

pub fn send_input_to_worker(repo_root: &Path, task_id: u64, input: &str) -> anyhow::Result<String> {
    let endpoint = get_worker_endpoint(repo_root, task_id)?
        .ok_or_else(|| anyhow::anyhow!("未找到 task {} 的 worker 映射", task_id))?;
    if endpoint.status != "running" {
        anyhow::bail!("task {} 的 worker 已停止", task_id);
    }

    match endpoint.channel.as_str() {
        "terminal" => {
            let manager =
                TerminalManager::with_log_dir(ProjectContext::scoped_team_dir_for(repo_root).join("sessions"));
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
    ProjectContext::scoped_team_dir_for(repo_root).join("agents.json")
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
        WorkerHandle::LaunchManaged { owner, .. } => owner.clone(),
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
            "task_create",
            "task_list",
            "task_get",
            "delegate_long_running",
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
            "task_create",
            "task_list",
            "task_get",
            "task_update",
            "delegate_long_running",
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
            "task_create",
            "task_list",
            "task_get",
            "task_update",
            "delegate_long_running",
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
            "task_create",
            "task_list",
            "task_get",
            "task_update",
            "delegate_long_running",
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
            "task_create",
            "task_list",
            "task_get",
            "task_update",
            "delegate_long_running",
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

#[cfg(test)]
mod tests {
    use super::{
        prepare_task_worktree_for_spawn, reconcile_parent_after_child_exit,
        send_task_notifications, task_notification_targets,
    };
    use crate::project_tools::{ProjectContext, TaskCreateOptions, TaskRecord};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!("{}_{}_{}", name, std::process::id(), now_nanos());
            let path = std::env::temp_dir().join("tests").join(unique);
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

    fn now_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    }

    fn project_context(repo_root: &Path) -> ProjectContext {
        ProjectContext::new(repo_root.to_path_buf()).expect("project context")
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

    #[test]
    fn reconcile_parent_after_child_completion_sets_parent_pending_and_idle() {
        let temp = TestDir::new("parent_reconcile_completed");
        let project = project_context(temp.path());

        let parent_raw = project
            .tasks()
            .create_detailed(
                "parent task",
                "coordinate child work",
                TaskCreateOptions::default(),
            )
            .expect("create parent");
        let parent: TaskRecord = serde_json::from_str(&parent_raw).expect("parse parent");
        project
            .tasks()
            .update(parent.id, Some("in_progress"), Some("root-node"), None)
            .expect("update parent");
        project
            .agents()
            .set_state(
                "root-node",
                "running",
                Some(parent.id),
                Some("scheduler"),
                Some("root-loop"),
                Some("waiting"),
            )
            .expect("set parent state");

        let child_raw = project
            .tasks()
            .create_detailed(
                "child task",
                "do work",
                TaskCreateOptions {
                    parent_task_id: Some(parent.id),
                    depth: Some(1),
                    ..TaskCreateOptions::default()
                },
            )
            .expect("create child");
        let child: TaskRecord = serde_json::from_str(&child_raw).expect("parse child");
        project
            .tasks()
            .update(child.id, Some("completed"), Some("teammate-1"), None)
            .expect("complete child");

        reconcile_parent_after_child_exit(&project, child.id).expect("reconcile");

        let parent_after = project.tasks().get_record(parent.id).expect("parent after");
        assert_eq!(parent_after.status, "pending");

        let parent_state = project
            .agents()
            .state("root-node")
            .expect("agent state")
            .expect("parent state exists");
        assert_eq!(parent_state.status, "idle");
        assert_eq!(parent_state.current_task_id, Some(parent.id));
        assert_eq!(parent_state.channel.as_deref(), Some("scheduler"));
        assert_eq!(parent_state.target.as_deref(), Some("root-loop"));
        assert_eq!(
            parent_state.note.as_deref(),
            Some("子任务已回收，等待父任务继续")
        );
    }

    #[test]
    fn reconcile_parent_after_child_failure_sets_parent_blocked() {
        let temp = TestDir::new("parent_reconcile_failed");
        let project = project_context(temp.path());

        let parent_raw = project
            .tasks()
            .create_detailed(
                "parent task",
                "coordinate child work",
                TaskCreateOptions::default(),
            )
            .expect("create parent");
        let parent: TaskRecord = serde_json::from_str(&parent_raw).expect("parse parent");
        project
            .tasks()
            .update(parent.id, Some("in_progress"), Some("root-node"), None)
            .expect("update parent");
        project
            .agents()
            .set_state(
                "root-node",
                "running",
                Some(parent.id),
                Some("scheduler"),
                Some("root-loop"),
                Some("waiting"),
            )
            .expect("set parent state");

        let child_raw = project
            .tasks()
            .create_detailed(
                "child task",
                "do work",
                TaskCreateOptions {
                    parent_task_id: Some(parent.id),
                    depth: Some(1),
                    ..TaskCreateOptions::default()
                },
            )
            .expect("create child");
        let child: TaskRecord = serde_json::from_str(&child_raw).expect("parse child");
        project
            .tasks()
            .update(child.id, Some("failed"), Some("teammate-1"), None)
            .expect("fail child");

        reconcile_parent_after_child_exit(&project, child.id).expect("reconcile");

        let parent_after = project.tasks().get_record(parent.id).expect("parent after");
        assert_eq!(parent_after.status, "pending");

        let parent_state = project
            .agents()
            .state("root-node")
            .expect("agent state")
            .expect("parent state exists");
        assert_eq!(parent_state.status, "blocked");
        assert_eq!(
            parent_state.note.as_deref(),
            Some("子任务已结束，但存在失败或阻塞，等待父任务处理")
        );
    }

    #[test]
    fn prepare_task_worktree_prefers_active_worktree() {
        let temp = TestDir::new("prepare_task_worktree_prefers_active");
        init_git_repo(temp.path());
        let project = project_context(temp.path());

        let active_task_raw = project
            .tasks()
            .create_detailed(
                "active task",
                "existing worktree owner",
                TaskCreateOptions::default(),
            )
            .expect("create active task");
        let active_task: TaskRecord =
            serde_json::from_str(&active_task_raw).expect("parse active task");
        let task_raw = project
            .tasks()
            .create_detailed(
                "new task",
                "should reuse current worktree first",
                TaskCreateOptions::default(),
            )
            .expect("create task");
        let task: TaskRecord = serde_json::from_str(&task_raw).expect("parse task");

        let index_path = temp.path().join(".worktrees").join("index.json");
        std::fs::create_dir_all(index_path.parent().expect("worktrees dir"))
            .expect("create worktrees dir");
        std::fs::write(
            &index_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "worktrees": [{
                    "name": "team-1",
                    "path": temp.path().join(".worktrees").join("team-1").display().to_string(),
                    "branch": "wt/team-1",
                    "task_id": active_task.id,
                    "status": "active",
                    "created_at": 1.0
                }]
            }))
            .expect("serialize index"),
        )
        .expect("write index");

        let prepared = prepare_task_worktree_for_spawn(&project, task.id, "teammate-2")
            .expect("prepare task worktree");
        assert!(prepared.borrowed_active);

        let task_after = project.tasks().get_record(task.id).expect("task after");
        assert_eq!(task_after.worktree, "team-1");
        assert_eq!(task_after.owner, "teammate-2");
    }

    #[test]
    fn prepare_task_worktree_skips_creation_outside_git_repo() {
        let temp = TestDir::new("prepare_task_worktree_without_git");
        let project = project_context(temp.path());

        let task_raw = project
            .tasks()
            .create_detailed(
                "new task",
                "should still spawn worker without a git repo",
                TaskCreateOptions::default(),
            )
            .expect("create task");
        let task: TaskRecord = serde_json::from_str(&task_raw).expect("parse task");

        assert!(!project.worktrees().git_available);

        let prepared = prepare_task_worktree_for_spawn(&project, task.id, "teammate-1")
            .expect("prepare task worktree");
        assert!(!prepared.borrowed_active);

        let task_after = project.tasks().get_record(task.id).expect("task after");
        assert!(task_after.worktree.is_empty());
        assert_eq!(task_after.owner, "teammate-1");
    }

    #[test]
    fn child_completion_notifies_parent_owner_and_root_actor() {
        let temp = TestDir::new("child_completion_notifies_parent");
        let project = project_context(temp.path());
        unsafe {
            std::env::set_var("RUSTPILOT_ROOT_AGENT_ID", "root-node");
        }

        let parent_raw = project
            .tasks()
            .create_detailed(
                "parent task",
                "coordinate child work",
                TaskCreateOptions::default(),
            )
            .expect("create parent");
        let parent: TaskRecord = serde_json::from_str(&parent_raw).expect("parse parent");
        project
            .tasks()
            .update(
                parent.id,
                Some("in_progress"),
                Some("teammate-parent"),
                None,
            )
            .expect("claim parent");

        let child_raw = project
            .tasks()
            .create_detailed(
                "child task",
                "complete work",
                TaskCreateOptions {
                    parent_task_id: Some(parent.id),
                    depth: Some(1),
                    ..TaskCreateOptions::default()
                },
            )
            .expect("create child");
        let child: TaskRecord = serde_json::from_str(&child_raw).expect("parse child");
        let targets = task_notification_targets(&project, &child, "teammate-child");
        assert_eq!(
            targets,
            vec!["teammate-parent".to_string(), "root-node".to_string()]
        );

        send_task_notifications(
            &project,
            &targets,
            "teammate-child",
            "task.completed",
            "任务 #2 已完成：child task",
            Some(child.id),
            Some("task-2"),
            true,
        )
        .expect("send notifications");

        let parent_inbox = project
            .mailbox()
            .poll("teammate-parent", 0, 20)
            .expect("poll parent inbox");
        assert!(parent_inbox.contains("\"msg_type\": \"task.completed\""));
        assert!(parent_inbox.contains("任务 #2 已完成"));

        let root_inbox = project
            .mailbox()
            .poll("root-node", 0, 20)
            .expect("poll root inbox");
        assert!(root_inbox.contains("\"msg_type\": \"task.completed\""));
        assert!(root_inbox.contains("任务 #2 已完成"));

        unsafe {
            std::env::remove_var("RUSTPILOT_ROOT_AGENT_ID");
        }
    }
}
