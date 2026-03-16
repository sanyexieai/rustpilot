use crate::activity::new_activity_handle;
use crate::app_commands::{CliRuntime, LoopDirective, process_cli_action};
use crate::app_support::{
    InteractionMode, collect_lead_mailbox_events, current_agent_id, load_repo_env,
    parse_resident_args, parse_teammate_args, resolve_ui_port, root_actor_id,
};
use crate::cli::handle_cli_command;
use crate::config::{LlmConfig, default_llm_user_agent};
use crate::openai_compat::Message;
use crate::project_tools::ProjectContext;
use crate::prompt_manager::render_root_system_prompt;
use crate::resident_agents::{AgentSupervisor, run_resident_agent};
use crate::runtime_env::{
    detect_repo_root, ensure_env_guidance, llm_timeout_secs_for_provider,
    prompt_and_store_llm_api_key,
};
use crate::skills::SkillRegistry;
use crate::team::run_teammate_once;
use crate::ui_server::spawn_ui_server;
use crate::wire::{WireEnvelope, WireEvent, WireFrame, WireRequest, WireResponse};
use crate::wire_exec::{WireRuntime, execute_wire_request};
use crossterm::cursor::MoveToColumn;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::Print;
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use std::io::{self, BufRead, BufReader, BufWriter, Stdout, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AUTO_TEAM_MAX_PARALLEL: usize = 2;
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(40);
const MAILBOX_POLL_INTERVAL: Duration = Duration::from_millis(400);
const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

struct InteractiveConsole {
    stdout: Stdout,
    prompt: String,
    buffer: String,
    raw_enabled: bool,
    dirty: bool,
}

impl InteractiveConsole {
    fn new(prompt: String) -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self {
            stdout: io::stdout(),
            prompt,
            buffer: String::new(),
            raw_enabled: true,
            dirty: true,
        })
    }

    fn set_prompt(&mut self, prompt: String) {
        if self.prompt != prompt {
            self.prompt = prompt;
            self.dirty = true;
        }
    }

    fn render_prompt(&mut self) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        queue!(
            self.stdout,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            Print(format!("{}> {}", self.prompt, self.buffer))
        )?;
        self.stdout.flush()?;
        self.dirty = false;
        Ok(())
    }

    fn print_lines(&mut self, lines: &[String]) -> anyhow::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        queue!(self.stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        for line in lines {
            queue!(self.stdout, Print(line), Print("\r\n"))?;
        }
        self.dirty = true;
        self.render_prompt()
    }

    fn handle_key(&mut self, key: KeyEvent) -> anyhow::Result<Option<String>> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                anyhow::bail!("interrupted");
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(String::new()));
            }
            KeyCode::Enter => {
                let line = self.buffer.clone();
                queue!(
                    self.stdout,
                    MoveToColumn(0),
                    Clear(ClearType::CurrentLine),
                    Print(format!("{}> {}", self.prompt, self.buffer)),
                    Print("\r\n")
                )?;
                self.stdout.flush()?;
                self.buffer.clear();
                self.dirty = true;
                return Ok(Some(line));
            }
            KeyCode::Backspace => {
                if self.buffer.pop().is_some() {
                    self.dirty = true;
                }
            }
            KeyCode::Char(ch) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.buffer.push(ch);
                    self.dirty = true;
                }
            }
            _ => {}
        }
        self.render_prompt()?;
        Ok(None)
    }

    fn suspend_raw(&mut self) -> anyhow::Result<()> {
        if self.raw_enabled {
            terminal::disable_raw_mode()?;
            self.raw_enabled = false;
        }
        Ok(())
    }

}

impl Drop for InteractiveConsole {
    fn drop(&mut self) {
        let _ = self.suspend_raw();
        let _ = execute!(self.stdout, MoveToColumn(0), Clear(ClearType::CurrentLine));
    }
}

pub async fn run() -> anyhow::Result<()> {
    let root_actor = root_actor_id();
    unsafe {
        std::env::set_var("RUSTPILOT_ROOT_AGENT_ID", root_actor.clone());
        std::env::set_var("RUSTPILOT_AGENT_ID", root_actor.clone());
    }
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|value| value == "teammate-run") {
        let teammate = parse_teammate_args(&args[2..])?;
        unsafe {
            std::env::set_var("RUSTPILOT_AGENT_ID", teammate.owner.clone());
            std::env::set_var("RUSTPILOT_TASK_ID", teammate.task_id.to_string());
        }
        load_repo_env(&teammate.repo_root);
        return run_teammate_once(
            teammate.repo_root,
            teammate.task_id,
            teammate.owner,
            teammate.role_hint,
        )
        .await;
    }
    if args
        .get(1)
        .is_some_and(|value| value == "resident-agent-run")
    {
        let resident = parse_resident_args(&args[2..], AUTO_TEAM_MAX_PARALLEL)?;
        load_repo_env(&resident.repo_root);
        return run_resident_agent(
            resident.repo_root,
            resident.agent_id,
            resident.role,
            resident.max_parallel,
        );
    }
    if args
        .get(1)
        .is_some_and(|value| value == "root-runtime-run")
    {
        let (repo_root, parent_pid) = parse_root_runtime_args(&args[2..])?;
        load_repo_env(&repo_root);
        return run_root_runtime(repo_root, parent_pid).await;
    }

    run_root_console().await
}

async fn run_root_console() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let env_update = ensure_env_guidance(&cwd)?;
    dotenvy::from_path(cwd.join(".env")).ok();

    if env_update.created {
        println!("created .env template at {}", cwd.display());
    } else if !env_update.added_keys.is_empty() {
        println!(
            "updated .env with missing keys: {}",
            env_update.added_keys.join(", ")
        );
    }

    let repo_root = detect_repo_root(&cwd).unwrap_or_else(|| cwd.clone());
    unsafe {
        std::env::set_var("RUSTPILOT_REPO_ROOT", repo_root.display().to_string());
    }

    match LlmConfig::from_repo_root(&repo_root) {
        Ok(_) => {}
        Err(err)
            if err.to_string().contains("LLM_API_KEY is required")
                || err.to_string().contains("No API key found for provider") =>
        {
            println!("no valid LLM API key detected");
            println!(
                "you can store it into {}/.env from the prompt below",
                cwd.display()
            );
            if !prompt_and_store_llm_api_key(&cwd)? {
                println!("cancelled");
                return Ok(());
            }
            dotenvy::from_path_override(cwd.join(".env")).ok();
            let _ = LlmConfig::from_repo_root(&repo_root)?;
        }
        Err(err) => return Err(err),
    }

    let project = ProjectContext::new(repo_root.clone())?;
    let ui_port = resolve_ui_port(&project);
    let root_actor = root_actor_id();
    let default_session =
        project
            .sessions()
            .ensure_session("cli-main", Some("primary"), &root_actor, "active")?;

    println!("repo root: {}", repo_root.display());
    println!("focus: root");
    println!("session: {}", default_session.session_id);
    println!("ui: http://127.0.0.1:{ui_port}");
    if !project.worktrees().git_available {
        println!("warning: current directory is not a git repository");
    }

    // 启动时确认未完成的任务（在进入 raw 模式前，使用普通 stdin/stdout）
    confirm_unfinished_tasks(&project)?;

    let (mut child, mut child_stdin, frame_rx) = spawn_root_runtime_process(&repo_root)?;
    let mut console = InteractiveConsole::new("root".to_string())?;
    console.render_prompt()?;

    loop {
        let mut lines = Vec::new();
        let mut should_exit = false;
        while let Ok(frame) = frame_rx.try_recv() {
            match frame {
                Ok(frame) => {
                    apply_frame_to_console(&frame, &mut console, &mut lines);
                    if is_exit_ack(&frame) {
                        should_exit = true;
                    }
                }
                Err(err) => lines.push(format!("error: runtime stream {}", err)),
            }
        }
        console.print_lines(&lines)?;

        if should_exit {
            break;
        }
        if child.try_wait()?.is_some() {
            break;
        }

        if !event::poll(INPUT_POLL_INTERVAL)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        let Some(input) = console.handle_key(key)? else {
            continue;
        };
        if input.is_empty() {
            break;
        }
        let trimmed = input.trim();
        if trimmed.is_empty() {
            console.render_prompt()?;
            continue;
        }
        write_wire_request(
            &mut child_stdin,
            &WireRequest::ConsoleInput {
                input: trimmed.to_string(),
            },
        )?;
    }

    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

async fn run_root_runtime(repo_root: std::path::PathBuf, parent_pid: Option<u32>) -> anyhow::Result<()> {
    unsafe {
        std::env::set_var("RUSTPILOT_REPO_ROOT", repo_root.display().to_string());
    }
    let llm = LlmConfig::from_repo_root(&repo_root)?;
    let client = reqwest::Client::builder()
        .user_agent(default_llm_user_agent())
        .timeout(Duration::from_secs(llm_timeout_secs_for_provider(
            &llm.provider,
        )))
        .build()?;

    let project = ProjectContext::new(repo_root.clone())?;
    let root_actor = root_actor_id();
    project.agents().ensure_profile(
        &root_actor,
        "scheduler",
        "Receive user input, maintain the main dialogue, and coordinate workers.",
        &[
            "accept user requests",
            "coordinate tasks",
            "summarize progress",
        ],
        &["do not bypass tasks or mailbox routing"],
    )?;
    project.agents().set_state(
        &root_actor,
        "idle",
        None,
        Some("cli"),
        Some("main"),
        Some("primary console"),
    )?;
    project
        .budgets()
        .ensure_ledger(&root_actor, 120_000, 30_000, 12_000)?;

    let mut supervisor =
        AgentSupervisor::start_defaults(repo_root.clone(), AUTO_TEAM_MAX_PARALLEL)?;
    let mut skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
    let progress = new_activity_handle();
    let mut lead_cursor = 0usize;
    let mut interaction_mode = InteractionMode::Lead;
    let system_prompt = render_root_system_prompt(&repo_root)?;
    let ui_port = resolve_ui_port(&project);
    let _ui_server = start_main_ui_server(repo_root.clone(), ui_port);
    let default_session =
        project
            .sessions()
            .ensure_session("cli-main", Some("primary"), &root_actor, "active")?;
    let mut current_session_id = default_session.session_id.clone();
    let mut current_session_label = default_session.label.clone();
    let mut messages = project.sessions().load_messages(&current_session_id)?;
    if messages.is_empty() {
        messages.push(Message {
            role: "system".to_string(),
            content: Some(system_prompt.clone()),
            tool_call_id: None,
            tool_calls: None,
        });
        project
            .sessions()
            .save_messages(&current_session_id, &messages)?;
    }

    let request_rx = spawn_request_reader_thread();
    let parent_exit_rx = start_parent_exit_watch(parent_pid);
    let mut stdout = BufWriter::new(io::stdout());
    let mut last_mailbox_poll = Instant::now()
        .checked_sub(MAILBOX_POLL_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_reconcile = Instant::now()
        .checked_sub(RECONCILE_INTERVAL)
        .unwrap_or_else(Instant::now);

    loop {
        if parent_exit_rx.try_recv().is_ok() {
            break;
        }
        let now = Instant::now();
        if now.duration_since(last_reconcile) >= RECONCILE_INTERVAL {
            supervisor.reconcile()?;
            last_reconcile = now;
        }
        if now.duration_since(last_mailbox_poll) >= MAILBOX_POLL_INTERVAL {
            let lines = collect_lead_mailbox_events(&project, &mut lead_cursor, &mut messages)?;
            emit_mailbox_lines(&mut stdout, &lines)?;
            last_mailbox_poll = now;
        }

        let Ok(request_text) = request_rx.recv_timeout(INPUT_POLL_INTERVAL) else {
            continue;
        };
        let request: WireRequest = serde_json::from_str(&request_text)?;
        let outcome = match request {
            WireRequest::ConsoleInput { input } => {
                process_console_input(
                    &input,
                    &repo_root,
                    &client,
                    &llm,
                    &project,
                    &mut messages,
                    &progress,
                    &mut supervisor,
                    &mut skills,
                    &mut lead_cursor,
                    &mut interaction_mode,
                    &system_prompt,
                    &mut current_session_id,
                    &mut current_session_label,
                )
                .await?
            }
            other => {
                execute_wire_request(
                    other,
                    WireRuntime {
                        repo_root: &repo_root,
                        client: &client,
                        llm: &llm,
                        project: &project,
                        messages: &mut messages,
                        progress: &progress,
                        supervisor: &mut supervisor,
                        lead_cursor: &mut lead_cursor,
                        interaction_mode: &interaction_mode,
                        sessions: project.sessions(),
                        current_session_id: &mut current_session_id,
                        current_session_label: &mut current_session_label,
                    },
                )
                .await?
            }
        };

        for frame in &outcome.frames {
            emit_wire_frame(&mut stdout, frame)?;
        }
        stdout.flush()?;
        if matches!(outcome.directive, LoopDirective::Exit) {
            break;
        }
    }

    supervisor.stop_all();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn process_console_input(
    trimmed: &str,
    repo_root: &std::path::Path,
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &crate::activity::ActivityHandle,
    supervisor: &mut AgentSupervisor,
    skills: &mut SkillRegistry,
    lead_cursor: &mut usize,
    interaction_mode: &mut InteractionMode,
    system_prompt: &str,
    current_session_id: &mut String,
    current_session_label: &mut Option<String>,
) -> anyhow::Result<crate::app_commands::CommandOutcome> {
    if let Some(action) = handle_cli_command(trimmed, project, progress, skills)? {
        let outcome = process_cli_action(
            action,
            CliRuntime {
                repo_root,
                project,
                supervisor,
                skills,
                current_session_id,
                current_session_label,
                messages,
                system_prompt,
                interaction_mode,
                auto_team_max_parallel: AUTO_TEAM_MAX_PARALLEL,
            },
        )
        .await?;
        project.sessions().update_state(
            current_session_id,
            current_session_label.as_deref(),
            &interaction_mode.label(),
            "active",
        )?;
        project.sessions().save_messages(current_session_id, messages)?;
        *skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
        return Ok(outcome);
    }

    let outcome = execute_wire_request(
        WireRequest::ChatSend {
            input: trimmed.to_string(),
            focus: Some(interaction_mode.label()),
        },
        WireRuntime {
            repo_root,
            client,
            llm,
            project,
            messages,
            progress,
            supervisor,
            lead_cursor,
            interaction_mode,
            sessions: project.sessions(),
            current_session_id,
            current_session_label,
        },
    )
    .await?;
    project.sessions().update_state(
        current_session_id,
        current_session_label.as_deref(),
        &interaction_mode.label(),
        "active",
    )?;
    project.sessions().save_messages(current_session_id, messages)?;
    *skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
    Ok(outcome)
}

fn parse_root_runtime_args(args: &[String]) -> anyhow::Result<(std::path::PathBuf, Option<u32>)> {
    let mut idx = 0usize;
    let mut repo_root = None::<std::path::PathBuf>;
    let mut parent_pid = None::<u32>;
    while idx < args.len() {
        match args[idx].as_str() {
            "--repo-root" => {
                idx += 1;
                repo_root = Some(std::path::PathBuf::from(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --repo-root value"))?,
                ));
            }
            "--parent-pid" => {
                idx += 1;
                parent_pid = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --parent-pid value"))?
                        .parse::<u32>()?,
                );
            }
            _ => {}
        }
        idx += 1;
    }
    Ok((
        repo_root.ok_or_else(|| anyhow::anyhow!("missing --repo-root"))?,
        parent_pid,
    ))
}

fn spawn_root_runtime_process(
    repo_root: &std::path::Path,
) -> anyhow::Result<(Child, ChildStdin, Receiver<anyhow::Result<WireFrame>>)> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("root-runtime-run")
        .arg("--repo-root")
        .arg(repo_root)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("runtime stdin missing"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("runtime stdout missing"))?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let result = match line {
                Ok(text) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<WireFrame>(trimmed) {
                        Ok(frame) => Ok(frame),
                        Err(_) => Ok(WireFrame::Event {
                            event: WireEnvelope::new(
                                "event",
                                WireEvent::MessageDelta {
                                    role: "system".to_string(),
                                    content: trimmed.to_string(),
                                },
                            ),
                        }),
                    }
                }
                Err(err) => Err(anyhow::anyhow!("runtime read failed: {}", err)),
            };
            if tx.send(result).is_err() {
                break;
            }
        }
    });
    Ok((child, stdin, rx))
}

fn start_parent_exit_watch(parent_pid: Option<u32>) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let Some(parent_pid) = parent_pid else {
        return rx;
    };
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(500));
        if !process_is_alive(parent_pid) {
            let _ = tx.send(());
            break;
        }
    });
    rx
}

fn process_is_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        Command::new("powershell")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-Command",
                &format!(
                    "$p = Get-Process -Id {} -ErrorAction SilentlyContinue; if ($null -ne $p) {{ exit 0 }} else {{ exit 1 }}",
                    pid
                ),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(windows))]
    {
        Command::new("sh")
            .args(["-c", &format!("kill -0 {} >/dev/null 2>&1", pid)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

fn spawn_request_reader_thread() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let stdin = io::stdin();
        let reader = BufReader::new(stdin.lock());
        for line in reader.lines() {
            match line {
                Ok(text) => {
                    if tx.send(text).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn write_wire_request(stdin: &mut ChildStdin, request: &WireRequest) -> anyhow::Result<()> {
    writeln!(stdin, "{}", serde_json::to_string(request)?)?;
    stdin.flush()?;
    Ok(())
}

fn emit_wire_frame(stdout: &mut BufWriter<io::Stdout>, frame: &WireFrame) -> anyhow::Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(frame)?)?;
    Ok(())
}

fn emit_mailbox_lines(stdout: &mut BufWriter<io::Stdout>, lines: &[String]) -> anyhow::Result<()> {
    for line in lines {
        emit_wire_frame(
            stdout,
            &WireFrame::Event {
                event: WireEnvelope::new(
                    "event",
                    WireEvent::MessageDelta {
                        role: "system".to_string(),
                        content: line.clone(),
                    },
                ),
            },
        )?;
    }
    stdout.flush()?;
    Ok(())
}

fn apply_frame_to_console(frame: &WireFrame, console: &mut InteractiveConsole, lines: &mut Vec<String>) {
    match frame {
        WireFrame::Response { response } => match &response.payload {
            WireResponse::Ack { message } => lines.push(message.clone()),
            WireResponse::Error { message } => lines.push(format!("error: {}", message)),
            other => lines.push(serde_json::to_string(other).unwrap_or_default()),
        },
        WireFrame::Event { event } => match &event.payload {
            WireEvent::Error { message } => lines.push(format!("error: {}", message)),
            WireEvent::SessionUpdated {
                focus,
                status,
                abortable,
            } => {
                console.set_prompt(focus.clone());
                if let Some(abortable) = abortable {
                    lines.push(format!(
                        "[session] focus={} status={} abortable={}",
                        focus, status, abortable
                    ));
                } else {
                    lines.push(format!("[session] focus={} status={}", focus, status));
                }
            }
            WireEvent::MessageDelta { content, .. } => lines.push(content.clone()),
            other => lines.push(serde_json::to_string(other).unwrap_or_default()),
        },
    }
}

fn is_exit_ack(frame: &WireFrame) -> bool {
    matches!(
        frame,
        WireFrame::Response {
            response: WireEnvelope {
                payload: WireResponse::Ack { message },
                ..
            }
        } if message == "exit"
    )
}

/// 启动时检查未完成任务，逐一询问用户是否继续；选否则标记为取消。
/// 此函数在进入 raw 终端模式之前调用，使用普通 stdin/stdout 交互。
fn confirm_unfinished_tasks(project: &ProjectContext) -> anyhow::Result<()> {
    let active: Vec<_> = project
        .tasks()
        .list_records()?
        .into_iter()
        .filter(|t| matches!(t.status.as_str(), "in_progress" | "blocked" | "pending"))
        .collect();

    if active.is_empty() {
        return Ok(());
    }

    println!();
    println!("发现 {} 个未完成的任务：", active.len());
    for task in &active {
        println!(
            "  #{} [{}] {}",
            task.id, task.status, task.subject
        );
    }
    println!();

    let stdin = io::stdin();
    let mut cancelled = 0usize;

    for task in &active {
        print!(
            "继续任务 #{} 「{}」? (Y/n): ",
            task.id, task.subject
        );
        io::Write::flush(&mut io::stdout())?;

        let mut input = String::new();
        stdin.lock().read_line(&mut input)?;
        let answer = input.trim().to_ascii_lowercase();

        if answer == "n" || answer == "no" || answer == "否" {
            project
                .tasks()
                .update(task.id, Some("cancelled"), None, None)?;
            println!("  → 已取消");
            cancelled += 1;
        } else {
            project
                .tasks()
                .update(task.id, Some("pending"), None, None)?;
            println!("  → 继续");
        }
    }

    if cancelled > 0 {
        println!();
        println!("已取消 {} 个任务。", cancelled);
    }
    println!();

    Ok(())
}

fn start_main_ui_server(repo_root: std::path::PathBuf, port: u16) -> Option<JoinHandle<()>> {
    match spawn_ui_server(repo_root, current_agent_id(), port) {
        Ok(handle) => Some(handle),
        Err(err) => {
            let address_in_use = err
                .chain()
                .filter_map(|item| item.downcast_ref::<std::io::Error>())
                .any(|io_err| io_err.kind() == std::io::ErrorKind::AddrInUse);
            if address_in_use {
                eprintln!("ui server already available on http://127.0.0.1:{port}");
            } else {
                eprintln!("warning: failed to start ui server on port {port}: {err}");
            }
            None
        }
    }
}
