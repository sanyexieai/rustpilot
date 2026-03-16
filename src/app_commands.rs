use crate::abort_control::{abort_session, has_active_request};
use crate::activity::ActivityHandle;
use crate::app_support::{
    InteractionMode, current_agent_id, open_browser, parse_interaction_mode_label,
    parse_priority_prefixed_goal, parse_ui_intent, ui_base_url,
};
use crate::cli::CliAction;
use crate::config::LlmConfig;
use crate::external_tools::import_external_tool;
use crate::openai_compat::Message;
use crate::project_tools::{ApprovalMode, ProjectContext};
use crate::resident_agents::AgentSupervisor;
use crate::runtime::approval::render_approval_status;
use crate::runtime::lead::{
    estimate_text_tokens, looks_like_question, maybe_reflect_energy, run_root_turn_with_recovery,
};
use crate::runtime::policy::{
    render_policy_agent_text, render_policy_overview_text, render_policy_task_text,
};
use crate::runtime::residents::{render_residents_status, render_team_status};
use crate::runtime::team::{
    control_task, focus_lead, focus_shell, focus_team, focus_worker, render_task_tree,
    reply_task, resident_send, team_run, team_start, team_stop,
};
use crate::runtime::usage::render_usage_text;
use crate::resident_agents::{restart_launch, stop_launch};
use crate::shell_file_tools::{is_dangerous_command, run_shell_command};
use crate::skills::SkillRegistry;
use crate::team::send_input_to_worker;
use crate::wire::WireFrame;
use std::path::Path;

pub(crate) enum LoopDirective {
    Continue,
    Exit,
}

pub(crate) struct CommandOutcome {
    pub(crate) directive: LoopDirective,
    pub(crate) frames: Vec<WireFrame>,
}

pub(crate) struct AppRuntime<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) repo_root: &'a Path,
    pub(crate) client: &'a reqwest::Client,
    pub(crate) llm: &'a LlmConfig,
    pub(crate) project: &'a ProjectContext,
    pub(crate) messages: &'a mut Vec<Message>,
    pub(crate) progress: &'a ActivityHandle,
    pub(crate) supervisor: &'a mut AgentSupervisor,
    pub(crate) lead_cursor: &'a mut usize,
    pub(crate) interaction_mode: &'a InteractionMode,
}

pub(crate) struct CliRuntime<'a> {
    pub(crate) repo_root: &'a Path,
    pub(crate) project: &'a ProjectContext,
    pub(crate) supervisor: &'a mut AgentSupervisor,
    pub(crate) skills: &'a mut SkillRegistry,
    pub(crate) current_session_id: &'a mut String,
    pub(crate) current_session_label: &'a mut Option<String>,
    pub(crate) messages: &'a mut Vec<Message>,
    pub(crate) system_prompt: &'a str,
    pub(crate) interaction_mode: &'a mut InteractionMode,
    pub(crate) auto_team_max_parallel: usize,
}

pub(crate) async fn process_cli_action(
    action: CliAction,
    runtime: CliRuntime<'_>,
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
        CliAction::Abort => {
            let message = if abort_session(runtime.current_session_id) {
                "chat abort requested"
            } else {
                "no active chat request to abort"
            };
            frames.push(WireFrame::ack(message));
        }
        CliAction::ReloadSkills => {
            *runtime.skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
            frames.push(WireFrame::ack("skills reloaded"));
        }
        CliAction::ApprovalStatus => {
            frames.push(WireFrame::ack(render_approval_status(runtime.project)?));
        }
        CliAction::ApprovalHistory { limit, reason } => {
            let items = runtime
                .project
                .approval()
                .list_recent_blocks(limit, reason.as_deref())?;
            if items.is_empty() {
                frames.push(WireFrame::ack("approval history: none"));
            } else {
                frames.push(WireFrame::ack(
                    items
                        .into_iter()
                        .map(|item| {
                            format!(
                                "- ts={} actor={} reason={} tool={} command={}",
                                item.ts,
                                item.actor_id,
                                item.reason_code,
                                item.tool_name,
                                item.command
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ));
            }
        }
        CliAction::ApprovalSet { mode } => {
            let mode = match mode.as_str() {
                "auto" => ApprovalMode::Auto,
                "read_only" => ApprovalMode::ReadOnly,
                "manual" => ApprovalMode::Manual,
                other => anyhow::bail!("unknown approval mode: {}", other),
            };
            runtime.project.approval().set_mode(mode)?;
            frames.push(WireFrame::ack(render_approval_status(runtime.project)?));
        }
        CliAction::SessionList => {
            let sessions = runtime.project.sessions().list()?;
            if sessions.is_empty() {
                frames.push(WireFrame::ack("no sessions"));
            } else {
                frames.push(WireFrame::ack(
                    sessions
                        .into_iter()
                        .map(|item| {
                            let marker = if item.session_id == *runtime.current_session_id {
                                "*"
                            } else {
                                "-"
                            };
                            format!(
                                "{} {} label={} focus={} status={} abortable={}",
                                marker,
                                item.session_id,
                                item.label.unwrap_or_else(|| "-".to_string()),
                                item.focus,
                                item.status,
                                if has_active_request(&item.session_id) {
                                    "yes"
                                } else {
                                    "no"
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ));
            }
        }
        CliAction::SessionCurrent => {
            frames.push(WireFrame::ack(format!(
                "current session: {} label={} focus={} abortable={}",
                runtime.current_session_id,
                runtime
                    .current_session_label
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
                runtime.interaction_mode.label(),
                if has_active_request(runtime.current_session_id) {
                    "yes"
                } else {
                    "no"
                }
            )));
        }
        CliAction::SessionNew { label, focus } => {
            let interaction_mode = focus
                .as_deref()
                .map(parse_interaction_mode_label)
                .transpose()?
                .unwrap_or_else(|| runtime.interaction_mode.clone());
            let status = session_status_for_mode(&interaction_mode);
            persist_session(
                runtime.project,
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                runtime.messages,
            )?;
            let created = runtime.project.sessions().create(
                label.as_deref(),
                &interaction_mode.label(),
                status,
            )?;
            *runtime.current_session_id = created.session_id.clone();
            *runtime.current_session_label = created.label.clone();
            *runtime.interaction_mode = interaction_mode;
            *runtime.messages = default_session_messages(runtime.system_prompt);
            persist_session(
                runtime.project,
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                runtime.messages,
            )?;
            frames.push(WireFrame::session_updated(
                runtime.interaction_mode.label(),
                status,
            ));
            frames.push(WireFrame::ack(format!(
                "created session {}",
                runtime.current_session_id
            )));
        }
        CliAction::SessionUse { session_id } => {
            persist_session(
                runtime.project,
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                runtime.messages,
            )?;
            let Some(session) = runtime.project.sessions().get(&session_id)? else {
                frames.push(WireFrame::error(format!("unknown session: {}", session_id)));
                return Ok(CommandOutcome {
                    directive: LoopDirective::Continue,
                    frames,
                });
            };
            let mut loaded = runtime
                .project
                .sessions()
                .load_messages(&session.session_id)?;
            if loaded.is_empty() {
                loaded = default_session_messages(runtime.system_prompt);
            }
            let interaction_mode = parse_interaction_mode_label(&session.focus)?;
            let status = session_status_for_mode(&interaction_mode);
            *runtime.current_session_id = session.session_id.clone();
            *runtime.current_session_label = session.label.clone();
            *runtime.interaction_mode = interaction_mode;
            *runtime.messages = loaded;
            runtime.project.sessions().update_state(
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                &runtime.interaction_mode.label(),
                status,
            )?;
            frames.push(WireFrame::session_updated(
                runtime.interaction_mode.label(),
                status,
            ));
            frames.push(WireFrame::ack(format!(
                "using session {}",
                runtime.current_session_id
            )));
        }
        CliAction::FocusLead => {
            focus_lead(runtime.project, runtime.interaction_mode);
            runtime.project.sessions().update_state(
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                &runtime.interaction_mode.label(),
                "active",
            )?;
            frames.push(WireFrame::session_updated("root", "active"));
            frames.push(WireFrame::ack("focus: root"));
        }
        CliAction::FocusShell => {
            focus_shell(runtime.project, runtime.interaction_mode);
            runtime.project.sessions().update_state(
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                &runtime.interaction_mode.label(),
                "active",
            )?;
            frames.push(WireFrame::session_updated("shell", "active"));
            frames.push(WireFrame::ack("focus: shell"));
        }
        CliAction::FocusTeam => {
            focus_team(runtime.project, runtime.interaction_mode);
            runtime.project.sessions().update_state(
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                &runtime.interaction_mode.label(),
                "idle",
            )?;
            frames.push(WireFrame::session_updated("team", "idle"));
            frames.push(WireFrame::ack("focus: team"));
        }
        CliAction::FocusWorker { task_id } => {
            let message = focus_worker(
                runtime.repo_root,
                runtime.project,
                runtime.interaction_mode,
                task_id,
            )?;
            runtime.project.sessions().update_state(
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                &runtime.interaction_mode.label(),
                "active",
            )?;
            frames.push(WireFrame::session_updated(
                runtime.interaction_mode.label(),
                "active",
            ));
            frames.push(WireFrame::ack(message));
        }
        CliAction::FocusStatus => {
            frames.push(WireFrame::ack(format!(
                "current focus: {} abortable={}",
                runtime.interaction_mode.label(),
                if has_active_request(runtime.current_session_id) {
                    "yes"
                } else {
                    "no"
                }
            )));
        }
        CliAction::ReplyTask { task_id, content } => {
            frames.push(WireFrame::ack(reply_task(
                runtime.repo_root,
                runtime.project,
                runtime.supervisor,
                task_id,
                &content,
            )?));
        }
        CliAction::TaskTree => {
            frames.push(WireFrame::ack(render_task_tree(runtime.project)?));
        }
        CliAction::LaunchList => {
            let launches = runtime.project.launches().list()?;
            let text = if launches.is_empty() {
                "no launches".to_string()
            } else {
                launches
                    .into_iter()
                    .map(|item| {
                        format!(
                            "- {} kind={} agent={} status={} pid={} task={} target={}",
                            item.launch_id,
                            item.kind,
                            item.agent_id,
                            item.status,
                            item.pid
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                            item.task_id
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                            if item.target.is_empty() {
                                "-".to_string()
                            } else {
                                item.target
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            frames.push(WireFrame::ack(text));
        }
        CliAction::LaunchControl { launch_id, action } => {
            let message = match action.as_str() {
                "stop" => {
                    stop_launch(runtime.project, &launch_id)?;
                    format!("stopped launch {}", launch_id)
                }
                "restart" => {
                    let restarted = restart_launch(runtime.project, &launch_id)?;
                    format!(
                        "restarted launch {} as {}",
                        launch_id, restarted.launch_id
                    )
                }
                _ => format!("unsupported launch action: {}", action),
            };
            frames.push(WireFrame::ack(message));
        }
        CliAction::LaunchLogs { launch_id, lines } => {
            let Some(launch) = runtime.project.launches().get(&launch_id)? else {
                frames.push(WireFrame::error(format!("launch not found: {}", launch_id)));
                return Ok(CommandOutcome {
                    directive: LoopDirective::Continue,
                    frames,
                });
            };
            if launch.log_path.trim().is_empty() {
                frames.push(WireFrame::error(format!(
                    "launch {} has no log path",
                    launch_id
                )));
            } else {
                let tail = crate::launch_log::read_tail(&launch.log_path, lines);
                frames.push(WireFrame::ack(if tail.trim().is_empty() {
                    format!("no log output for launch {}", launch_id)
                } else {
                    tail
                }));
            }
        }
        CliAction::TaskControl {
            task_id,
            action,
            priority,
        } => {
            frames.push(WireFrame::ack(control_task(
                runtime.project,
                runtime.supervisor,
                task_id,
                &action,
                priority.as_deref(),
            )?));
        }
        CliAction::TeamRun { goal, priority } => {
            frames.push(WireFrame::ack(team_run(
                runtime.project,
                runtime.supervisor,
                &goal,
                &priority,
            )?));
        }
        CliAction::TeamStart { .. } => {
            frames.push(WireFrame::ack(team_start(runtime.supervisor)?));
        }
        CliAction::TeamStop => {
            frames.push(WireFrame::ack(team_stop(runtime.supervisor)));
        }
        CliAction::TeamStatus => {
            frames.push(WireFrame::ack(render_team_status(
                runtime.project,
                runtime.supervisor,
            )?));
        }
        CliAction::Residents => {
            frames.push(WireFrame::ack(render_residents_status(
                runtime.project,
                runtime.supervisor,
            )?));
        }
        CliAction::ResidentSend {
            agent_id,
            msg_type,
            content,
        } => {
            frames.push(WireFrame::ack(resident_send(
                runtime.project,
                runtime.supervisor,
                &agent_id,
                &msg_type,
                &content,
            )?));
        }
        CliAction::PolicyOverview => {
            frames.push(WireFrame::ack(render_policy_overview_text(
                runtime.project,
                runtime.auto_team_max_parallel,
            )));
        }
        CliAction::PolicyTask { task_id } => {
            match render_policy_task_text(runtime.project, task_id, runtime.auto_team_max_parallel)
            {
                Ok(text) => frames.push(WireFrame::ack(text)),
                Err(err) => frames.push(WireFrame::error(format!(
                    "failed to read task policy: {}",
                    err
                ))),
            }
        }
        CliAction::PolicyAgent { agent_id } => {
            match render_policy_agent_text(runtime.project, &agent_id) {
                Ok(text) => frames.push(WireFrame::ack(text)),
                Err(err) => frames.push(WireFrame::error(format!(
                    "failed to read agent policy: {}",
                    err
                ))),
            }
        }
        CliAction::Usage => frames.push(WireFrame::ack(render_usage_text(runtime.project)?)),
        CliAction::ToolImport { source_dir } => {
            let imported = import_external_tool(Path::new(&source_dir))?;
            frames.push(WireFrame::ack(format!(
                "imported tool to {}",
                imported.display()
            )));
        }
        CliAction::ShellRun { command } => {
            if is_dangerous_command(&command) {
                frames.push(WireFrame::error(format!(
                    "refused dangerous shell command: {}",
                    command
                )));
            } else {
                let output = run_shell_command(&command, Some(runtime.project.repo_root()))?;
                frames.push(WireFrame::ack(output));
            }
        }
        CliAction::Continue => {}
    }

    Ok(CommandOutcome {
        directive: LoopDirective::Continue,
        frames,
    })
}

fn default_session_messages(system_prompt: &str) -> Vec<Message> {
    vec![Message {
        role: "system".to_string(),
        content: Some(system_prompt.to_string()),
        tool_call_id: None,
        tool_calls: None,
    }]
}

fn session_status_for_mode(interaction_mode: &InteractionMode) -> &'static str {
    match interaction_mode {
        InteractionMode::TeamQueue => "idle",
        InteractionMode::Lead | InteractionMode::Shell | InteractionMode::Worker { .. } => "active",
    }
}

fn persist_session(
    project: &ProjectContext,
    session_id: &str,
    label: Option<&str>,
    messages: &[Message],
) -> anyhow::Result<()> {
    project
        .sessions()
        .ensure_session(session_id, label, "root", "active")?;
    project.sessions().save_messages(session_id, messages)
}

pub(crate) async fn process_user_input(
    trimmed: &str,
    runtime: AppRuntime<'_>,
) -> anyhow::Result<CommandOutcome> {
    let actor_id = current_agent_id();
    let mut frames = Vec::new();
    if trimmed.is_empty() {
        return Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames,
        });
    }

    if parse_ui_intent(trimmed).is_some() {
        let url = ui_base_url(runtime.project);
        let message = match open_browser(&url) {
            Ok(()) => format!("opened management page: {}", url),
            Err(err) => format!("management page: {} (browser launch failed: {})", url, err),
        };
        frames.push(WireFrame::ack(message));
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
        runtime.messages.push(Message {
            role: "user".to_string(),
            content: Some(prompt.to_string()),
            tool_call_id: None,
            tool_calls: None,
        });
        let _ = runtime
            .project
            .budgets()
            .record_usage(&actor_id, estimate_text_tokens(prompt).saturating_add(40));
        maybe_reflect_energy(
            runtime.project,
            &actor_id,
            "user.ask",
            None,
            "processed /ask input",
        );
        run_root_turn_with_recovery(prompt, runtime).await?;
        frames.push(WireFrame::ack("/ask completed"));
        return Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames,
        });
    }

    if matches!(runtime.interaction_mode, InteractionMode::Shell) {
        return Ok(run_shell_mode_input(runtime.project, trimmed));
    }

    if !trimmed.starts_with('/') {
        match runtime.interaction_mode {
            InteractionMode::TeamQueue => {
                if looks_like_question(trimmed) {
                    let _ = runtime
                        .project
                        .budgets()
                        .record_usage(&actor_id, estimate_text_tokens(trimmed).saturating_add(40));
                    maybe_reflect_energy(
                        runtime.project,
                        &actor_id,
                        "root.message",
                        None,
                        "processed question from team focus via current parent node",
                    );
                    runtime.messages.push(Message {
                        role: "user".to_string(),
                        content: Some(trimmed.to_string()),
                        tool_call_id: None,
                        tool_calls: None,
                    });
                    run_root_turn_with_recovery(trimmed, runtime).await?;
                    frames.push(WireFrame::ack("root question handled"));
                    return Ok(CommandOutcome {
                        directive: LoopDirective::Continue,
                        frames,
                    });
                }
                let (priority, goal) = parse_priority_prefixed_goal(trimmed);
                let _ = runtime
                    .project
                    .budgets()
                    .record_usage(&actor_id, estimate_text_tokens(trimmed).saturating_add(20));
                maybe_reflect_energy(
                    runtime.project,
                    &actor_id,
                    "task.enqueue",
                    None,
                    "forwarded team queue input to concierge",
                );
                let payload = format!("[{}] {}", priority, goal);
                let _ = runtime.project.mailbox().send_typed(
                    &actor_id,
                    "concierge",
                    "user.request",
                    &payload,
                    None,
                    None,
                    false,
                    None,
                )?;
                let _ = runtime.project.decisions().append(
                    &actor_id,
                    "resident.message.sent",
                    None,
                    None,
                    "sent team queue input to concierge",
                    &format!("priority={} target=concierge", priority),
                );
                let _ = runtime.supervisor.ensure_running("concierge");
                frames.push(WireFrame::ack(format!(
                    "forwarded to concierge: {}",
                    payload
                )));
            }
            InteractionMode::Lead => {
                let _ = runtime
                    .project
                    .budgets()
                    .record_usage(&actor_id, estimate_text_tokens(trimmed).saturating_add(40));
                maybe_reflect_energy(
                    runtime.project,
                    &actor_id,
                    "root.message",
                    None,
                    "processed root input",
                );
                runtime.messages.push(Message {
                    role: "user".to_string(),
                    content: Some(trimmed.to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                });
                run_root_turn_with_recovery(trimmed, runtime).await?;
                frames.push(WireFrame::ack("root input handled"));
            }
            InteractionMode::Shell => unreachable!("shell mode handled before routing"),
            InteractionMode::Worker { task_id } => {
                match send_input_to_worker(runtime.repo_root, *task_id, trimmed) {
                    Ok(text) => frames.push(WireFrame::ack(text)),
                    Err(err) => frames.push(WireFrame::error(format!(
                        "failed to route to worker: {}",
                        err
                    ))),
                }
            }
        }
        return Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames,
        });
    }

    runtime.messages.push(Message {
        role: "user".to_string(),
        content: Some(trimmed.to_string()),
        tool_call_id: None,
        tool_calls: None,
    });
    let _ = runtime
        .project
        .budgets()
        .record_usage(&actor_id, estimate_text_tokens(trimmed).saturating_add(30));
    maybe_reflect_energy(
        runtime.project,
        &actor_id,
        "command",
        None,
        "processed command",
    );
    run_root_turn_with_recovery(trimmed, runtime).await?;
    frames.push(WireFrame::ack("command processed"));

    Ok(CommandOutcome {
        directive: LoopDirective::Continue,
        frames,
    })
}

fn run_shell_mode_input(project: &ProjectContext, command: &str) -> CommandOutcome {
    let actor_id = current_agent_id();
    let mut frames = Vec::new();
    let _ = project
        .budgets()
        .record_usage(&actor_id, estimate_text_tokens(command).saturating_add(10));
    maybe_reflect_energy(
        project,
        &actor_id,
        "shell.input",
        None,
        "processed shell mode input",
    );
    if is_dangerous_command(command) {
        frames.push(WireFrame::error(format!(
            "refused dangerous shell command: {}",
            command
        )));
    } else {
        match run_shell_command(command, Some(project.repo_root())) {
            Ok(output) => frames.push(WireFrame::ack(output)),
            Err(err) => frames.push(WireFrame::error(format!("shell command failed: {}", err))),
        }
    }
    CommandOutcome {
        directive: LoopDirective::Continue,
        frames,
    }
}

#[cfg(test)]
mod tests {
    use super::{CliRuntime, process_cli_action, run_shell_mode_input};
    use crate::app_support::InteractionMode;
    use crate::cli::CliAction;
    use crate::openai_compat::Message;
    use crate::project_tools::ProjectContext;
    use crate::resident_agents::AgentSupervisor;
    use crate::skills::SkillRegistry;
    use crate::wire::{WireFrame, WireResponse};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "{}_{}_{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("time")
                    .as_nanos()
            );
            let path = std::env::temp_dir().join("rustpilot-tests").join(unique);
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn run(repo: &Path, program: &str, args: &[&str]) {
        let output = Command::new(program)
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run command");
        assert!(
            output.status.success(),
            "{} {:?} failed: {}{}",
            program,
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo(path: &Path) {
        run(path, "git", &["init"]);
        run(path, "git", &["config", "user.name", "Codex"]);
        run(path, "git", &["config", "user.email", "codex@example.com"]);
        fs::write(path.join("README.md"), "hello\n").expect("write readme");
        run(path, "git", &["add", "."]);
        run(path, "git", &["commit", "-m", "init"]);
    }

    fn project_context(repo_root: &Path) -> ProjectContext {
        ProjectContext::new(repo_root.to_path_buf()).expect("project context")
    }

    #[tokio::test]
    async fn session_new_can_override_focus() {
        let temp = TestDir::new("session_new_focus");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut skills = SkillRegistry::empty();
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut interaction_mode = InteractionMode::Lead;

        let outcome = process_cli_action(
            CliAction::SessionNew {
                label: Some("shell-session".to_string()),
                focus: Some("shell".to_string()),
            },
            CliRuntime {
                repo_root: temp.path(),
                project: &project,
                supervisor: &mut supervisor,
                skills: &mut skills,
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
                messages: &mut messages,
                system_prompt: "system",
                interaction_mode: &mut interaction_mode,
                auto_team_max_parallel: 1,
            },
        )
        .await
        .expect("process action");

        assert!(matches!(interaction_mode, InteractionMode::Shell));
        assert_eq!(current_session_label.as_deref(), Some("shell-session"));
        match outcome.frames.first() {
            Some(WireFrame::Event { event }) => match &event.payload {
                crate::wire::WireEvent::SessionUpdated { focus, status, .. } => {
                    assert_eq!(focus, "shell");
                    assert_eq!(status, "active");
                }
                other => panic!("unexpected event: {:?}", other),
            },
            other => panic!("unexpected frame: {:?}", other),
        }
    }

    #[tokio::test]
    async fn session_use_switches_to_target_focus() {
        let temp = TestDir::new("session_use_focus");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        let created = project
            .sessions()
            .create(Some("shell-session"), "shell", "idle")
            .expect("create session");
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut skills = SkillRegistry::empty();
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut interaction_mode = InteractionMode::Lead;

        let outcome = process_cli_action(
            CliAction::SessionUse {
                session_id: created.session_id,
            },
            CliRuntime {
                repo_root: temp.path(),
                project: &project,
                supervisor: &mut supervisor,
                skills: &mut skills,
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
                messages: &mut messages,
                system_prompt: "system",
                interaction_mode: &mut interaction_mode,
                auto_team_max_parallel: 1,
            },
        )
        .await
        .expect("process action");

        assert!(matches!(interaction_mode, InteractionMode::Shell));
        match outcome.frames.first() {
            Some(WireFrame::Event { event }) => match &event.payload {
                crate::wire::WireEvent::SessionUpdated { focus, status, .. } => {
                    assert_eq!(focus, "shell");
                    assert_eq!(status, "active");
                }
                other => panic!("unexpected event: {:?}", other),
            },
            other => panic!("unexpected frame: {:?}", other),
        }
    }

    #[tokio::test]
    async fn cli_abort_reports_when_no_active_request_exists() {
        let temp = TestDir::new("cli_abort_idle");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut skills = SkillRegistry::empty();
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut interaction_mode = InteractionMode::Lead;

        let outcome = process_cli_action(
            CliAction::Abort,
            CliRuntime {
                repo_root: temp.path(),
                project: &project,
                supervisor: &mut supervisor,
                skills: &mut skills,
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
                messages: &mut messages,
                system_prompt: "system",
                interaction_mode: &mut interaction_mode,
                auto_team_max_parallel: 1,
            },
        )
        .await
        .expect("process action");

        match outcome.frames.first() {
            Some(WireFrame::Response { response }) => match &response.payload {
                WireResponse::Ack { message } => {
                    assert_eq!(message, "no active chat request to abort")
                }
                other => panic!("unexpected response: {:?}", other),
            },
            other => panic!("unexpected frame: {:?}", other),
        }
    }

    #[test]
    fn shell_mode_runs_safe_command() {
        let temp = TestDir::new("shell_mode_safe");
        init_git_repo(temp.path());
        let project = project_context(temp.path());

        let outcome = run_shell_mode_input(&project, "git status");

        match outcome.frames.first() {
            Some(WireFrame::Response { response }) => match &response.payload {
                WireResponse::Ack { message } => assert!(message.contains("On branch")),
                other => panic!("unexpected response: {:?}", other),
            },
            other => panic!("unexpected frame: {:?}", other),
        }
    }

    #[test]
    fn shell_mode_rejects_dangerous_command() {
        let temp = TestDir::new("shell_mode_dangerous");
        init_git_repo(temp.path());
        let project = project_context(temp.path());

        let outcome = run_shell_mode_input(&project, "Remove-Item -Recurse -Force .");

        match outcome.frames.first() {
            Some(WireFrame::Event { event }) => match &event.payload {
                crate::wire::WireEvent::Error { message } => {
                    assert!(message.contains("refused dangerous shell command"))
                }
                _ => panic!("unexpected event payload"),
            },
            _ => panic!("unexpected frame"),
        }
    }
}
