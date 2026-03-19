use crate::abort_control::SessionAbortLease;
use anyhow::Context;

use crate::activity::{ActivityHandle, WaitHeartbeat, set_activity};
use crate::agent_tools::{builtin_tool_definitions, handle_builtin_tool_call};
use crate::anthropic_compat;
use crate::config::LlmConfig;
use crate::constants::{
    MAX_AGENT_TURNS, RETRY_INITIAL_DELAY_MS, RETRY_MAX_ATTEMPTS, RETRY_MAX_DELAY_MS,
    WORKER_TURN_TIMEOUT_SECS,
};
use crate::external_tools::{
    external_tool_definitions, external_tool_summaries, handle_external_tool_call,
};
use crate::launch_log;
use crate::llm_profiles::LlmApiKind;
use crate::mcp::{handle_mcp_tool_call, mcp_tool_definitions};
use crate::openai_compat::{ChatRequest, ChatResponse, Message, Tool, ToolCall, ToolChoice};
use crate::project_tools::{
    ApprovalMode, ProjectContext, handle_project_tool_call, project_tool_definitions,
};
use crate::shell_file_tools::{is_dangerous_command, is_read_only_command};
use crate::tool_capability::{ToolCapabilityLevel, ToolRuntimeKind};
use crate::wire::WireToolSummary;
use serde_json::Value;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct AgentProgressReport {
    pub from: String,
    pub to: String,
    pub task_id: Option<u64>,
    pub trace_id: Option<String>,
}

async fn send_llm_request(
    client: &reqwest::Client,
    config: &LlmConfig,
    request: &ChatRequest,
    abort: Option<&SessionAbortLease>,
) -> anyhow::Result<reqwest::Response> {
    let mut attempt = 0;
    let mut delay_ms = RETRY_INITIAL_DELAY_MS;

    loop {
        attempt += 1;

        if abort.as_ref().is_some_and(|lease| lease.is_cancelled()) {
            anyhow::bail!("request aborted");
        }

        let request_future = async {
            match config.api_kind {
                LlmApiKind::OpenAiChatCompletions => {
                    let url = format!(
                        "{}/chat/completions",
                        config.api_base_url.trim_end_matches('/')
                    );
                    client
                        .post(&url)
                        .bearer_auth(&config.api_key)
                        .json(request)
                        .send()
                        .await
                }
                LlmApiKind::AnthropicMessages => {
                    let url = format!(
                        "{}/messages?beta=true",
                        config.api_base_url.trim_end_matches('/')
                    );
                    let anthropic_request = anthropic_compat::build_request(
                        &config.model,
                        &request.messages,
                        request.tools.as_deref(),
                        request.tool_choice.as_ref(),
                        request.temperature,
                    );
                    client
                        .post(&url)
                        .bearer_auth(&config.api_key)
                        .header("x-api-key", &config.api_key)
                        .header("anthropic-version", "2023-06-01")
                        .header(
                            "anthropic-beta",
                            "claude-code-20250219,interleaved-thinking-2025-05-14",
                        )
                        .header("anthropic-dangerous-direct-browser-access", "true")
                        .header("x-app", "cli")
                        .json(&anthropic_request)
                        .send()
                        .await
                }
            }
        };

        let response = match await_with_abort(request_future, abort).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => {
                let should_retry = err.is_timeout() || err.is_connect() || err.is_request();
                if !should_retry || attempt >= RETRY_MAX_ATTEMPTS {
                    return Err(err).context("LLM request failed");
                }
                launch_log::emit(format!(
                    "> [retry] transport error ({}), retrying in {}ms ({}/{})...",
                    err, delay_ms, attempt, RETRY_MAX_ATTEMPTS
                ));
                let jitter = (rand_u32() % 500) as u64;
                sleep_with_abort(tokio::time::Duration::from_millis(delay_ms + jitter), abort)
                    .await?;
                delay_ms = (delay_ms * 2).min(RETRY_MAX_DELAY_MS);
                continue;
            }
            Err(err) => return Err(err),
        };

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let should_retry = status.as_u16() == 429 || status.is_server_error();
        if !should_retry {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM request failed with {}: {}", status, body);
        }

        if attempt >= RETRY_MAX_ATTEMPTS {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "LLM request failed after {} attempts with {}: {}",
                RETRY_MAX_ATTEMPTS,
                status,
                body
            );
        }

        launch_log::emit(format!(
            "> [retry] request failed ({}), retrying in {}ms ({}/{})...",
            status, delay_ms, attempt, RETRY_MAX_ATTEMPTS
        ));

        let jitter = (rand_u32() % 500) as u64;
        sleep_with_abort(tokio::time::Duration::from_millis(delay_ms + jitter), abort).await?;
        delay_ms = (delay_ms * 2).min(RETRY_MAX_DELAY_MS);
    }
}

async fn await_with_abort<T>(
    future: impl std::future::Future<Output = T>,
    abort: Option<&SessionAbortLease>,
) -> anyhow::Result<T> {
    if let Some(lease) = abort {
        tokio::select! {
            output = future => Ok(output),
            _ = lease.cancelled() => anyhow::bail!("request aborted"),
        }
    } else {
        Ok(future.await)
    }
}

async fn sleep_with_abort(
    duration: tokio::time::Duration,
    abort: Option<&SessionAbortLease>,
) -> anyhow::Result<()> {
    await_with_abort(tokio::time::sleep(duration), abort).await?;
    Ok(())
}

async fn parse_llm_response(
    response: reqwest::Response,
    api_kind: LlmApiKind,
) -> anyhow::Result<Message> {
    match api_kind {
        LlmApiKind::OpenAiChatCompletions => {
            let parsed: ChatResponse = response
                .json()
                .await
                .context("failed to parse LLM response")?;
            parsed
                .choices
                .into_iter()
                .next()
                .map(|choice| choice.message)
                .ok_or_else(|| anyhow::anyhow!("no choices returned by LLM"))
        }
        LlmApiKind::AnthropicMessages => {
            let parsed: anthropic_compat::AnthropicResponse = response
                .json()
                .await
                .context("failed to parse Anthropic response")?;
            Ok(anthropic_compat::parse_response(parsed))
        }
    }
}

fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    const MULTIPLIER: u32 = 1103515245;
    const INCREMENT: u32 = 12345;
    const MOD: u32 = 2_u32.pow(31);
    (nanos.wrapping_mul(MULTIPLIER).wrapping_add(INCREMENT)) % MOD
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent_loop(
    client: &reqwest::Client,
    config: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    progress: ActivityHandle,
    report: Option<&AgentProgressReport>,
    abort: Option<&SessionAbortLease>,
) -> anyhow::Result<()> {
    let agent_label = current_agent_log_label();
    for turn in 0..MAX_AGENT_TURNS {
        if abort.as_ref().is_some_and(|lease| lease.is_cancelled()) {
            anyhow::bail!("request aborted");
        }
        set_activity(&progress, turn + 1, "waiting for model response", None);
        emit_progress(
            project,
            report,
            "task.progress",
            &format!("turn {}: waiting for model response", turn + 1),
        );

        let request = ChatRequest {
            model: config.model.clone(),
            messages: messages.clone(),
            tools: Some(tools.to_vec()),
            tool_choice: Some(ToolChoice::Auto("auto".to_string())),
            temperature: Some(0.2),
        };

        launch_log::emit(format!("> [{}] [model] turn {}", agent_label, turn + 1));
        let heartbeat = WaitHeartbeat::start(progress.clone(), format!("model turn {}", turn + 1));
        let turn_result = tokio::time::timeout(
            std::time::Duration::from_secs(WORKER_TURN_TIMEOUT_SECS),
            async {
                let response = send_llm_request(client, config, &request, abort).await?;
                parse_llm_response(response, config.api_kind).await
            },
        )
        .await;
        drop(heartbeat);
        let assistant = match turn_result {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                launch_log::emit(format!(
                    "[{}] turn {} timed out after {}s — triggering self-recovery",
                    agent_label,
                    turn + 1,
                    WORKER_TURN_TIMEOUT_SECS
                ));
                anyhow::bail!(
                    "LLM turn {} timed out after {}s with no response",
                    turn + 1,
                    WORKER_TURN_TIMEOUT_SECS
                );
            }
        };
        let tool_calls = assistant.tool_calls.clone().unwrap_or_default();
        messages.push(assistant.clone());

        if tool_calls.is_empty() {
            set_activity(&progress, turn + 1, "completed", None);
            emit_progress(
                project,
                report,
                "task.progress",
                &format!("turn {}: model returned final result", turn + 1),
            );
            if let Some(content) = assistant.content {
                launch_log::emit(format!("> [{}] [final]\n{}", agent_label, content));
            }
            return Ok(());
        }

        for call in tool_calls {
            set_activity(
                &progress,
                turn + 1,
                "executing tool",
                Some(call.function.name.clone()),
            );
            emit_progress(
                project,
                report,
                "task.progress",
                &format!("turn {}: executing tool {}", turn + 1, call.function.name),
            );
            launch_log::emit(format!(
                "> [{}] [activity] running tool {}",
                agent_label, call.function.name
            ));
            if call.function.name == "skill_create" {
                launch_log::emit(format!(
                    "> [{}] [skill_create] args: {}",
                    agent_label,
                    truncate_for_print(&call.function.arguments)
                ));
            }
            let tool_started = Instant::now();
            let output = match handle_tool_call(project, &call) {
                Ok(output) => output,
                Err(err) => format!("error: {}", err),
            };
            launch_log::emit(format!(
                "> [{}] [{}] {:.2}s {}",
                agent_label,
                call.function.name,
                tool_started.elapsed().as_secs_f64(),
                truncate_for_print(&output)
            ));
            messages.push(Message {
                role: "tool".to_string(),
                content: Some(output),
                tool_call_id: Some(call.id.clone()),
                tool_calls: None,
            });
            set_activity(
                &progress,
                turn + 1,
                "tool completed",
                Some(call.function.name.clone()),
            );
            emit_progress(
                project,
                report,
                "task.progress",
                &format!("turn {}: tool {} completed", turn + 1, call.function.name),
            );
        }
    }

    set_activity(&progress, MAX_AGENT_TURNS, "stopped", None);
    anyhow::bail!(
        "agent loop exceeded {} turns, stop the request or reduce prompt scope",
        MAX_AGENT_TURNS
    )
}

fn current_agent_log_label() -> String {
    let agent_id = std::env::var("RUSTPILOT_AGENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "root".to_string());
    let task_suffix = std::env::var("RUSTPILOT_TASK_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" task={}", value))
        .unwrap_or_default();
    format!("{}{}", agent_id, task_suffix)
}

fn emit_progress(
    project: &ProjectContext,
    report: Option<&AgentProgressReport>,
    msg_type: &str,
    message: &str,
) {
    let Some(report) = report else {
        return;
    };
    let _ = project.mailbox().send_typed(
        &report.from,
        &report.to,
        msg_type,
        message,
        report.task_id,
        report.trace_id.as_deref(),
        false,
        None,
    );
}

pub fn truncate_for_print(text: &str) -> String {
    const MAX: usize = 200;
    if text.len() <= MAX {
        return text.to_string();
    }

    let end = text
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx < MAX)
        .last()
        .unwrap_or(0);

    if end == 0 {
        "...".to_string()
    } else {
        format!("{}...", &text[..end])
    }
}

pub fn tool_definitions() -> Vec<Tool> {
    let mut tools = builtin_tool_definitions();
    tools.extend(project_tool_definitions());
    tools.extend(external_tool_definitions());
    tools.extend(mcp_tool_definitions());
    tools
}

pub fn tool_summaries() -> Vec<WireToolSummary> {
    let mut tools = Vec::new();
    append_tool_summaries(
        &mut tools,
        "builtin",
        ToolCapabilityLevel::Kernel,
        ToolRuntimeKind::RustBinary,
        builtin_tool_definitions(),
    );
    append_tool_summaries(
        &mut tools,
        "project",
        ToolCapabilityLevel::Project,
        ToolRuntimeKind::RustBinary,
        project_tool_definitions(),
    );
    tools.extend(external_tool_summaries());
    append_tool_summaries(
        &mut tools,
        "mcp",
        ToolCapabilityLevel::Project,
        ToolRuntimeKind::Mcp,
        mcp_tool_definitions(),
    );
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
}

fn append_tool_summaries(
    into: &mut Vec<WireToolSummary>,
    source: &str,
    capability_level: ToolCapabilityLevel,
    runtime_kind: ToolRuntimeKind,
    tools: Vec<Tool>,
) {
    into.extend(tools.into_iter().map(|tool| WireToolSummary {
        name: tool.function.name,
        source: source.to_string(),
        description: tool.function.description,
        parameters: tool.function.parameters,
        capability_level: Some(capability_level.as_str().to_string()),
        runtime_kind: Some(runtime_kind.as_str().to_string()),
    }));
}

pub fn handle_tool_call(project: &ProjectContext, call: &ToolCall) -> anyhow::Result<String> {
    guard_tool_policy(project, call)?;
    if let Some(output) = handle_builtin_tool_call(call)? {
        return Ok(output);
    }
    if let Some(output) = handle_project_tool_call(project, call)? {
        return Ok(output);
    }
    if let Some(output) = handle_external_tool_call(call)? {
        return Ok(output);
    }
    if let Some(output) = handle_mcp_tool_call(call)? {
        return Ok(output);
    }
    anyhow::bail!("unknown tool: {}", call.function.name)
}

fn guard_tool_policy(project: &ProjectContext, call: &ToolCall) -> anyhow::Result<()> {
    let policy = project.approval().get_policy()?;
    let actor_id = approval_actor_id();
    if !matches!(call.function.name.as_str(), "bash" | "worktree_run") {
        return Ok(());
    }
    let command =
        extract_command(&call.function.arguments).unwrap_or_else(|| "<unknown>".to_string());
    if is_dangerous_command(&command) {
        let message = format!(
            "tool '{}' is blocked because the command is classified as dangerous: {}",
            call.function.name, command
        );
        let _ = project.approval().record_block(
            &actor_id,
            &call.function.name,
            &command,
            "dangerous",
            &message,
        );
        anyhow::bail!("{}", message);
    }
    if policy.mode == ApprovalMode::ReadOnly && !is_read_only_command(&command) {
        let message = format!(
            "tool '{}' is blocked by approval mode=read_only. Only clearly read-only shell commands are allowed; use /shell for manual execution: {}",
            call.function.name, command
        );
        let _ = project.approval().record_block(
            &actor_id,
            &call.function.name,
            &command,
            "read_only",
            &message,
        );
        anyhow::bail!("{}", message);
    }
    if policy.mode == ApprovalMode::Manual {
        let message = format!(
            "tool '{}' is blocked by approval mode=manual. Run it explicitly with /shell if you want to execute: {}",
            call.function.name, command
        );
        let _ = project.approval().record_block(
            &actor_id,
            &call.function.name,
            &command,
            "manual",
            &message,
        );
        anyhow::bail!("{}", message);
    }
    // worktree_run 时额外检查：root/parent 只允许只读命令
    if call.function.name == "worktree_run"
        && crate::agent_tools::current_node_is_parent().unwrap_or(false)
        && !is_read_only_command(&command)
    {
        let message = format!(
            "worktree_run refused: root/parent coordinator may only run read-only commands directly. \
             Choose the appropriate path:\n\
             Option A — single child task: use task_create if one worker can own this end-to-end. Command: `{}`\n\
             Option B — team: if this requires multiple coordinated roles or phases, \
             create one task_create per role/phase and coordinate via team_send.",
            command.trim()
        );
        let _ = project.approval().record_block(
            &actor_id,
            &call.function.name,
            &command,
            "parent_policy",
            &message,
        );
        anyhow::bail!("{}", message);
    }
    Ok(())
}

fn extract_command(arguments: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(arguments).ok()?;
    parsed
        .get("command")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn approval_actor_id() -> String {
    std::env::var("RUSTPILOT_AGENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "lead".to_string())
}

pub async fn one_shot_completion(
    client: &reqwest::Client,
    config: &LlmConfig,
    system: &str,
    user: &str,
) -> anyhow::Result<String> {
    let request = ChatRequest {
        model: config.model.clone(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: Some(system.to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(user.to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
        ],
        tools: None,
        tool_choice: None,
        temperature: Some(0.0),
    };
    let response = send_llm_request(client, config, &request, None).await?;
    let message = parse_llm_response(response, config.api_kind).await?;
    Ok(message.content.unwrap_or_default())
}

pub async fn diagnose_agent_failure(
    client: &reqwest::Client,
    config: &LlmConfig,
    prompt: &str,
) -> anyhow::Result<String> {
    let request = ChatRequest {
        model: config.model.clone(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: Some(
                    "You analyze runtime failures for a coding agent. Explain the likely cause, the exact checks to run next, and a minimal recovery plan. Keep the answer concise and actionable.".to_string(),
                ),
                tool_call_id: None,
                tool_calls: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(prompt.trim().to_string()),
                tool_call_id: None,
                tool_calls: None,
            },
        ],
        tools: None,
        tool_choice: None,
        temperature: Some(0.1),
    };

    let response = send_llm_request(client, config, &request, None).await?;
    let message = parse_llm_response(response, config.api_kind).await?;
    Ok(message
        .content
        .unwrap_or_else(|| "No diagnostic content returned.".to_string()))
}
