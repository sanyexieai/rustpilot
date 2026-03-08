use anyhow::Context;
use rustpilot::activity::new_activity_handle;
use rustpilot::agent::{run_agent_loop, tool_definitions};
use rustpilot::cli::{CliAction, handle_cli_command};
use rustpilot::config::LlmConfig;
use rustpilot::openai_compat::Message;
use rustpilot::project_tools::ProjectContext;
use rustpilot::runtime_env::{
    detect_repo_root, ensure_env_guidance, llm_timeout_secs, prompt_and_store_llm_api_key,
};
use rustpilot::skills::SkillRegistry;
use rustpilot::team::{TeamRuntime, get_worker_endpoint, run_teammate_once, send_input_to_worker};
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
        return run_teammate_once(teammate.repo_root, teammate.task_id, teammate.owner).await;
    }

    let cwd = std::env::current_dir()?;
    let env_update = ensure_env_guidance(&cwd)?;
    dotenvy::from_path(cwd.join(".env")).ok();

    if env_update.created {
        println!(
            "已在 {} 生成 .env 引导模板，请先填写 LLM_API_KEY。",
            cwd.display()
        );
    } else if !env_update.added_keys.is_empty() {
        println!("已补齐 .env 缺失项: {}", env_update.added_keys.join(", "));
    }

    let repo_root = detect_repo_root(&cwd).unwrap_or_else(|| cwd.clone());
    let llm = match LlmConfig::from_env() {
        Ok(cfg) => cfg,
        Err(err) if err.to_string().contains("LLM_API_KEY is required") => {
            println!("未检测到有效的 LLM_API_KEY。");
            println!("可直接在命令行补齐并写入 {}/.env。", cwd.display());

            if !prompt_and_store_llm_api_key(&cwd)? {
                println!("已取消输入，请补齐 .env 后重新运行。");
                return Ok(());
            }

            dotenvy::from_path_override(cwd.join(".env")).ok();
            match LlmConfig::from_env() {
                Ok(cfg) => cfg,
                Err(err) if err.to_string().contains("LLM_API_KEY is required") => {
                    println!("LLM_API_KEY 仍无效，请检查 .env 后重新运行。");
                    return Ok(());
                }
                Err(err) => return Err(err),
            }
        }
        Err(err) => return Err(err),
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(llm_timeout_secs()))
        .build()?;

    let system_prompt = format!(
        "你是位于 {} 的编码代理。优先使用 task_* 和 worktree_* 工具处理并行或高风险工作；任务是控制面，worktree 是执行面。团队协作时使用 team_send/team_inbox。需要查看生命周期时使用 worktree_events。",
        cwd.display()
    );

    let project = ProjectContext::new(repo_root.clone())?;
    let mut skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
    let progress = new_activity_handle();
    let mut team: Option<TeamRuntime> = None;
    let mut lead_cursor = 0usize;
    let mut interaction_mode = InteractionMode::TeamQueue;

    println!("仓库根目录: {}", repo_root.display());
    if !project.worktrees().git_available {
        println!("提示: 当前目录不是 git 仓库，worktree_* 工具会返回错误。");
    }

    let mut messages = vec![Message {
        role: "system".to_string(),
        content: Some(system_prompt),
        tool_call_id: None,
        tool_calls: None,
    }];

    loop {
        reconcile_team_runtime(&project, &repo_root, &mut team, AUTO_TEAM_MAX_PARALLEL)?;
        pump_lead_mailbox(&project, &mut lead_cursor, &mut messages)?;

        let mut input = String::new();
        print!("> ");
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
                println!("已切换交互焦点: lead");
                continue;
            }
            Some(CliAction::FocusTeam) => {
                interaction_mode = InteractionMode::TeamQueue;
                println!("已切换交互焦点: team");
                continue;
            }
            Some(CliAction::FocusWorker { task_id }) => {
                match get_worker_endpoint(&repo_root, task_id)? {
                    Some(endpoint) if endpoint.status == "running" => {
                        interaction_mode = InteractionMode::Worker { task_id };
                        println!(
                            "已切换交互焦点: worker task={} channel={} target={}",
                            task_id, endpoint.channel, endpoint.target
                        );
                    }
                    Some(endpoint) => {
                        println!(
                            "task {} 的 worker 当前状态为 {}，不能切换。",
                            task_id, endpoint.status
                        );
                    }
                    None => println!("未找到 task {} 的 worker。", task_id),
                }
                continue;
            }
            Some(CliAction::FocusStatus) => {
                println!("当前交互焦点: {}", interaction_mode.label());
                continue;
            }
            Some(CliAction::TeamRun { goal }) => {
                let task = project.tasks().create(&goal, "由 /team run 创建")?;
                println!("已创建团队任务:\n{}", task);
                ensure_team_running(&repo_root, &mut team, AUTO_TEAM_MAX_PARALLEL);
                continue;
            }
            Some(CliAction::TeamStart { max_parallel }) => {
                if team.is_some() {
                    println!("team 调度器已在运行。");
                    continue;
                }
                team = Some(TeamRuntime::start(repo_root.clone(), max_parallel));
                println!("team 调度器已启动，max_parallel={}", max_parallel);
                continue;
            }
            Some(CliAction::TeamStop) => {
                if let Some(mut runtime) = team.take() {
                    runtime.stop();
                    println!("team 调度器已停止。");
                } else {
                    println!("team 调度器未运行。");
                }
                continue;
            }
            Some(CliAction::TeamStatus) => {
                if let Some(runtime) = team.as_ref() {
                    let s = runtime.snapshot();
                    println!(
                        "team: running={} max_parallel={} launched={} completed={} failed={}",
                        s.running, s.max_parallel, s.launched, s.completed, s.failed
                    );
                } else {
                    println!("team: stopped");
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
                println!("用法: /ask <内容>");
                continue;
            }
            messages.push(Message {
                role: "user".to_string(),
                content: Some(prompt.to_string()),
                tool_call_id: None,
                tool_calls: None,
            });

            let tools = tool_definitions();
            run_agent_loop(
                &client,
                &llm,
                &project,
                &mut messages,
                &tools,
                progress.clone(),
                None,
            )
            .await?;
            reconcile_team_runtime(&project, &repo_root, &mut team, AUTO_TEAM_MAX_PARALLEL)?;
            pump_lead_mailbox(&project, &mut lead_cursor, &mut messages)?;
            println!();
            continue;
        }

        if !trimmed.starts_with('/') {
            match &interaction_mode {
                InteractionMode::TeamQueue => {
                    let task = project.tasks().create(trimmed, "由自然输入自动入队")?;
                    println!("已自动创建团队任务:\n{}", task);
                    ensure_team_running(&repo_root, &mut team, AUTO_TEAM_MAX_PARALLEL);
                }
                InteractionMode::Lead => {
                    messages.push(Message {
                        role: "user".to_string(),
                        content: Some(trimmed.to_string()),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                    let tools = tool_definitions();
                    run_agent_loop(
                        &client,
                        &llm,
                        &project,
                        &mut messages,
                        &tools,
                        progress.clone(),
                        None,
                    )
                    .await?;
                    reconcile_team_runtime(
                        &project,
                        &repo_root,
                        &mut team,
                        AUTO_TEAM_MAX_PARALLEL,
                    )?;
                    pump_lead_mailbox(&project, &mut lead_cursor, &mut messages)?;
                    println!();
                }
                InteractionMode::Worker { task_id } => {
                    match send_input_to_worker(&repo_root, *task_id, trimmed) {
                        Ok(text) => println!("{}", text),
                        Err(err) => println!("路由到 worker 失败: {}", err),
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

        let tools = tool_definitions();
        run_agent_loop(
            &client,
            &llm,
            &project,
            &mut messages,
            &tools,
            progress.clone(),
            None,
        )
        .await?;
        reconcile_team_runtime(&project, &repo_root, &mut team, AUTO_TEAM_MAX_PARALLEL)?;
        pump_lead_mailbox(&project, &mut lead_cursor, &mut messages)?;
        println!();
    }

    if let Some(mut runtime) = team {
        runtime.stop();
    }

    Ok(())
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

fn ensure_team_running(repo_root: &PathBuf, team: &mut Option<TeamRuntime>, max_parallel: usize) {
    if team.is_none() {
        *team = Some(TeamRuntime::start(repo_root.clone(), max_parallel));
        println!("team 调度器已自动启动，max_parallel={}", max_parallel);
    }
}

fn reconcile_team_runtime(
    project: &ProjectContext,
    repo_root: &PathBuf,
    team: &mut Option<TeamRuntime>,
    max_parallel: usize,
) -> anyhow::Result<()> {
    let pending = project.tasks().pending_count()?;
    if pending > 0 {
        ensure_team_running(repo_root, team, max_parallel);
        return Ok(());
    }

    if let Some(runtime) = team.as_ref()
        && runtime.snapshot().running == 0
    {
        if let Some(mut runtime) = team.take() {
            runtime.stop();
            println!("team 调度器已自动停止（队列为空）。");
        }
    }
    Ok(())
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
        if item.requires_ack {
            let _ = project.mailbox().ack("lead", &item.msg_id, "收到，继续");
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
    #[serde(default)]
    requires_ack: bool,
}

fn parse_teammate_args(args: &[String]) -> anyhow::Result<TeammateArgs> {
    let mut repo_root = None::<PathBuf>;
    let mut task_id = None::<u64>;
    let mut owner = None::<String>;
    let mut idx = 0usize;

    while idx < args.len() {
        match args[idx].as_str() {
            "--repo-root" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| anyhow::anyhow!("缺少 --repo-root 值"))?;
                repo_root = Some(PathBuf::from(value));
            }
            "--task-id" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| anyhow::anyhow!("缺少 --task-id 值"))?;
                task_id = Some(value.parse::<u64>()?);
            }
            "--owner" => {
                idx += 1;
                let value = args
                    .get(idx)
                    .ok_or_else(|| anyhow::anyhow!("缺少 --owner 值"))?;
                owner = Some(value.to_string());
            }
            flag => anyhow::bail!("未知参数: {}", flag),
        }
        idx += 1;
    }

    Ok(TeammateArgs {
        repo_root: repo_root.ok_or_else(|| anyhow::anyhow!("必须提供 --repo-root"))?,
        task_id: task_id.ok_or_else(|| anyhow::anyhow!("必须提供 --task-id"))?,
        owner: owner.unwrap_or_else(|| "teammate".to_string()),
    })
}
