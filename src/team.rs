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
use crate::openai_compat::Message;
use crate::project_tools::{ProjectContext, TaskRecord};
use crate::runtime_env::llm_timeout_secs;
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
                            let _ = project.tasks().update(*task_id, Some("completed"), None);
                            completed.fetch_add(1, Ordering::Relaxed);
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
                cleanup_worker(worker, &terminal_manager);
            }
        }

        while workers.len() < max_parallel {
            let Some(task) = (match project.tasks().claim_next_pending("team-manager") {
                Ok(task) => task,
                Err(err) => {
                    eprintln!("team scheduler: claim task failed: {err}");
                    None
                }
            }) else {
                break;
            };

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
                &spawn_mode,
                &terminal_manager,
            ) {
                Ok(worker) => {
                    let _ = register_worker_endpoint(&repo_root, &owner, &worker);
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
        cleanup_worker(worker, &terminal_manager);
    }
}

fn spawn_teammate_process(
    repo_root: &Path,
    task_id: u64,
    owner: &str,
    spawn_mode: &SpawnMode,
    terminal_manager: &TerminalManager,
) -> anyhow::Result<WorkerHandle> {
    if matches!(spawn_mode, SpawnMode::Tmux) {
        return spawn_teammate_in_tmux(repo_root, task_id, owner);
    }
    if matches!(spawn_mode, SpawnMode::Terminal) {
        return spawn_teammate_in_terminal(repo_root, task_id, owner, terminal_manager);
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
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(WorkerHandle::Child { task_id, child })
}

fn spawn_teammate_in_tmux(
    repo_root: &Path,
    task_id: u64,
    owner: &str,
) -> anyhow::Result<WorkerHandle> {
    ensure_tmux_session("rustpilot-team")?;
    let exe = std::env::current_exe()?;
    let window_name = format!("rustpilot-team:teammate-{}", task_id);
    let pane_name = format!("teammate-{}", task_id);
    let command = format!(
        "{} teammate-run --repo-root {} --task-id {} --owner {}",
        shell_quote(exe.to_string_lossy().as_ref()),
        shell_quote(repo_root.to_string_lossy().as_ref()),
        task_id,
        shell_quote(owner)
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
    terminal_manager: &TerminalManager,
) -> anyhow::Result<WorkerHandle> {
    let exe = std::env::current_exe()?;
    let info = terminal_manager.create(TerminalCreateRequest {
        cwd: Some(repo_root.to_path_buf()),
        shell: None,
        env: Vec::new(),
    })?;
    let command = format!(
        "{} teammate-run --repo-root {} --task-id {} --owner {}\n",
        shell_quote(exe.to_string_lossy().as_ref()),
        shell_quote(repo_root.to_string_lossy().as_ref()),
        task_id,
        shell_quote(owner)
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
) -> anyhow::Result<()> {
    dotenvy::from_path(repo_root.join(".env")).ok();
    let llm = LlmConfig::from_env()?;
    let project = ProjectContext::new(repo_root.clone())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(llm_timeout_secs()))
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

    let system_prompt = format!(
        "你是团队成员 {}。只完成当前任务并汇报结果；任务是控制面，worktree 是执行面。需要协作时使用 team_send 和 team_inbox。仓库: {}",
        owner,
        repo_root.display()
    );
    let task_prompt = if task.description.trim().is_empty() {
        task_subject.clone()
    } else {
        format!("{}\n\n{}", task_subject, task.description)
    };

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

    let tools = tool_definitions();
    let progress = crate::activity::new_activity_handle();
    let report = AgentProgressReport {
        from: owner.clone(),
        to: "lead".to_string(),
        task_id: Some(task_id),
        trace_id: Some(trace_id.clone()),
    };
    let result = run_agent_loop(
        &client,
        &llm,
        &project,
        &mut messages,
        &tools,
        progress,
        Some(&report),
    )
    .await;
    match result {
        Ok(()) => {
            let _ = project
                .tasks()
                .update(task_id, Some("completed"), Some(&owner));
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
            Ok(())
        }
        Err(err) => {
            let _ = project
                .tasks()
                .update(task_id, Some("failed"), Some(&owner));
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
            Err(err)
        }
    }
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
