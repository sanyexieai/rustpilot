use crate::activity::new_activity_handle;
use crate::app_commands::{CliRuntime, LoopDirective, process_cli_action};
use crate::app_support::{
    InteractionMode, load_repo_env, parse_resident_args, parse_teammate_args, pump_lead_mailbox,
};
use crate::cli::handle_cli_command;
use crate::config::{LlmConfig, default_llm_user_agent};
use crate::openai_compat::Message;
use crate::project_tools::ProjectContext;
use crate::prompt_manager::render_lead_system_prompt;
use crate::resident_agents::{AgentSupervisor, run_resident_agent};
use crate::runtime_env::{
    detect_repo_root, ensure_env_guidance, llm_timeout_secs_for_provider,
    prompt_and_store_llm_api_key,
};
use crate::skills::SkillRegistry;
use crate::team::run_teammate_once;
use crate::wire::{WireEvent, WireFrame, WireResponse};
use crate::wire_exec::{WireRuntime, execute_wire_request};
use anyhow::Context;
use std::io::{self, Write};
use std::time::Duration;

const AUTO_TEAM_MAX_PARALLEL: usize = 2;

pub async fn run() -> anyhow::Result<()> {
    unsafe {
        std::env::set_var("RUSTPILOT_AGENT_ID", "lead");
    }
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
        let resident = parse_resident_args(&args[2..], AUTO_TEAM_MAX_PARALLEL)?;
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
        .timeout(Duration::from_secs(llm_timeout_secs_for_provider(
            &llm.provider,
        )))
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
    let system_prompt = render_lead_system_prompt(&repo_root)?;
    let default_session =
        project
            .sessions()
            .ensure_session("cli-main", Some("primary"), "lead", "active")?;
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

    println!("repo root: {}", repo_root.display());
    println!("focus: {}", interaction_mode.label());
    println!("session: {}", current_session_id);
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
        if let Some(action) = handle_cli_command(trimmed, &project, &progress, &skills)? {
            let outcome = process_cli_action(
                action,
                CliRuntime {
                    repo_root: &repo_root,
                    project: &project,
                    supervisor: &mut supervisor,
                    skills: &mut skills,
                    current_session_id: &mut current_session_id,
                    current_session_label: &mut current_session_label,
                    messages: &mut messages,
                    system_prompt: &system_prompt,
                    interaction_mode: &mut interaction_mode,
                    auto_team_max_parallel: AUTO_TEAM_MAX_PARALLEL,
                },
            )
            .await?;
            emit_wire_frames(&outcome.frames);
            project.sessions().update_state(
                &current_session_id,
                current_session_label.as_deref(),
                &interaction_mode.label(),
                "active",
            )?;
            project
                .sessions()
                .save_messages(&current_session_id, &messages)?;
            match outcome.directive {
                LoopDirective::Continue => continue,
                LoopDirective::Exit => break,
            }
        }

        let outcome = execute_wire_request(
            crate::wire::WireRequest::ChatSend {
                input: trimmed.to_string(),
                focus: Some(interaction_mode.label()),
            },
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
        .await?;
        emit_wire_frames(&outcome.frames);
        project.sessions().update_state(
            &current_session_id,
            current_session_label.as_deref(),
            &interaction_mode.label(),
            "active",
        )?;
        project
            .sessions()
            .save_messages(&current_session_id, &messages)?;
    }

    supervisor.stop_all();
    Ok(())
}

fn emit_wire_frames(frames: &[WireFrame]) {
    for frame in frames {
        match frame {
            WireFrame::Response { response } => match &response.payload {
                WireResponse::Ack { message } => println!("{}", message),
                WireResponse::Error { message } => println!("error: {}", message),
                other => println!("{}", serde_json::to_string(other).unwrap_or_default()),
            },
            WireFrame::Event { event } => match &event.payload {
                WireEvent::Error { message } => println!("error: {}", message),
                WireEvent::SessionUpdated {
                    focus,
                    status,
                    abortable,
                } => {
                    if let Some(abortable) = abortable {
                        println!(
                            "[session] focus={} status={} abortable={}",
                            focus, status, abortable
                        )
                    } else {
                        println!("[session] focus={} status={}", focus, status)
                    }
                }
                other => println!("{}", serde_json::to_string(other).unwrap_or_default()),
            },
        }
    }
}
