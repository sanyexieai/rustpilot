use crate::activity::{ActivityHandle, render_activity};
use crate::mcp::init_mcp_tool;
use crate::project_tools::ProjectContext;
use crate::skills::{SkillRegistry, init_tool_skill};

pub enum CliAction {
    Continue,
    ReloadSkills,
    FocusLead,
    FocusTeam,
    FocusWorker { task_id: u64 },
    FocusStatus,
    TeamRun { goal: String },
    TeamStart { max_parallel: usize },
    TeamStop,
    TeamStatus,
    Exit,
}

pub fn handle_cli_command(
    trimmed: &str,
    project: &ProjectContext,
    progress: &ActivityHandle,
    skills: &SkillRegistry,
) -> anyhow::Result<Option<CliAction>> {
    if matches!(trimmed, "q" | "quit" | "exit") {
        return Ok(Some(CliAction::Exit));
    }
    if let Some(rest) = trimmed.strip_prefix("/focus").map(str::trim) {
        if rest.is_empty() || rest == "status" {
            return Ok(Some(CliAction::FocusStatus));
        }
        if rest == "lead" {
            return Ok(Some(CliAction::FocusLead));
        }
        if rest == "team" {
            return Ok(Some(CliAction::FocusTeam));
        }
        if let Some(arg) = rest.strip_prefix("worker").map(str::trim) {
            if arg.is_empty() {
                println!("用法: /focus worker <task_id>");
                return Ok(Some(CliAction::Continue));
            }
            let task_id = match arg.parse::<u64>() {
                Ok(value) => value,
                Err(_) => {
                    println!("task_id 必须是整数");
                    return Ok(Some(CliAction::Continue));
                }
            };
            return Ok(Some(CliAction::FocusWorker { task_id }));
        }
        println!("用法: /focus lead | /focus team | /focus worker <task_id> | /focus status");
        return Ok(Some(CliAction::Continue));
    }
    if let Some(rest) = trimmed.strip_prefix("/team").map(str::trim) {
        if rest.is_empty() || rest == "status" {
            return Ok(Some(CliAction::TeamStatus));
        }
        if let Some(goal) = rest.strip_prefix("run").map(str::trim) {
            if goal.is_empty() {
                println!("用法: /team run <需求>");
                return Ok(Some(CliAction::Continue));
            }
            return Ok(Some(CliAction::TeamRun {
                goal: goal.to_string(),
            }));
        }
        if rest == "stop" {
            return Ok(Some(CliAction::TeamStop));
        }
        if let Some(arg) = rest.strip_prefix("start").map(str::trim) {
            let max_parallel = if arg.is_empty() {
                2usize
            } else {
                arg.parse::<usize>().unwrap_or(2).max(1)
            };
            return Ok(Some(CliAction::TeamStart { max_parallel }));
        }
        println!("用法: /team run <需求> | /team start [max_parallel] | /team stop | /team status");
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/tasks" {
        println!("{}", project.tasks().list_all()?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/worktrees" {
        println!("{}", project.worktrees().list_all()?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/events" {
        println!("{}", project.events().list_recent(20)?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/status" {
        println!("{}", render_activity(progress));
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/skills" {
        if skills.list().is_empty() {
            println!("没有可用 skills。");
        } else {
            println!("skills dir: {}", skills.base_dir().display());
            for skill in skills.list() {
                println!("- {}: {}", skill.name, skill.description);
            }
        }
        return Ok(Some(CliAction::Continue));
    }
    if let Some(name) = trimmed.strip_prefix("/skill ").map(str::trim) {
        if name.is_empty() {
            println!("用法: /skill <name>");
        } else {
            match skills.get(name) {
                Ok(content) => println!("{}", content),
                Err(err) => println!("错误: {}", err),
            }
        }
        return Ok(Some(CliAction::Continue));
    }
    if let Some(name) = trimmed.strip_prefix("/skill-tool-init ").map(str::trim) {
        if name.is_empty() {
            println!("用法: /skill-tool-init <name>");
        } else {
            match init_tool_skill(name) {
                Ok(path) => {
                    println!("已创建工具 skill 模板: {}", path.display());
                    return Ok(Some(CliAction::ReloadSkills));
                }
                Err(err) => println!("错误: {}", err),
            }
        }
        return Ok(Some(CliAction::Continue));
    }
    if let Some(name) = trimmed.strip_prefix("/mcp-tool-init ").map(str::trim) {
        if name.is_empty() {
            println!("用法: /mcp-tool-init <name>");
        } else {
            match init_mcp_tool(name) {
                Ok(path) => println!("已创建 MCP 工具模板: {}", path.display()),
                Err(err) => println!("错误: {}", err),
            }
        }
        return Ok(Some(CliAction::Continue));
    }

    Ok(None)
}
