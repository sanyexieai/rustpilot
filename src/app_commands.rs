use crate::activity::ActivityHandle;
use crate::app_support::{InteractionMode, parse_priority_prefixed_goal};
use crate::cli::CliAction;
use crate::config::LlmConfig;
use crate::openai_compat::Message;
use crate::project_tools::ProjectContext;
use crate::resident_agents::AgentSupervisor;
use crate::runtime::lead::{
    estimate_text_tokens, looks_like_question, maybe_reflect_energy, run_lead_turn_with_recovery,
};
use crate::runtime::policy::{
    render_policy_agent_text, render_policy_overview_text, render_policy_task_text,
};
use crate::runtime::residents::{render_residents_status, render_team_status};
use crate::runtime::team::{
    focus_lead, focus_team, focus_worker, reply_task, resident_send, team_run, team_start,
    team_stop,
};
use crate::skills::SkillRegistry;
use crate::team::send_input_to_worker;
use crate::wire::WireFrame;
use std::path::PathBuf;

pub(crate) enum LoopDirective {
    Continue,
    Exit,
}

pub(crate) struct CommandOutcome {
    pub(crate) directive: LoopDirective,
    pub(crate) frames: Vec<WireFrame>,
}

pub(crate) async fn process_cli_action(
    action: CliAction,
    repo_root: &PathBuf,
    project: &ProjectContext,
    supervisor: &mut AgentSupervisor,
    skills: &mut SkillRegistry,
    interaction_mode: &mut InteractionMode,
    auto_team_max_parallel: usize,
) -> anyhow::Result<CommandOutcome> {
    let mut frames = Vec::new();
    match action {
        CliAction::Exit => {
            frames.push(WireFrame::ack("exit"));
            return Ok(CommandOutcome {
                directive: LoopDirective::Exit,
                frames,
            });
        }
        CliAction::ReloadSkills => {
            *skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
            frames.push(WireFrame::ack("skills reloaded"));
        }
        CliAction::FocusLead => {
            focus_lead(project, interaction_mode);
            frames.push(WireFrame::session_updated("lead", "active"));
            frames.push(WireFrame::ack("focus: lead"));
        }
        CliAction::FocusTeam => {
            focus_team(project, interaction_mode);
            frames.push(WireFrame::session_updated("team", "idle"));
            frames.push(WireFrame::ack("focus: team"));
        }
        CliAction::FocusWorker { task_id } => {
            let message = focus_worker(repo_root, project, interaction_mode, task_id)?;
            frames.push(WireFrame::session_updated(interaction_mode.label(), "active"));
            frames.push(WireFrame::ack(message));
        }
        CliAction::FocusStatus => {
            frames.push(WireFrame::ack(format!(
                "current focus: {}",
                interaction_mode.label()
            )));
        }
        CliAction::ReplyTask { task_id, content } => {
            frames.push(WireFrame::ack(reply_task(
                repo_root,
                project,
                supervisor,
                task_id,
                &content,
            )?));
        }
        CliAction::TeamRun { goal, priority } => {
            frames.push(WireFrame::ack(team_run(project, supervisor, &goal, &priority)?));
        }
        CliAction::TeamStart { .. } => {
            frames.push(WireFrame::ack(team_start(supervisor)?));
        }
        CliAction::TeamStop => {
            frames.push(WireFrame::ack(team_stop(supervisor)));
        }
        CliAction::TeamStatus => {
            frames.push(WireFrame::ack(render_team_status(project, supervisor)?));
        }
        CliAction::Residents => {
            frames.push(WireFrame::ack(render_residents_status(project, supervisor)?));
        }
        CliAction::ResidentSend {
            agent_id,
            msg_type,
            content,
        } => {
            frames.push(WireFrame::ack(resident_send(
                project,
                supervisor,
                &agent_id,
                &msg_type,
                &content,
            )?));
        }
        CliAction::PolicyOverview => {
            frames.push(WireFrame::ack(render_policy_overview_text(
                project,
                auto_team_max_parallel,
            )));
        }
        CliAction::PolicyTask { task_id } => match render_policy_task_text(
            project,
            task_id,
            auto_team_max_parallel,
        ) {
            Ok(text) => frames.push(WireFrame::ack(text)),
            Err(err) => frames.push(WireFrame::error(format!(
                "failed to read task policy: {}",
                err
            ))),
        },
        CliAction::PolicyAgent { agent_id } => match render_policy_agent_text(project, &agent_id) {
            Ok(text) => frames.push(WireFrame::ack(text)),
            Err(err) => frames.push(WireFrame::error(format!(
                "failed to read agent policy: {}",
                err
            ))),
        },
        CliAction::Continue => {}
    }

    Ok(CommandOutcome {
        directive: LoopDirective::Continue,
        frames,
    })
}

pub(crate) async fn process_user_input(
    trimmed: &str,
    repo_root: &PathBuf,
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &ActivityHandle,
    supervisor: &mut AgentSupervisor,
    lead_cursor: &mut usize,
    interaction_mode: &InteractionMode,
) -> anyhow::Result<CommandOutcome> {
    let mut frames = Vec::new();
    if trimmed.is_empty() {
        return Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames,
        });
    }

    if let Some(prompt) = trimmed.strip_prefix("/ask ").map(str::trim) {
        if prompt.is_empty() {
            frames.push(WireFrame::error("usage: /ask <content>"));
            return Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames,
            });
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
        maybe_reflect_energy(project, "lead", "user.ask", None, "processed /ask input");
        run_lead_turn_with_recovery(
            client,
            llm,
            project,
            messages,
            progress,
            supervisor,
            lead_cursor,
            interaction_mode,
            prompt,
        )
        .await?;
        frames.push(WireFrame::ack("/ask completed"));
        return Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames,
        });
    }

    if !trimmed.starts_with('/') {
        match interaction_mode {
            InteractionMode::TeamQueue => {
                if looks_like_question(trimmed) {
                    let _ = project
                        .budgets()
                        .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(40));
                    maybe_reflect_energy(
                        project,
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
                    run_lead_turn_with_recovery(
                        client,
                        llm,
                        project,
                        messages,
                        progress,
                        supervisor,
                        lead_cursor,
                        interaction_mode,
                        trimmed,
                    )
                    .await?;
                    frames.push(WireFrame::ack("lead question handled"));
                    return Ok(CommandOutcome {
                        directive: LoopDirective::Continue,
                        frames,
                    });
                }
                let (priority, goal) = parse_priority_prefixed_goal(trimmed);
                let _ = project
                    .budgets()
                    .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(20));
                maybe_reflect_energy(
                    project,
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
                frames.push(WireFrame::ack(format!("forwarded to concierge: {}", payload)));
            }
            InteractionMode::Lead => {
                let _ = project
                    .budgets()
                    .record_usage("lead", estimate_text_tokens(trimmed).saturating_add(40));
                maybe_reflect_energy(project, "lead", "lead.message", None, "processed lead input");
                messages.push(Message {
                    role: "user".to_string(),
                    content: Some(trimmed.to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                });
                run_lead_turn_with_recovery(
                    client,
                    llm,
                    project,
                    messages,
                    progress,
                    supervisor,
                    lead_cursor,
                    interaction_mode,
                    trimmed,
                )
                .await?;
                frames.push(WireFrame::ack("lead input handled"));
            }
            InteractionMode::Worker { task_id } => match send_input_to_worker(repo_root, *task_id, trimmed) {
                Ok(text) => frames.push(WireFrame::ack(text)),
                Err(err) => frames.push(WireFrame::error(format!(
                    "failed to route to worker: {}",
                    err
                ))),
            },
        }
        return Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames,
        });
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
    maybe_reflect_energy(project, "lead", "command", None, "processed command");
    run_lead_turn_with_recovery(
        client,
        llm,
        project,
        messages,
        progress,
        supervisor,
        lead_cursor,
        interaction_mode,
        trimmed,
    )
    .await?;
    frames.push(WireFrame::ack("command processed"));

    Ok(CommandOutcome {
        directive: LoopDirective::Continue,
        frames,
    })
}
