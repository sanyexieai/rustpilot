use crate::abort_control::{abort_session, has_active_request};
use crate::activity::ActivityHandle;
use crate::agent::{handle_tool_call, tool_summaries};
use crate::app_commands::{CommandOutcome, LoopDirective, process_user_input};
use crate::app_support::{InteractionMode, parse_interaction_mode_label};
use crate::config::LlmConfig;
use crate::openai_compat::Message;
use crate::openai_compat::{ToolCall, ToolCallFunction};
use crate::project_tools::{ApprovalMode, ProjectContext, SessionManager};
use crate::resident_agents::AgentSupervisor;
use crate::runtime::approval::{approval_mode_name, approval_mode_summary};
use crate::runtime::lead::looks_like_question;
use crate::wire::{
    WireApprovalBlock, WireEnvelope, WireFrame, WireRequest, WireResponse, WireSessionSummary,
    WireToolSummary,
};
use std::path::Path;

pub(crate) struct WireExecContext<'a> {
    pub(crate) repo_root: &'a Path,
    pub(crate) session_id: String,
    pub(crate) label: Option<String>,
    pub(crate) focus: String,
    pub(crate) status: String,
    pub(crate) source_id: String,
    pub(crate) decision_action: Option<String>,
}

impl<'a> WireExecContext<'a> {
    fn current_session_summary(&self) -> WireSessionSummary {
        WireSessionSummary {
            session_id: self.session_id.clone(),
            label: self.label.clone(),
            focus: Some(self.focus.clone()),
            status: Some(self.status.clone()),
            abortable: Some(has_active_request(&self.session_id)),
        }
    }

    fn session_list(&self) -> WireResponse {
        WireResponse::SessionList {
            sessions: vec![self.current_session_summary()],
        }
    }

    fn tool_result(&self, name: &str, output: String) -> WireResponse {
        WireResponse::ToolResult {
            name: name.to_string(),
            output,
        }
    }

    fn tool_list(&self, tools: Vec<WireToolSummary>) -> WireResponse {
        WireResponse::ToolList { tools }
    }

    fn tool_error(&self, name: &str, message: String) -> WireResponse {
        WireResponse::Error {
            message: format!("tool {} failed: {}", name, message),
        }
    }
}

fn resolve_session_create_mode(
    focus: Option<&str>,
    fallback: &InteractionMode,
) -> anyhow::Result<InteractionMode> {
    focus
        .filter(|value| !value.trim().is_empty())
        .map(parse_interaction_mode_label)
        .transpose()?
        .map_or_else(|| Ok(fallback.clone()), Ok)
}

fn build_tool_call(name: &str, arguments_json: String) -> ToolCall {
    ToolCall {
        id: format!("wire-{}", name),
        r#type: "function".to_string(),
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: arguments_json,
        },
    }
}

pub(crate) struct WireRuntime<'a> {
    pub(crate) repo_root: &'a Path,
    pub(crate) client: &'a reqwest::Client,
    pub(crate) llm: &'a LlmConfig,
    pub(crate) project: &'a ProjectContext,
    pub(crate) messages: &'a mut Vec<Message>,
    pub(crate) progress: &'a ActivityHandle,
    pub(crate) supervisor: &'a mut AgentSupervisor,
    pub(crate) lead_cursor: &'a mut usize,
    pub(crate) interaction_mode: &'a InteractionMode,
    pub(crate) sessions: &'a SessionManager,
    pub(crate) current_session_id: &'a mut String,
    pub(crate) current_session_label: &'a mut Option<String>,
}

pub(crate) async fn execute_wire_request(
    request: WireRequest,
    runtime: WireRuntime<'_>,
) -> anyhow::Result<CommandOutcome> {
    let context = WireExecContext {
        repo_root: runtime.repo_root,
        session_id: runtime.current_session_id.clone(),
        label: runtime.current_session_label.clone(),
        focus: runtime.interaction_mode.label(),
        status: "active".to_string(),
        source_id: "cli".to_string(),
        decision_action: None,
    };
    match request {
        WireRequest::ChatSend { input, focus } => {
            let focus_override = focus
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(parse_interaction_mode_label)
                .transpose()?;
            let interaction_mode = focus_override.as_ref().unwrap_or(runtime.interaction_mode);
            let focus_label = interaction_mode.label();
            let session_status = session_status_for_mode(interaction_mode);
            let mut outcome = process_user_input(
                &input,
                crate::app_commands::AppRuntime {
                    session_id: runtime.current_session_id,
                    repo_root: runtime.repo_root,
                    client: runtime.client,
                    llm: runtime.llm,
                    project: runtime.project,
                    messages: runtime.messages,
                    progress: runtime.progress,
                    supervisor: runtime.supervisor,
                    lead_cursor: runtime.lead_cursor,
                    interaction_mode,
                },
            )
            .await?;
            let mut frames = Vec::with_capacity(outcome.frames.len() + 2);
            frames.push(WireFrame::session_updated_with_abortable(
                focus_label.clone(),
                session_status,
                Some(will_start_abortable_request(&input, interaction_mode)),
            ));
            frames.append(&mut outcome.frames);
            frames.push(WireFrame::session_updated_with_abortable(
                focus_label,
                session_status_for_mode(interaction_mode),
                Some(has_active_request(runtime.current_session_id)),
            ));
            outcome.frames = frames;
            Ok(outcome)
        }
        WireRequest::ChatAbort => {
            let aborted = abort_session(runtime.current_session_id);
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![
                    WireFrame::ack(if aborted {
                        "chat abort requested"
                    } else {
                        "no active chat request to abort"
                    }),
                    WireFrame::session_updated_with_abortable(
                        runtime.interaction_mode.label(),
                        session_status_for_mode(runtime.interaction_mode),
                        Some(has_active_request(runtime.current_session_id)),
                    ),
                ],
            })
        }
        WireRequest::SessionCreate { label, focus } => {
            let interaction_mode =
                resolve_session_create_mode(focus.as_deref(), runtime.interaction_mode)?;
            let created_focus = interaction_mode.label();
            let status = session_status_for_mode(&interaction_mode).to_string();
            let created = runtime
                .sessions
                .create(label.as_deref(), &created_focus, &status)?;
            *runtime.current_session_id = created.session_id.clone();
            *runtime.current_session_label = created.label.clone();
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![
                    WireFrame::Response {
                        response: WireEnvelope::new(
                            "response",
                            WireResponse::SessionCreated {
                                session_id: created.session_id,
                                label: created.label,
                                session: Some(WireSessionSummary {
                                    session_id: runtime.current_session_id.clone(),
                                    label: runtime.current_session_label.clone(),
                                    focus: Some(created_focus.clone()),
                                    status: Some(status.clone()),
                                    abortable: Some(has_active_request(runtime.current_session_id)),
                                }),
                            },
                        ),
                    },
                    WireFrame::session_updated_with_abortable(
                        created_focus,
                        status,
                        Some(has_active_request(runtime.current_session_id)),
                    ),
                ],
            })
        }
        WireRequest::SessionUse { session_id } => {
            let Some(session) = runtime.sessions.get(&session_id)? else {
                return Ok(CommandOutcome {
                    directive: LoopDirective::Continue,
                    frames: vec![WireFrame::Response {
                        response: WireEnvelope::new(
                            "response",
                            WireResponse::Error {
                                message: format!("unknown session: {}", session_id),
                            },
                        ),
                    }],
                });
            };
            let mut loaded = runtime.sessions.load_messages(&session.session_id)?;
            let session_focus = session.focus.clone();
            if loaded.is_empty() {
                loaded.push(Message {
                    role: "system".to_string(),
                    content: Some(String::new()),
                    tool_call_id: None,
                    tool_calls: None,
                });
            }
            *runtime.messages = loaded;
            *runtime.current_session_id = session.session_id.clone();
            *runtime.current_session_label = session.label.clone();
            runtime.sessions.update_state(
                runtime.current_session_id,
                runtime.current_session_label.as_deref(),
                &session_focus,
                "active",
            )?;
            let status = "active".to_string();
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![
                    WireFrame::Response {
                        response: WireEnvelope::new(
                            "response",
                            WireResponse::SessionCreated {
                                session_id: runtime.current_session_id.clone(),
                                label: runtime.current_session_label.clone(),
                                session: Some(WireSessionSummary {
                                    session_id: runtime.current_session_id.clone(),
                                    label: runtime.current_session_label.clone(),
                                    focus: Some(session_focus.clone()),
                                    status: Some(status.clone()),
                                    abortable: Some(has_active_request(runtime.current_session_id)),
                                }),
                            },
                        ),
                    },
                    WireFrame::session_updated_with_abortable(
                        session_focus,
                        status,
                        Some(has_active_request(runtime.current_session_id)),
                    ),
                ],
            })
        }
        WireRequest::SessionList => {
            let sessions = runtime
                .sessions
                .list()?
                .into_iter()
                .map(|item| {
                    let abortable = has_active_request(&item.session_id);
                    WireSessionSummary {
                        session_id: item.session_id,
                        label: item.label,
                        focus: Some(item.focus),
                        status: Some(item.status),
                        abortable: Some(abortable),
                    }
                })
                .collect::<Vec<_>>();
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![WireFrame::Response {
                    response: WireEnvelope::new("response", WireResponse::SessionList { sessions }),
                }],
            })
        }
        WireRequest::ApprovalStatus => {
            let policy = runtime.project.approval().get_policy()?;
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![WireFrame::Response {
                    response: WireEnvelope::new(
                        "response",
                        WireResponse::ApprovalStatus {
                            mode: approval_mode_name(policy.mode).to_string(),
                            summary: approval_mode_summary(policy.mode).to_string(),
                            allowed_tools: crate::runtime::approval::approval_allowed_tools(
                                policy.mode,
                            )
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                            last_block: policy.last_block.as_ref().map(wire_approval_block),
                        },
                    ),
                }],
            })
        }
        WireRequest::ApprovalHistory { limit, reason } => {
            let items = runtime
                .project
                .approval()
                .list_recent_blocks(limit.unwrap_or(10), reason.as_deref())?
                .into_iter()
                .map(|item| wire_approval_block(&item))
                .collect::<Vec<_>>();
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![WireFrame::Response {
                    response: WireEnvelope::new(
                        "response",
                        WireResponse::ApprovalHistory { items },
                    ),
                }],
            })
        }
        WireRequest::ApprovalSet { mode } => {
            let mode = match mode.as_str() {
                "auto" => ApprovalMode::Auto,
                "read_only" => ApprovalMode::ReadOnly,
                "manual" => ApprovalMode::Manual,
                _ => {
                    return Ok(CommandOutcome {
                        directive: LoopDirective::Continue,
                        frames: vec![WireFrame::Response {
                            response: WireEnvelope::new(
                                "response",
                                WireResponse::Error {
                                    message: format!("unknown approval mode: {}", mode),
                                },
                            ),
                        }],
                    });
                }
            };
            let updated = runtime.project.approval().set_mode(mode)?;
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![WireFrame::Response {
                    response: WireEnvelope::new(
                        "response",
                        WireResponse::ApprovalStatus {
                            mode: approval_mode_name(updated.mode).to_string(),
                            summary: approval_mode_summary(updated.mode).to_string(),
                            allowed_tools: crate::runtime::approval::approval_allowed_tools(
                                updated.mode,
                            )
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                            last_block: updated.last_block.as_ref().map(wire_approval_block),
                        },
                    ),
                }],
            })
        }
        WireRequest::ToolList => Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames: vec![WireFrame::Response {
                response: WireEnvelope::new("response", context.tool_list(tool_summaries())),
            }],
        }),
        WireRequest::ToolCall {
            name,
            arguments_json,
        } => {
            let call = build_tool_call(&name, arguments_json);
            let response = match handle_tool_call(runtime.project, &call) {
                Ok(output) => context.tool_result(&name, output),
                Err(err) => context.tool_error(&name, err.to_string()),
            };
            Ok(CommandOutcome {
                directive: LoopDirective::Continue,
                frames: vec![WireFrame::Response {
                    response: WireEnvelope::new("response", response),
                }],
            })
        }
    }
}

fn session_status_for_mode(interaction_mode: &InteractionMode) -> &'static str {
    match interaction_mode {
        InteractionMode::TeamQueue => "idle",
        InteractionMode::Lead | InteractionMode::Shell | InteractionMode::Worker { .. } => "active",
    }
}

fn will_start_abortable_request(input: &str, interaction_mode: &InteractionMode) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some(prompt) = trimmed.strip_prefix("/ask").map(str::trim) {
        return !prompt.is_empty();
    }
    match interaction_mode {
        InteractionMode::Shell | InteractionMode::Worker { .. } => false,
        InteractionMode::Lead => true,
        InteractionMode::TeamQueue => {
            if !trimmed.starts_with('/') {
                looks_like_question(trimmed)
            } else {
                true
            }
        }
    }
}

pub(crate) fn execute_ui_wire_request(
    request: WireRequest,
    repo_root: &Path,
    agent_id: &str,
    decision_action: &str,
) -> anyhow::Result<WireResponse> {
    let context = WireExecContext {
        repo_root,
        session_id: format!("ui-{}", agent_id),
        label: Some("ui-server".to_string()),
        focus: agent_id.to_string(),
        status: "active".to_string(),
        source_id: "ui-web".to_string(),
        decision_action: Some(decision_action.to_string()),
    };
    match request {
        WireRequest::ChatSend { input, focus } => {
            let message = input.trim();
            if message.is_empty() {
                anyhow::bail!("message cannot be empty");
            }

            let target = focus.unwrap_or_else(|| "ui".to_string()).trim().to_string();
            let msg_type = match target.as_str() {
                "concierge" => "user.request",
                "reviewer" => "proposal.request",
                _ => "ui.request",
            };
            let content = format!("[medium] {}", message);

            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            project.mailbox().send_typed(
                &context.source_id,
                &target,
                msg_type,
                &content,
                None,
                None,
                false,
                None,
            )?;
            let _ = project.decisions().append(
                &context.source_id,
                context
                    .decision_action
                    .as_deref()
                    .unwrap_or(decision_action),
                None,
                None,
                "accepted wire request from local ui surface",
                &format!("target={} type={}", target, msg_type),
            );

            Ok(WireResponse::Ack {
                message: serde_json::to_string(&serde_json::json!({
                    "queued": true,
                    "target": target,
                    "message": content
                }))?,
            })
        }
        WireRequest::SessionList => Ok(context.session_list()),
        WireRequest::SessionUse { session_id } => {
            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            let Some(session) = project.sessions().get(&session_id)? else {
                return Ok(WireResponse::Error {
                    message: format!("unknown session: {}", session_id),
                });
            };
            let session_focus = session.focus.clone();
            project.sessions().update_state(
                &session.session_id,
                session.label.as_deref(),
                &session_focus,
                "active",
            )?;
            let label = session.label.clone();
            Ok(WireResponse::SessionCreated {
                session_id: session.session_id,
                label: label.clone(),
                session: Some(WireSessionSummary {
                    session_id: session_id.clone(),
                    label,
                    focus: Some(session_focus),
                    status: Some("active".to_string()),
                    abortable: Some(has_active_request(&session_id)),
                }),
            })
        }
        WireRequest::ApprovalStatus => {
            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            let policy = project.approval().get_policy()?;
            Ok(WireResponse::ApprovalStatus {
                mode: approval_mode_name(policy.mode).to_string(),
                summary: approval_mode_summary(policy.mode).to_string(),
                allowed_tools: crate::runtime::approval::approval_allowed_tools(policy.mode)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                last_block: policy.last_block.as_ref().map(wire_approval_block),
            })
        }
        WireRequest::ApprovalHistory { limit, reason } => {
            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            let items = project
                .approval()
                .list_recent_blocks(limit.unwrap_or(10), reason.as_deref())?
                .into_iter()
                .map(|item| wire_approval_block(&item))
                .collect::<Vec<_>>();
            Ok(WireResponse::ApprovalHistory { items })
        }
        WireRequest::ApprovalSet { mode } => {
            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            let mode = match mode.as_str() {
                "auto" => ApprovalMode::Auto,
                "read_only" => ApprovalMode::ReadOnly,
                "manual" => ApprovalMode::Manual,
                _ => {
                    return Ok(WireResponse::Error {
                        message: format!("unknown approval mode: {}", mode),
                    });
                }
            };
            let updated = project.approval().set_mode(mode)?;
            Ok(WireResponse::ApprovalStatus {
                mode: approval_mode_name(updated.mode).to_string(),
                summary: approval_mode_summary(updated.mode).to_string(),
                allowed_tools: crate::runtime::approval::approval_allowed_tools(updated.mode)
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
                last_block: updated.last_block.as_ref().map(wire_approval_block),
            })
        }
        WireRequest::ToolList => Ok(context.tool_list(tool_summaries())),
        WireRequest::SessionCreate { label, focus } => {
            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            let interaction_mode =
                resolve_session_create_mode(focus.as_deref(), &InteractionMode::Lead)?;
            let focus_label = interaction_mode.label();
            let status = session_status_for_mode(&interaction_mode).to_string();
            let created = project
                .sessions()
                .create(label.as_deref(), &focus_label, &status)?;
            Ok(WireResponse::SessionCreated {
                session_id: created.session_id.clone(),
                label: created.label.clone(),
                session: Some(WireSessionSummary {
                    session_id: created.session_id,
                    label: created.label,
                    focus: Some(focus_label),
                    status: Some(status),
                    abortable: Some(false),
                }),
            })
        }
        WireRequest::ChatAbort => Ok(WireResponse::Ack {
            message: "ui-server has no active chat stream to abort".to_string(),
        }),
        WireRequest::ToolCall {
            name,
            arguments_json,
        } => {
            let project = ProjectContext::new(context.repo_root.to_path_buf())?;
            let call = build_tool_call(&name, arguments_json);
            Ok(match handle_tool_call(&project, &call) {
                Ok(output) => context.tool_result(&name, output),
                Err(err) => context.tool_error(&name, err.to_string()),
            })
        }
    }
}

fn wire_approval_block(block: &crate::project_tools::ApprovalBlockRecord) -> WireApprovalBlock {
    WireApprovalBlock {
        ts: block.ts,
        actor_id: block.actor_id.clone(),
        tool_name: block.tool_name.clone(),
        command: block.command.clone(),
        reason_code: block.reason_code.clone(),
        message: block.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{WireRuntime, execute_wire_request};
    use crate::activity::new_activity_handle;
    use crate::app_support::InteractionMode;
    use crate::config::LlmConfig;
    use crate::openai_compat::Message;
    use crate::project_tools::ProjectContext;
    use crate::resident_agents::AgentSupervisor;
    use crate::wire::{WireFrame, WireRequest, WireResponse};
    use serde_json::Value;
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
                "rustpilot-wire-exec-{}-{}-{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
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

    fn init_git_repo(path: &Path) {
        let status = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .status()
            .expect("git init");
        assert!(status.success());

        fs::write(path.join("README.md"), "hello\n").expect("write readme");
        let status = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .status()
            .expect("git add");
        assert!(status.success());

        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Test User",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(path)
            .status()
            .expect("git commit");
        assert!(status.success());
    }

    fn dummy_llm() -> LlmConfig {
        LlmConfig {
            provider: "test".to_string(),
            profile_id: "test:default".to_string(),
            api_key: "test-key".to_string(),
            api_base_url: "http://127.0.0.1:9".to_string(),
            model: "test-model".to_string(),
            api_kind: crate::llm_profiles::LlmApiKind::OpenAiChatCompletions,
            source: "test".to_string(),
        }
    }

    #[tokio::test]
    async fn wire_chat_send_can_override_focus_to_shell() {
        let temp = TestDir::new("wire-focus-shell");
        init_git_repo(temp.path());

        let project = ProjectContext::new(temp.path().to_path_buf()).expect("project");
        let client = reqwest::Client::new();
        let llm = dummy_llm();
        let progress = new_activity_handle();
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut lead_cursor = 0usize;
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());

        #[cfg(target_os = "windows")]
        let input = "Write-Output wire-focus-ok";
        #[cfg(not(target_os = "windows"))]
        let input = "printf 'wire-focus-ok\\n'";

        let outcome = execute_wire_request(
            WireRequest::ChatSend {
                input: input.to_string(),
                focus: Some("shell".to_string()),
            },
            WireRuntime {
                repo_root: temp.path(),
                client: &client,
                llm: &llm,
                project: &project,
                messages: &mut messages,
                progress: &progress,
                supervisor: &mut supervisor,
                lead_cursor: &mut lead_cursor,
                interaction_mode: &InteractionMode::Lead,
                sessions: project.sessions(),
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
            },
        )
        .await
        .expect("wire request");

        let mut ack_text = String::new();
        for frame in outcome.frames {
            if let crate::wire::WireFrame::Response { response } = frame
                && let WireResponse::Ack { message } = response.payload
            {
                ack_text = message;
            }
        }

        assert!(ack_text.contains("wire-focus-ok"), "ack text: {}", ack_text);
    }

    #[tokio::test]
    async fn wire_chat_abort_emits_session_update_with_abortable_false() {
        let temp = TestDir::new("wire-chat-abort");
        init_git_repo(temp.path());

        let project = ProjectContext::new(temp.path().to_path_buf()).expect("project");
        let client = reqwest::Client::new();
        let llm = dummy_llm();
        let progress = new_activity_handle();
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut lead_cursor = 0usize;
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());

        let outcome = execute_wire_request(
            WireRequest::ChatAbort,
            WireRuntime {
                repo_root: temp.path(),
                client: &client,
                llm: &llm,
                project: &project,
                messages: &mut messages,
                progress: &progress,
                supervisor: &mut supervisor,
                lead_cursor: &mut lead_cursor,
                interaction_mode: &InteractionMode::Lead,
                sessions: project.sessions(),
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
            },
        )
        .await
        .expect("wire request");

        let mut saw_abort_ack = false;
        let mut saw_abortable_false = false;
        for frame in outcome.frames {
            match frame {
                WireFrame::Response { response } => {
                    if let WireResponse::Ack { message } = response.payload {
                        saw_abort_ack = message == "no active chat request to abort";
                    }
                }
                WireFrame::Event { event } => {
                    if let crate::wire::WireEvent::SessionUpdated { abortable, .. } = event.payload
                    {
                        saw_abortable_false = abortable == Some(false);
                    }
                }
            }
        }

        assert!(saw_abort_ack);
        assert!(saw_abortable_false);
    }

    #[tokio::test]
    async fn wire_session_use_returns_target_session_focus() {
        let temp = TestDir::new("wire-session-use-focus");
        init_git_repo(temp.path());

        let project = ProjectContext::new(temp.path().to_path_buf()).expect("project");
        let created = project
            .sessions()
            .create(Some("shell-session"), "shell", "idle")
            .expect("create session");
        let client = reqwest::Client::new();
        let llm = dummy_llm();
        let progress = new_activity_handle();
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut lead_cursor = 0usize;
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());

        let outcome = execute_wire_request(
            WireRequest::SessionUse {
                session_id: created.session_id.clone(),
            },
            WireRuntime {
                repo_root: temp.path(),
                client: &client,
                llm: &llm,
                project: &project,
                messages: &mut messages,
                progress: &progress,
                supervisor: &mut supervisor,
                lead_cursor: &mut lead_cursor,
                interaction_mode: &InteractionMode::Lead,
                sessions: project.sessions(),
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
            },
        )
        .await
        .expect("wire request");

        let mut focus = None;
        let mut status = None;
        for frame in outcome.frames {
            if let WireFrame::Response { response } = frame
                && let WireResponse::SessionCreated {
                    session: Some(session),
                    ..
                } = response.payload
            {
                focus = session.focus;
                status = session.status;
            }
        }

        assert_eq!(focus.as_deref(), Some("shell"));
        assert_eq!(status.as_deref(), Some("active"));
    }

    #[tokio::test]
    async fn wire_session_create_can_override_focus() {
        let temp = TestDir::new("wire-session-create-focus");
        init_git_repo(temp.path());

        let project = ProjectContext::new(temp.path().to_path_buf()).expect("project");
        let client = reqwest::Client::new();
        let llm = dummy_llm();
        let progress = new_activity_handle();
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut lead_cursor = 0usize;
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());

        let outcome = execute_wire_request(
            WireRequest::SessionCreate {
                label: Some("shell-session".to_string()),
                focus: Some("shell".to_string()),
            },
            WireRuntime {
                repo_root: temp.path(),
                client: &client,
                llm: &llm,
                project: &project,
                messages: &mut messages,
                progress: &progress,
                supervisor: &mut supervisor,
                lead_cursor: &mut lead_cursor,
                interaction_mode: &InteractionMode::Lead,
                sessions: project.sessions(),
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
            },
        )
        .await
        .expect("wire request");

        let mut focus = None;
        let mut status = None;
        for frame in outcome.frames {
            if let WireFrame::Response { response } = frame
                && let WireResponse::SessionCreated {
                    session: Some(session),
                    ..
                } = response.payload
            {
                focus = session.focus;
                status = session.status;
            }
        }

        assert_eq!(focus.as_deref(), Some("shell"));
        assert_eq!(status.as_deref(), Some("active"));
    }

    #[tokio::test]
    async fn wire_session_use_emits_session_updated_event() {
        let temp = TestDir::new("wire-session-use-event");
        init_git_repo(temp.path());

        let project = ProjectContext::new(temp.path().to_path_buf()).expect("project");
        let created = project
            .sessions()
            .create(Some("team-session"), "team", "idle")
            .expect("create session");
        let client = reqwest::Client::new();
        let llm = dummy_llm();
        let progress = new_activity_handle();
        let mut supervisor =
            AgentSupervisor::start_defaults(temp.path().to_path_buf(), 1).expect("supervisor");
        let mut messages = vec![Message {
            role: "system".to_string(),
            content: Some("system".to_string()),
            tool_call_id: None,
            tool_calls: None,
        }];
        let mut lead_cursor = 0usize;
        let mut current_session_id = "cli-main".to_string();
        let mut current_session_label = Some("primary".to_string());

        let outcome = execute_wire_request(
            WireRequest::SessionUse {
                session_id: created.session_id,
            },
            WireRuntime {
                repo_root: temp.path(),
                client: &client,
                llm: &llm,
                project: &project,
                messages: &mut messages,
                progress: &progress,
                supervisor: &mut supervisor,
                lead_cursor: &mut lead_cursor,
                interaction_mode: &InteractionMode::Lead,
                sessions: project.sessions(),
                current_session_id: &mut current_session_id,
                current_session_label: &mut current_session_label,
            },
        )
        .await
        .expect("wire request");

        let mut saw_event = false;
        for frame in outcome.frames {
            if let WireFrame::Event { event } = frame
                && let crate::wire::WireEvent::SessionUpdated {
                    focus,
                    status,
                    abortable,
                } = event.payload
            {
                saw_event = focus == "team" && status == "active" && abortable == Some(false);
            }
        }

        assert!(saw_event);
    }

    #[test]
    fn parse_tool_result_response_stays_json_safe() {
        let response = WireResponse::ToolResult {
            name: "read_file".to_string(),
            output: "ok".to_string(),
        };
        let parsed: Value =
            serde_json::from_str(&serde_json::to_string(&response).expect("serialize"))
                .expect("parse");
        assert_eq!(parsed["type"].as_str(), Some("tool_result"));
    }
}
