use crate::activity::ActivityHandle;
use crate::agent::{handle_tool_call, tool_summaries};
use crate::app_commands::{CommandOutcome, LoopDirective, process_user_input};
use crate::app_support::InteractionMode;
use crate::config::LlmConfig;
use crate::openai_compat::Message;
use crate::openai_compat::{ToolCall, ToolCallFunction};
use crate::project_tools::ProjectContext;
use crate::resident_agents::AgentSupervisor;
use crate::wire::{
    WireEnvelope, WireFrame, WireRequest, WireResponse, WireSessionSummary, WireToolSummary,
};
use std::path::PathBuf;

pub(crate) struct WireExecContext<'a> {
    pub(crate) repo_root: &'a PathBuf,
    pub(crate) session_id: String,
    pub(crate) label: Option<String>,
    pub(crate) focus: String,
    pub(crate) status: String,
    pub(crate) source_id: String,
    pub(crate) decision_action: Option<String>,
}

impl<'a> WireExecContext<'a> {
    fn session_created(&self, label: Option<String>) -> WireResponse {
        WireResponse::SessionCreated {
            session_id: self.session_id.clone(),
            label: label.or_else(|| self.label.clone()),
        }
    }

    fn session_list(&self) -> WireResponse {
        WireResponse::SessionList {
            sessions: vec![WireSessionSummary {
                session_id: self.session_id.clone(),
                label: self.label.clone(),
                focus: Some(self.focus.clone()),
                status: Some(self.status.clone()),
            }],
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

pub(crate) async fn execute_wire_request(
    request: WireRequest,
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
    let context = WireExecContext {
        repo_root,
        session_id: "cli-main".to_string(),
        label: Some("primary".to_string()),
        focus: interaction_mode.label(),
        status: "active".to_string(),
        source_id: "cli".to_string(),
        decision_action: None,
    };
    match request {
        WireRequest::ChatSend { input, .. } => {
            process_user_input(
                &input,
                repo_root,
                client,
                llm,
                project,
                messages,
                progress,
                supervisor,
                lead_cursor,
                interaction_mode,
            )
            .await
        }
        WireRequest::ChatAbort => Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames: vec![WireFrame::ack("chat abort requested")],
        }),
        WireRequest::SessionCreate { label } => Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames: vec![WireFrame::Response {
                response: WireEnvelope::new("response", context.session_created(label)),
            }],
        }),
        WireRequest::SessionList => Ok(CommandOutcome {
            directive: LoopDirective::Continue,
            frames: vec![WireFrame::Response {
                response: WireEnvelope::new("response", context.session_list()),
            }],
        }),
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
            let response = match handle_tool_call(project, &call) {
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

pub(crate) fn execute_ui_wire_request(
    request: WireRequest,
    repo_root: &PathBuf,
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

            let project = ProjectContext::new(context.repo_root.clone())?;
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
                context.decision_action.as_deref().unwrap_or(decision_action),
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
        WireRequest::ToolList => Ok(context.tool_list(tool_summaries())),
        WireRequest::SessionCreate { label } => Ok(context.session_created(label)),
        WireRequest::ChatAbort => Ok(WireResponse::Ack {
            message: "ui-server has no active chat stream to abort".to_string(),
        }),
        WireRequest::ToolCall {
            name,
            arguments_json,
        } => {
            let project = ProjectContext::new(context.repo_root.clone())?;
            let call = build_tool_call(&name, arguments_json);
            Ok(match handle_tool_call(&project, &call) {
                Ok(output) => context.tool_result(&name, output),
                Err(err) => context.tool_error(&name, err.to_string()),
            })
        }
    }
}
