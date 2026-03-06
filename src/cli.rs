use crate::activity::{ActivityHandle, render_activity};
use crate::project_tools::ProjectContext;
use crate::skills::SkillRegistry;

pub enum CliAction {
    Continue,
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

    Ok(None)
}
