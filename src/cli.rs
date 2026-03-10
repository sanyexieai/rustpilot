use crate::activity::{ActivityHandle, render_activity};
use crate::mcp::init_mcp_tool;
use crate::project_tools::ProjectContext;
use crate::skills::{SkillRegistry, init_tool_skill};

pub enum CliAction {
    Continue,
    ReloadSkills,
    FocusLead,
    FocusTeam,
    FocusWorker {
        task_id: u64,
    },
    FocusStatus,
    ReplyTask {
        task_id: u64,
        content: String,
    },
    TeamRun {
        goal: String,
        priority: String,
    },
    TeamStart {
        max_parallel: usize,
    },
    TeamStop,
    TeamStatus,
    Residents,
    ResidentSend {
        agent_id: String,
        msg_type: String,
        content: String,
    },
    PolicyOverview,
    PolicyTask {
        task_id: u64,
    },
    PolicyAgent {
        agent_id: String,
    },
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
    if let Some(rest) = trimmed.strip_prefix("/reply").map(str::trim) {
        let mut parts = rest.splitn(2, ' ');
        let Some(id_raw) = parts.next() else {
            println!("用法: /reply <task_id> <补充信息>");
            return Ok(Some(CliAction::Continue));
        };
        let Some(content) = parts.next().map(str::trim) else {
            println!("用法: /reply <task_id> <补充信息>");
            return Ok(Some(CliAction::Continue));
        };
        if content.is_empty() {
            println!("用法: /reply <task_id> <补充信息>");
            return Ok(Some(CliAction::Continue));
        }
        let task_id = match id_raw.parse::<u64>() {
            Ok(value) => value,
            Err(_) => {
                println!("task_id 必须是整数");
                return Ok(Some(CliAction::Continue));
            }
        };
        return Ok(Some(CliAction::ReplyTask {
            task_id,
            content: content.to_string(),
        }));
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
            let mut parts = goal.splitn(2, ' ');
            let first = parts.next().unwrap_or_default();
            let second = parts.next().map(str::trim).unwrap_or_default();
            let (priority, goal) = if is_task_priority(first) && !second.is_empty() {
                (first.to_string(), second.to_string())
            } else {
                ("medium".to_string(), goal.to_string())
            };
            return Ok(Some(CliAction::TeamRun { goal, priority }));
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
    if trimmed == "/agents" {
        println!(
            "{}",
            project.agents().list_all_with_budgets(project.budgets())?
        );
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/residents" {
        return Ok(Some(CliAction::Residents));
    }
    if let Some(rest) = trimmed.strip_prefix("/resident ").map(str::trim) {
        let mut parts = rest.splitn(3, ' ');
        let Some(subcmd) = parts.next() else {
            println!("usage: /resident send <agent_id> <message>");
            return Ok(Some(CliAction::Continue));
        };
        if subcmd != "send" {
            println!("usage: /resident send <agent_id> <message>");
            return Ok(Some(CliAction::Continue));
        }
        let Some(agent_id) = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            println!("usage: /resident send <agent_id> <message>");
            return Ok(Some(CliAction::Continue));
        };
        let Some(content) = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            println!("usage: /resident send <agent_id> <message>");
            return Ok(Some(CliAction::Continue));
        };
        return Ok(Some(CliAction::ResidentSend {
            agent_id: agent_id.to_string(),
            msg_type: "message".to_string(),
            content: content.to_string(),
        }));
    }
    if let Some(content) = trimmed.strip_prefix("/concierge ").map(str::trim) {
        if content.is_empty() {
            println!("usage: /concierge <message>");
            return Ok(Some(CliAction::Continue));
        }
        return Ok(Some(CliAction::ResidentSend {
            agent_id: "concierge".to_string(),
            msg_type: "user.request".to_string(),
            content: content.to_string(),
        }));
    }
    if let Some(content) = trimmed.strip_prefix("/ui ").map(str::trim) {
        if content.is_empty() {
            println!("usage: /ui <message>");
            return Ok(Some(CliAction::Continue));
        }
        return Ok(Some(CliAction::ResidentSend {
            agent_id: "ui".to_string(),
            msg_type: "ui.request".to_string(),
            content: content.to_string(),
        }));
    }
    if let Some(content) = trimmed.strip_prefix("/reviewer ").map(str::trim) {
        if content.is_empty() {
            println!("usage: /reviewer <message>");
            return Ok(Some(CliAction::Continue));
        }
        return Ok(Some(CliAction::ResidentSend {
            agent_id: "reviewer".to_string(),
            msg_type: "proposal.request".to_string(),
            content: content.to_string(),
        }));
    }
    if trimmed == "/worktrees" {
        println!("{}", project.worktrees().list_all()?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/events" {
        println!("{}", project.events().list_recent(20)?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/reflections" {
        println!("{}", project.reflections().list_recent(20)?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/decisions" {
        println!("{}", project.decisions().list_recent(20)?);
        return Ok(Some(CliAction::Continue));
    }
    if trimmed == "/proposals" {
        println!("{}", project.proposals().list_recent(20)?);
        return Ok(Some(CliAction::Continue));
    }
    if let Some(rest) = trimmed.strip_prefix("/policy").map(str::trim) {
        if rest.is_empty() {
            return Ok(Some(CliAction::PolicyOverview));
        }
        if let Some(arg) = rest.strip_prefix("task").map(str::trim) {
            if arg.is_empty() {
                println!("用法: /policy task <task_id>");
                return Ok(Some(CliAction::Continue));
            }
            let task_id = match arg.parse::<u64>() {
                Ok(value) => value,
                Err(_) => {
                    println!("task_id 必须是整数");
                    return Ok(Some(CliAction::Continue));
                }
            };
            return Ok(Some(CliAction::PolicyTask { task_id }));
        }
        if let Some(arg) = rest.strip_prefix("agent").map(str::trim) {
            if arg.is_empty() {
                println!("用法: /policy agent <agent_id>");
                return Ok(Some(CliAction::Continue));
            }
            return Ok(Some(CliAction::PolicyAgent {
                agent_id: arg.to_string(),
            }));
        }
        println!("用法: /policy | /policy task <task_id> | /policy agent <agent_id>");
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

fn is_task_priority(value: &str) -> bool {
    matches!(value, "critical" | "high" | "medium" | "low")
}
