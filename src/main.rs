use anyhow::Context;
use rustpilot::activity::new_activity_handle;
use rustpilot::agent::{run_agent_loop, tool_definitions};
use rustpilot::cli::{CliAction, handle_cli_command};
use rustpilot::config::{LlmConfig, default_llm_user_agent};
use rustpilot::openai_compat::Message;
use rustpilot::project_tools::{EnergyMode, ProjectContext};
use rustpilot::resident_agents::{AgentSupervisor, resident_listen_port, run_resident_agent};
use rustpilot::runtime_env::{
    detect_repo_root, ensure_env_guidance, llm_timeout_secs, prompt_and_store_llm_api_key,
};
use rustpilot::skills::SkillRegistry;
use rustpilot::team::{
    get_worker_endpoint, render_agent_policy, render_policy_overview, render_task_policy,
    run_teammate_once, send_input_to_worker,
};
use serde::Deserialize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

const AUTO_TEAM_MAX_PARALLEL: usize = 2;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|value| value == "teammate-run") {
        let teammate = parse_teammate_args(&args[2..])?;
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
        let resident = parse_resident_args(&args[2..])?;
        load_repo_env(&resident.repo_root);
        return run_resident_agent(
            resident.repo_root,
            resident.agent_id,
            resident.role,
            resident.max_parallel,
        );
    }

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
    let llm = match LlmConfig::from_repo_root(&repo_root) {
        Ok(cfg) => cfg,
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
            LlmConfig::from_repo_root(&repo_root)?
        }
        Err(err) => return Err(err),
    };
    let client = reqwest::Client::builder()
        .user_agent(default_llm_user_agent())
        .timeout(Duration::from_secs(llm_timeout_secs()))
        .build()?;

    let project = ProjectContext::new(repo_root.clone())?;
    project.agents().ensure_profile(
        "lead",
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
        "lead",
        "idle",
        None,
        Some("cli"),
        Some("main"),
        Some("primary console"),
    )?;
    project
        .budgets()
        .ensure_ledger("lead", 120_000, 30_000, 12_000)?;

    let mut supervisor =
        AgentSupervisor::start_defaults(repo_root.clone(), AUTO_TEAM_MAX_PARALLEL)?;
    let mut skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
    let progress = new_activity_handle();
    let mut lead_cursor = 0usize;
    let mut interaction_mode = InteractionMode::Lead;
    let system_prompt = format!(
        "You are the lead coding agent in {}. Use task_* and worktree_* for delegated work. Use team_send and team_inbox when coordinating with the team.",
        repo_root.display()
    );
    let mut messages = vec![Message {
        role: "system".to_string(),
        content: Some(system_prompt),
        tool_call_id: None,
        tool_calls: None,
    }];

    println!("repo root: {}", repo_root.display());
    println!("focus: {}", interaction_mode.label());
    if !project.worktrees().git_available {
        println!("warning: current directory is not a git repository");
    }

    loop {
        supervisor.reconcile()?;
        pump_lead_mailbox(&project, &mut lead_cursor, &mut messages)?;

        let mut input = String::new();
        print!("{}> ", interaction_mode.label());
        io::stdout().flush().ok();
        let bytes = io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
        if bytes == 0 {
            break;
        }

        let trimmed = input.trim();
        match handle_cli_command(trimmed, &project, &progress, &skills)? {
            Some(CliAction::Exit) => break,
            Some(CliAction::ReloadSkills) => {
                skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
                continue;
            }
            Some(CliAction::FocusLead) => {
                interaction_mode = InteractionMode::Lead;
                let _ = project.budgets().record_usage("lead", 10);
                maybe_reflect_energy(
                    &project,
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
                println!("focus: lead");
                continue;
            }
            Some(CliAction::FocusTeam) => {
                interaction_mode = InteractionMode::TeamQueue;
                let _ = project.budgets().record_usage("lead", 5);
                maybe_reflect_energy(
                    &project,
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
                println!("focus: team");
                continue;
            }
            Some(CliAction::FocusWorker { task_id }) => {
                match get_worker_endpoint(&repo_root, task_id)? {
                    Some(endpoint) if endpoint.status == "running" => {
                        interaction_mode = InteractionMode::Worker { task_id };
                        let _ = project.budgets().record_usage("lead", 5);
                        maybe_reflect_energy(
                            &project,
                            "lead",
                            "focus.worker",
                            Some(task_id),
                            "switched focus to worker",
                        );
                        println!(
                            "focus: worker task={} channel={} target={}",
                            task_id, endpoint.channel, endpoint.target
                        );
                    }
                    Some(endpoint) => {
                        println!("worker for task {} is {}", task_id, endpoint.status);
                    }
                    None => println!("worker for task {} not found", task_id),
                }
                continue;
            }
            Some(CliAction::FocusStatus) => {
                println!("current focus: {}", interaction_mode.label());
                continue;
            }
            Some(CliAction::ReplyTask { task_id, content }) => {
                let worker_running = matches!(
                    get_worker_endpoint(&repo_root, task_id)?,
                    Some(endpoint) if endpoint.status == "running"
                );
                let updated = project.tasks().append_user_reply(
                    task_id,
                    &content,
                    if worker_running {
                        "in_progress"
                    } else {
                        "pending"
                    },
                )?;
                let _ = project.mailbox().send_typed(
                    "lead",
                    &format!("teammate-{}", task_id),
                    "task.clarification",
                    &format!("user clarification: {}", content),
                    Some(task_id),
                    Some(&format!("task-{}", task_id)),
                    false,
                    None,
                );
                if worker_running {
                    println!("clarification sent to running worker:\n{}", updated);
                } else {
                    println!("clarification appended and task re-queued:\n{}", updated);
                    let _ = supervisor.ensure_running("scheduler");
                }
                continue;
            }
            Some(CliAction::TeamRun { goal, priority }) => {
                let task = project.tasks().create_with_priority(
                    &goal,
                    &build_priority_task_description("/team run", &priority, &goal),
                    &priority,
                )?;
                println!("task created:\n{}", task);
                let _ = supervisor.ensure_running("scheduler");
                continue;
            }
            Some(CliAction::TeamStart { .. }) => {
                supervisor.ensure_running("scheduler")?;
                println!("resident scheduler ensured");
                continue;
            }
            Some(CliAction::TeamStop) => {
                supervisor.stop_agent("scheduler");
                println!("resident scheduler stopped");
                continue;
            }
            Some(CliAction::TeamStatus) => {
                let configured = project.residents().enabled_agents()?;
                if configured.is_empty() {
                    println!(
                        "team: no enabled resident agents pending={}",
                        project.tasks().pending_count()?
                    );
                } else {
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
                            format!(
                                "{}={} status={} backlog={} loop_ms={} note={}{} last={}",
                                item.agent_id,
                                running,
                                status,
                                backlog,
                                loop_ms,
                                note,
                                endpoint,
                                last_action
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if alerts.is_empty() {
                        println!(
                            "team: {} pending={} alerts=none",
                            states,
                            project.tasks().pending_count()?
                        );
                    } else {
                        println!(
                            "team: {} pending={} alerts={}",
                            states,
                            project.tasks().pending_count()?,
                            alerts.join(" | ")
                        );
                    }
                }
                continue;
            }
            Some(CliAction::Residents) => {
                let configured = project.residents().list_all()?;
                if configured.is_empty() {
                    println!("no resident agents configured");
                } else {
                    let lines = configured
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
                            format!(
                                "- {} role={} mode={} behavior={} enabled={} running={} status={} backlog={} loop_ms={} port={} last_msg={} last_error={} note={} last={}",
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
                                last_msg,
                                last_error,
                                note,
                                last_action
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    println!("{}", lines);
                }
                continue;
            }
            Some(CliAction::ResidentSend {
                agent_id,
                msg_type,
                content,
            }) => {
                let _ = project.mailbox().send_typed(
                    "lead", &agent_id, &msg_type, &content, None, None, false, None,
                )?;
                let _ = project.decisions().append(
                    "lead",
                    "resident.message.sent",
                    None,
                    None,
                    &format!("sent {} to {}", msg_type, agent_id),
                    "manual resident dispatch from cli",
                );
                let _ = supervisor.ensure_running(&agent_id);
                println!("resident message sent: {} -> {}", msg_type, agent_id);
                continue;
            }
            Some(CliAction::PolicyOverview) => {
                println!(
                    "{}",
                    render_policy_overview(&project, AUTO_TEAM_MAX_PARALLEL)
                );
                continue;
            }
            Some(CliAction::PolicyTask { task_id }) => {
                match render_task_policy(&project, task_id, AUTO_TEAM_MAX_PARALLEL) {
                    Ok(text) => println!("{}", text),
                    Err(err) => println!("failed to read task policy: {}", err),
                }
                continue;
            }
            Some(CliAction::PolicyAgent { agent_id }) => {
                match render_agent_policy(&project, &agent_id) {
                    Ok(text) => println!("{}", text),
                    Err(err) => println!("failed to read agent policy: {}", err),
                }
                continue;
            }
            Some(CliAction::Continue) => continue,
            None => {}
        }

        if trimmed.is_empty() {
            continue;
        }

        if let Some(prompt) = trimmed.strip_prefix("/ask ").map(str::trim) {
            if prompt.is_empty() {
                println!("usage: /ask <content>");
                continue;
            }
            messages.push(Message {
                role: "user".to_string(),
                content: Some(prompt.to_string()),
                tool_call_id: None,
                tool_calls: None,
            });
            let _ = project
                .budgets()
                .record_usage("lead", estimate_text_tokens(prompt).saturating_add(40));
            maybe_reflect_energy(&project, "lead", "user.ask", None, "processed /ask input");
            run_lead_turn(
                &client,
                &llm,
                &project,
                &mut messages,
                &progress,
                &mut supervisor,
                &mut lead_cursor,
            )
            .await?;
            continue;
        }

        if !trimmed.starts_with('/') {
            match &interaction_mode {
                InteractionMode::TeamQueue => {
                    if looks_like_question(trimmed) {
                        let _ = project
                            .budgets()
                            .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(40));
                        maybe_reflect_energy(
                            &project,
                            "lead",
                            "lead.message",
                            None,
                            "processed question from team focus via lead",
                        );
                        messages.push(Message {
                            role: "user".to_string(),
                            content: Some(trimmed.to_string()),
                            tool_call_id: None,
                            tool_calls: None,
                        });
                        run_lead_turn(
                            &client,
                            &llm,
                            &project,
                            &mut messages,
                            &progress,
                            &mut supervisor,
                            &mut lead_cursor,
                        )
                        .await?;
                        continue;
                    }
                    let (priority, goal) = parse_priority_prefixed_goal(trimmed);
                    let _ = project
                        .budgets()
                        .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(20));
                    maybe_reflect_energy(
                        &project,
                        "lead",
                        "task.enqueue",
                        None,
                        "forwarded team queue input to concierge",
                    );
                    let payload = format!("[{}] {}", priority, goal);
                    let _ = project.mailbox().send_typed(
                        "lead",
                        "concierge",
                        "user.request",
                        &payload,
                        None,
                        None,
                        false,
                        None,
                    )?;
                    let _ = project.decisions().append(
                        "lead",
                        "resident.message.sent",
                        None,
                        None,
                        "sent team queue input to concierge",
                        &format!("priority={} target=concierge", priority),
                    );
                    let _ = supervisor.ensure_running("concierge");
                    println!("forwarded to concierge: {}", payload);
                }
                InteractionMode::Lead => {
                    let _ = project
                        .budgets()
                        .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(40));
                    maybe_reflect_energy(
                        &project,
                        "lead",
                        "lead.message",
                        None,
                        "processed lead input",
                    );
                    messages.push(Message {
                        role: "user".to_string(),
                        content: Some(trimmed.to_string()),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                    run_lead_turn(
                        &client,
                        &llm,
                        &project,
                        &mut messages,
                        &progress,
                        &mut supervisor,
                        &mut lead_cursor,
                    )
                    .await?;
                }
                InteractionMode::Worker { task_id } => {
                    match send_input_to_worker(&repo_root, *task_id, trimmed) {
                        Ok(text) => println!("{}", text),
                        Err(err) => println!("failed to route to worker: {}", err),
                    }
                }
            }
            continue;
        }

        messages.push(Message {
            role: "user".to_string(),
            content: Some(trimmed.to_string()),
            tool_call_id: None,
            tool_calls: None,
        });
        let _ = project
            .budgets()
            .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(30));
        maybe_reflect_energy(&project, "lead", "command", None, "processed command");
        run_lead_turn(
            &client,
            &llm,
            &project,
            &mut messages,
            &progress,
            &mut supervisor,
            &mut lead_cursor,
        )
        .await?;
    }

    supervisor.stop_all();
    Ok(())
}

fn load_repo_env(repo_root: &PathBuf) {
    dotenvy::from_path_override(repo_root.join(".env")).ok();
}

async fn run_lead_turn(
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &rustpilot::activity::ActivityHandle,
    supervisor: &mut AgentSupervisor,
    lead_cursor: &mut usize,
) -> anyhow::Result<()> {
    let tools = tool_definitions();
    let mut lead_messages = prepare_messages_for_lead(project, messages);
    run_agent_loop(
        client,
        llm,
        project,
        &mut lead_messages,
        &tools,
        progress.clone(),
        None,
    )
    .await?;
    *messages = lead_messages;
    supervisor.reconcile()?;
    pump_lead_mailbox(project, lead_cursor, messages)?;
    println!();
    Ok(())
}

fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    chars.saturating_div(4).saturating_add(1)
}

fn looks_like_question(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.contains('?') || trimmed.contains('？') {
        return true;
    }
    let lower = trimmed.to_lowercase();
    [
        "what",
        "why",
        "how",
        "when",
        "where",
        "who",
        "can you",
        "could you",
        "would you",
        "什么",
        "为啥",
        "为什么",
        "怎么",
        "如何",
        "能不能",
        "可不可以",
        "是否",
        "有没有",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
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
                &["budget entered low mode", "should shrink follow-up scope"],
                Some("reduce exploration and close out smaller actions"),
                true,
            );
            let _ = project.proposals().create(
                agent_id,
                trigger,
                task_id,
                "shrink execution scope under low energy",
                summary,
                &["budget entered low mode", "should shrink follow-up scope"],
                Some("reduce exploration and prioritize close-out work"),
            );
        }
        Some(EnergyMode::Exhausted) => {
            let _ = project.reflections().append(
                agent_id,
                trigger,
                task_id,
                summary,
                &["budget exhausted", "agent should pause non-critical work"],
                Some("keep only critical responses until budget recovers"),
                true,
            );
            let _ = project.proposals().create(
                agent_id,
                trigger,
                task_id,
                "pause non-critical work under exhausted budget",
                summary,
                &["budget exhausted", "agent should pause non-critical work"],
                Some("keep only critical responses until budget recovers"),
            );
        }
        _ => {}
    }
}

fn prepare_messages_for_lead(project: &ProjectContext, messages: &[Message]) -> Vec<Message> {
    match project.budgets().energy_mode("lead").ok().flatten() {
        Some(EnergyMode::Constrained) => trim_messages(messages, 12),
        Some(EnergyMode::Low) => trim_messages(messages, 8),
        Some(EnergyMode::Exhausted) => trim_messages(messages, 4),
        _ => messages.to_vec(),
    }
}

fn trim_messages(messages: &[Message], keep_tail: usize) -> Vec<Message> {
    if messages.len() <= keep_tail.saturating_add(1) {
        return messages.to_vec();
    }
    let mut trimmed = Vec::new();
    if let Some(system) = messages.first().cloned() {
        trimmed.push(system);
    }
    let start = messages.len().saturating_sub(keep_tail);
    for message in &messages[start..] {
        trimmed.push(message.clone());
    }
    trimmed
}

fn parse_priority_prefixed_goal(input: &str) -> (String, String) {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('[')
        && let Some((raw_priority, goal)) = rest.split_once(']')
    {
        let priority = raw_priority.trim().to_lowercase();
        if matches!(priority.as_str(), "critical" | "high" | "medium" | "low") {
            let goal = goal.trim();
            if !goal.is_empty() {
                return (priority, goal.to_string());
            }
        }
    }
    ("medium".to_string(), trimmed.to_string())
}

fn build_priority_task_description(source: &str, priority: &str, goal: &str) -> String {
    format!(
        "[SOURCE={}][PRIORITY={}]\nGoal:\n{}\n\nExecution notes:\n{}",
        source,
        priority,
        goal,
        priority_execution_notes(priority)
    )
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

enum InteractionMode {
    TeamQueue,
    Lead,
    Worker { task_id: u64 },
}

impl InteractionMode {
    fn label(&self) -> String {
        match self {
            Self::TeamQueue => "team".to_string(),
            Self::Lead => "lead".to_string(),
            Self::Worker { task_id } => format!("worker({})", task_id),
        }
    }
}

fn pump_lead_mailbox(
    project: &ProjectContext,
    cursor: &mut usize,
    messages: &mut Vec<Message>,
) -> anyhow::Result<()> {
    let raw = project.mailbox().poll("lead", *cursor, 50)?;
    let polled: MailPoll = serde_json::from_str(&raw)?;
    *cursor = polled.next_cursor;
    for item in polled.items {
        println!(
            "[mail][{}][{}][from={}] {}",
            item.cursor, item.msg_type, item.from, item.message
        );
        if item.msg_type == "task.request_clarification"
            && let Some(task_id) = item.task_id
        {
            let _ = project.tasks().update(task_id, Some("blocked"), None);
            println!(
                "[clarification] task {} blocked, use /reply {} <message>",
                task_id, task_id
            );
        }
        if item.requires_ack {
            let _ = project.mailbox().ack("lead", &item.msg_id, "received");
        }
        if matches!(
            item.msg_type.as_str(),
            "task.started"
                | "task.progress"
                | "task.result"
                | "task.failed"
                | "task.blocked"
                | "task.request_clarification"
        ) {
            messages.push(Message {
                role: "user".to_string(),
                content: Some(format!(
                    "[team-update][from={}][type={}][msg_id={}] {}",
                    item.from, item.msg_type, item.msg_id, item.message
                )),
                tool_call_id: None,
                tool_calls: None,
            });
        }
    }
    Ok(())
}

struct TeammateArgs {
    repo_root: PathBuf,
    task_id: u64,
    owner: String,
    role_hint: String,
}

struct ResidentArgs {
    repo_root: PathBuf,
    agent_id: String,
    role: String,
    max_parallel: usize,
}

#[derive(Debug, Deserialize)]
struct MailPoll {
    next_cursor: usize,
    items: Vec<MailItem>,
}

#[derive(Debug, Deserialize)]
struct MailItem {
    cursor: usize,
    msg_id: String,
    msg_type: String,
    from: String,
    message: String,
    task_id: Option<u64>,
    #[serde(default)]
    requires_ack: bool,
}

fn parse_teammate_args(args: &[String]) -> anyhow::Result<TeammateArgs> {
    let mut repo_root = None::<PathBuf>;
    let mut task_id = None::<u64>;
    let mut owner = None::<String>;
    let mut role_hint = None::<String>;
    let mut idx = 0usize;

    while idx < args.len() {
        match args[idx].as_str() {
            "--repo-root" => {
                idx += 1;
                repo_root =
                    Some(PathBuf::from(args.get(idx).ok_or_else(|| {
                        anyhow::anyhow!("missing --repo-root value")
                    })?));
            }
            "--task-id" => {
                idx += 1;
                task_id = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --task-id value"))?
                        .parse::<u64>()?,
                );
            }
            "--owner" => {
                idx += 1;
                owner = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --owner value"))?
                        .to_string(),
                );
            }
            "--role-hint" => {
                idx += 1;
                role_hint = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --role-hint value"))?
                        .to_string(),
                );
            }
            flag => anyhow::bail!("unknown argument: {}", flag),
        }
        idx += 1;
    }

    Ok(TeammateArgs {
        repo_root: repo_root.ok_or_else(|| anyhow::anyhow!("missing --repo-root"))?,
        task_id: task_id.ok_or_else(|| anyhow::anyhow!("missing --task-id"))?,
        owner: owner.unwrap_or_else(|| "teammate".to_string()),
        role_hint: role_hint.unwrap_or_else(|| "developer".to_string()),
    })
}

fn parse_resident_args(args: &[String]) -> anyhow::Result<ResidentArgs> {
    let mut repo_root = None::<PathBuf>;
    let mut agent_id = None::<String>;
    let mut role = None::<String>;
    let mut max_parallel = None::<usize>;
    let mut idx = 0usize;

    while idx < args.len() {
        match args[idx].as_str() {
            "--repo-root" => {
                idx += 1;
                repo_root =
                    Some(PathBuf::from(args.get(idx).ok_or_else(|| {
                        anyhow::anyhow!("missing --repo-root value")
                    })?));
            }
            "--agent-id" => {
                idx += 1;
                agent_id = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --agent-id value"))?
                        .to_string(),
                );
            }
            "--role" => {
                idx += 1;
                role = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --role value"))?
                        .to_string(),
                );
            }
            "--max-parallel" => {
                idx += 1;
                max_parallel = Some(
                    args.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("missing --max-parallel value"))?
                        .parse::<usize>()?
                        .max(1),
                );
            }
            flag => anyhow::bail!("unknown argument: {}", flag),
        }
        idx += 1;
    }

    Ok(ResidentArgs {
        repo_root: repo_root.ok_or_else(|| anyhow::anyhow!("missing --repo-root"))?,
        agent_id: agent_id.ok_or_else(|| anyhow::anyhow!("missing --agent-id"))?,
        role: role.ok_or_else(|| anyhow::anyhow!("missing --role"))?,
        max_parallel: max_parallel.unwrap_or(AUTO_TEAM_MAX_PARALLEL),
    })
}
