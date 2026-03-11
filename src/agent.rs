use anyhow::Context;

use crate::activity::{ActivityHandle, WaitHeartbeat, set_activity};
use crate::agent_tools::{builtin_tool_definitions, handle_builtin_tool_call};
use crate::anthropic_compat;
use crate::config::LlmConfig;
use crate::constants::{
    MAX_AGENT_TURNS, RETRY_INITIAL_DELAY_MS, RETRY_MAX_ATTEMPTS, RETRY_MAX_DELAY_MS,
};
use crate::external_tools::{external_tool_definitions, handle_external_tool_call};
use crate::llm_profiles::LlmApiKind;
use crate::mcp::{handle_mcp_tool_call, mcp_tool_definitions};
use crate::openai_compat::{ChatRequest, ChatResponse, Message, Tool, ToolCall, ToolChoice};
use crate::project_tools::{ProjectContext, handle_project_tool_call, project_tool_definitions};
use crate::wire::WireToolSummary;

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
) -> anyhow::Result<reqwest::Response> {
    let mut attempt = 0;
    let mut delay_ms = RETRY_INITIAL_DELAY_MS;

    loop {
        attempt += 1;

        let response = match match config.api_kind {
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
        } {
            Ok(response) => response,
            Err(err) => {
                let should_retry = err.is_timeout() || err.is_connect() || err.is_request();
                if !should_retry || attempt >= RETRY_MAX_ATTEMPTS {
                    return Err(err).context("LLM request failed");
                }
                println!(
                    "> [retry] transport error ({}), retrying in {}ms ({}/{})...",
                    err, delay_ms, attempt, RETRY_MAX_ATTEMPTS
                );
                let jitter = (rand_u32() % 500) as u64;
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms + jitter)).await;
                delay_ms = (delay_ms * 2).min(RETRY_MAX_DELAY_MS);
                continue;
            }
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

        println!(
            "> [retry] request failed ({}), retrying in {}ms ({}/{})...",
            status, delay_ms, attempt, RETRY_MAX_ATTEMPTS
        );

        let jitter = (rand_u32() % 500) as u64;
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms + jitter)).await;
        delay_ms = (delay_ms * 2).min(RETRY_MAX_DELAY_MS);
    }
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

pub async fn run_agent_loop(
    client: &reqwest::Client,
    config: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    progress: ActivityHandle,
    report: Option<&AgentProgressReport>,
) -> anyhow::Result<()> {
    for turn in 0..MAX_AGENT_TURNS {
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

        println!("> [model] turn {}", turn + 1);
        let heartbeat = WaitHeartbeat::start(progress.clone(), format!("model turn {}", turn + 1));
        let response = send_llm_request(client, config, &request).await?;
        drop(heartbeat);

        let assistant = parse_llm_response(response, config.api_kind).await?;
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
                println!("{}", content);
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
            println!("> [activity] running tool {}", call.function.name);
            let output = match handle_tool_call(project, &call) {
                Ok(output) => output,
                Err(err) => format!("error: {}", err),
            };
            println!("> {}: {}", call.function.name, truncate_for_print(&output));
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
    append_tool_summaries(&mut tools, "builtin", builtin_tool_definitions());
    append_tool_summaries(&mut tools, "project", project_tool_definitions());
    append_tool_summaries(&mut tools, "external", external_tool_definitions());
    append_tool_summaries(&mut tools, "mcp", mcp_tool_definitions());
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools
}

fn append_tool_summaries(into: &mut Vec<WireToolSummary>, source: &str, tools: Vec<Tool>) {
    into.extend(tools.into_iter().map(|tool| WireToolSummary {
        name: tool.function.name,
        source: source.to_string(),
        description: tool.function.description,
        parameters: tool.function.parameters,
    }));
}

pub fn handle_tool_call(project: &ProjectContext, call: &ToolCall) -> anyhow::Result<String> {
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

    let response = send_llm_request(client, config, &request).await?;
    let message = parse_llm_response(response, config.api_kind).await?;
    Ok(message
        .content
        .unwrap_or_else(|| "No diagnostic content returned.".to_string()))
}
