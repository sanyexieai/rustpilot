use anyhow::Context;

use crate::activity::{ActivityHandle, WaitHeartbeat, set_activity};
use crate::agent_tools::{builtin_tool_definitions, handle_builtin_tool_call};
use crate::config::LlmConfig;
use crate::constants::{MAX_AGENT_TURNS, RETRY_MAX_ATTEMPTS, RETRY_INITIAL_DELAY_MS, RETRY_MAX_DELAY_MS};
use crate::external_tools::{external_tool_definitions, handle_external_tool_call};
use crate::mcp::{handle_mcp_tool_call, mcp_tool_definitions};
use crate::openai_compat::{ChatRequest, ChatResponse, Message, Tool, ToolCall, ToolChoice};
use crate::project_tools::{ProjectContext, handle_project_tool_call, project_tool_definitions};

/// 使用指数退避重试发送 LLM 请求
async fn send_llm_request(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    request: &ChatRequest,
) -> anyhow::Result<reqwest::Response> {
    let mut attempt = 0;
    let mut delay_ms = RETRY_INITIAL_DELAY_MS;

    loop {
        attempt += 1;

        let response = client
            .post(url)
            .bearer_auth(api_key)
            .json(request)
            .send()
            .await
            .context("LLM request failed")?;

        let status = response.status();

        // 成功或客户端错误（不需要重试）
        if status.is_success() {
            return Ok(response);
        }

        // 429 (Too Many Requests) 或 5xx 服务器错误时重试
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

        // 打印重试信息
        println!("> [重试] 请求失败 ({}), {}ms 后重试 ({}/{})...", 
            status, delay_ms, attempt, RETRY_MAX_ATTEMPTS);

        // 指数退避 + 随机抖动
        let jitter = (rand_u32() % 500) as u64;
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms + jitter)).await;

        // 更新延迟时间（指数增长，最大延迟限制）
        delay_ms = (delay_ms * 2).min(RETRY_MAX_DELAY_MS);
    }
}

/// 生成 0-999 的随机数用于抖动
fn rand_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // 使用简单的线性同余生成器
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
) -> anyhow::Result<()> {
    for turn in 0..MAX_AGENT_TURNS {
        set_activity(&progress, turn + 1, "等待模型响应", None);
        let request = ChatRequest {
            model: config.model.clone(),
            messages: messages.clone(),
            tools: Some(tools.to_vec()),
            tool_choice: Some(ToolChoice::Auto("auto".to_string())),
            temperature: Some(0.2),
        };

        let url = format!(
            "{}/chat/completions",
            config.api_base_url.trim_end_matches('/')
        );
        println!("> [模型] 第 {} 轮", turn + 1);
        let heartbeat = WaitHeartbeat::start(progress.clone(), format!("模型第 {} 轮", turn + 1));

        // 使用指数退避重试发送请求
        let response = send_llm_request(client, &url, &config.api_key, &request).await?;
        drop(heartbeat);

        let parsed: ChatResponse = response
            .json()
            .await
            .context("failed to parse LLM response")?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no choices returned by LLM"))?;
        let assistant = choice.message;
        let tool_calls = assistant.tool_calls.clone().unwrap_or_default();
        messages.push(assistant.clone());

        if tool_calls.is_empty() {
            set_activity(&progress, turn + 1, "已完成", None);
            if let Some(content) = assistant.content {
                println!("{}", content);
            }
            return Ok(());
        }

        for call in tool_calls {
            set_activity(
                &progress,
                turn + 1,
                "执行工具中",
                Some(call.function.name.clone()),
            );
            println!("> [活动] 正在执行工具 {}", call.function.name);
            let output = match handle_tool_call(project, &call) {
                Ok(output) => output,
                Err(err) => format!("错误: {}", err),
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
                "工具执行完成",
                Some(call.function.name.clone()),
            );
        }
    }

    set_activity(&progress, MAX_AGENT_TURNS, "已停止", None);
    anyhow::bail!(
        "代理循环超过 {} 轮，请停止当前请求或缩小提示范围",
        MAX_AGENT_TURNS
    )
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
