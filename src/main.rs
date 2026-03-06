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
use std::io::{self, Write};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let env_update = ensure_env_guidance(&cwd)?;
    dotenvy::from_path(cwd.join(".env")).ok();

    if env_update.created {
        println!(
            "已在 {} 生成 .env 引导模板，请先填写 LLM_API_KEY。",
            cwd.display()
        );
    } else if !env_update.added_keys.is_empty() {
        println!(
            "已补齐 .env 缺失项: {}",
            env_update.added_keys.join(", ")
        );
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
        "你是位于 {} 的编码代理。优先使用 task_* 和 worktree_* 工具处理并行或高风险工作；任务是控制面，worktree 是执行面。需要查看生命周期时使用 worktree_events。",
        cwd.display()
    );

    let project = ProjectContext::new(repo_root.clone())?;
    let mut skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
    let progress = new_activity_handle();

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
            Some(CliAction::Continue) => continue,
            None => {}
        }
        if trimmed.is_empty() {
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
        )
        .await?;
        println!();
    }

    Ok(())
}
