use crate::app_commands::{LoopDirective, process_cli_action, process_user_input};
use crate::app_support::{
    InteractionMode, load_repo_env, parse_resident_args, parse_teammate_args, pump_lead_mailbox,
};
use anyhow::Context;
use crate::activity::new_activity_handle;
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
use std::io::{self, Write};
use std::time::Duration;

const AUTO_TEAM_MAX_PARALLEL: usize = 2;

pub async fn run() -> anyhow::Result<()> {
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
        if let Some(action) = handle_cli_command(trimmed, &project, &progress, &skills)? {
            match process_cli_action(
                action,
                &repo_root,
                &project,
                &mut supervisor,
                &mut skills,
                &mut interaction_mode,
                AUTO_TEAM_MAX_PARALLEL,
            )
            .await?
            {
                LoopDirective::Continue => continue,
                LoopDirective::Exit => break,
            }
        }

        process_user_input(
            trimmed,
            &repo_root,
            &client,
            &llm,
            &project,
            &mut messages,
            &progress,
            &mut supervisor,
            &mut lead_cursor,
            &interaction_mode,
        )
        .await?;
    }

    supervisor.stop_all();
    Ok(())
}
