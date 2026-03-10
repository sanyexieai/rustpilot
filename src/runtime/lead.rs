use crate::activity::{ActivityHandle, set_activity};
use crate::agent::{diagnose_agent_failure, run_agent_loop, tool_definitions};
use crate::app_support::{InteractionMode, pump_lead_mailbox, trim_messages};
use crate::config::LlmConfig;
use crate::openai_compat::Message;
use crate::project_tools::{EnergyMode, ProjectContext};
use crate::prompt_manager::{PromptAdaptation, adapt_lead_prompt_detailed, render_lead_system_prompt};
use crate::resident_agents::AgentSupervisor;

pub(crate) async fn run_lead_turn(
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &ActivityHandle,
    supervisor: &mut AgentSupervisor,
    lead_cursor: &mut usize,
) -> anyhow::Result<()> {
    let tools = tool_definitions();
    let mut lead_messages = prepare_messages_for_lead(project, messages);
    run_agent_loop(
        client,
        llm,
        project,
        &mut lead_messages,
        &tools,
        progress.clone(),
        None,
    )
    .await?;
    *messages = lead_messages;
    supervisor.reconcile()?;
    pump_lead_mailbox(project, lead_cursor, messages)?;
    println!();
    Ok(())
}

pub(crate) async fn run_lead_turn_with_recovery(
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &ActivityHandle,
    supervisor: &mut AgentSupervisor,
    lead_cursor: &mut usize,
    interaction_mode: &InteractionMode,
    user_input: &str,
) -> anyhow::Result<()> {
    match run_lead_turn(
        client,
        llm,
        project,
        messages,
        progress,
        supervisor,
        lead_cursor,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(err) => {
            let error_text = format_error_chain(&err);
            let adaptation = adapt_lead_prompt_detailed(project.repo_root(), &error_text)
                .unwrap_or_else(|_| PromptAdaptation {
                    changed: false,
                    file_path: project.repo_root().join(".team").join("lead_agent_prompt.md"),
                    before: String::new(),
                    after: String::new(),
                    recovery: None,
                });
            if adaptation.changed {
                if let Some(recovery) = adaptation.recovery.as_ref() {
                    let _ = project.prompt_history().append(
                        "lead",
                        "lead",
                        &adaptation.file_path.display().to_string(),
                        &recovery.strategy,
                        &recovery.trigger,
                        &adaptation.before,
                        &adaptation.after,
                    );
                }
                refresh_lead_system_prompt(project.repo_root(), messages)?;
                if run_lead_turn(
                    client,
                    llm,
                    project,
                    messages,
                    progress,
                    supervisor,
                    lead_cursor,
                )
                .await
                .is_ok()
                {
                    let _ = project.decisions().append(
                        "lead",
                        "lead.recovered",
                        None,
                        None,
                        "lead turn recovered after auto-adjusting prompt",
                        &error_text,
                    );
                    return Ok(());
                }
            }
            handle_lead_turn_error(
                client,
                llm,
                project,
                messages,
                progress,
                interaction_mode,
                user_input,
                &err,
            )
            .await;
            Ok(())
        }
    }
}

pub(crate) fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    chars.saturating_div(4).saturating_add(1)
}

pub(crate) fn looks_like_question(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.contains('?') || trimmed.contains('锛') {
        return true;
    }
    let lower = trimmed.to_lowercase();
    [
        "what",
        "why",
        "how",
        "when",
        "where",
        "who",
        "can you",
        "could you",
        "would you",
        "浠€涔",
        "涓哄暐",
        "涓轰粈涔",
        "鎬庝箞",
        "濡備綍",
        "鑳戒笉鑳",
        "鍙笉鍙互",
        "鏄惁",
        "鏈夋病鏈",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

pub(crate) fn maybe_reflect_energy(
    project: &ProjectContext,
    agent_id: &str,
    trigger: &str,
    task_id: Option<u64>,
    summary: &str,
) {
    let Ok(mode) = project.budgets().energy_mode(agent_id) else {
        return;
    };
    match mode {
        Some(EnergyMode::Low) => {
            let _ = project.reflections().append(
                agent_id,
                trigger,
                task_id,
                summary,
                &["budget entered low mode", "should shrink follow-up scope"],
                Some("reduce exploration and close out smaller actions"),
                true,
            );
            let _ = project.proposals().create(
                agent_id,
                trigger,
                task_id,
                "shrink execution scope under low energy",
                summary,
                &["budget entered low mode", "should shrink follow-up scope"],
                Some("reduce exploration and prioritize close-out work"),
            );
        }
        Some(EnergyMode::Exhausted) => {
            let _ = project.reflections().append(
                agent_id,
                trigger,
                task_id,
                summary,
                &["budget exhausted", "agent should pause non-critical work"],
                Some("keep only critical responses until budget recovers"),
                true,
            );
            let _ = project.proposals().create(
                agent_id,
                trigger,
                task_id,
                "pause non-critical work under exhausted budget",
                summary,
                &["budget exhausted", "agent should pause non-critical work"],
                Some("keep only critical responses until budget recovers"),
            );
        }
        _ => {}
    }
}

fn prepare_messages_for_lead(project: &ProjectContext, messages: &[Message]) -> Vec<Message> {
    match project.budgets().energy_mode("lead").ok().flatten() {
        Some(EnergyMode::Constrained) => trim_messages(messages, 12),
        Some(EnergyMode::Low) => trim_messages(messages, 8),
        Some(EnergyMode::Exhausted) => trim_messages(messages, 4),
        _ => messages.to_vec(),
    }
}

fn refresh_lead_system_prompt(
    repo_root: &std::path::Path,
    messages: &mut Vec<Message>,
) -> anyhow::Result<()> {
    let prompt = render_lead_system_prompt(repo_root)?;
    if let Some(system) = messages
        .first_mut()
        .filter(|message| message.role == "system")
    {
        system.content = Some(prompt);
        return Ok(());
    }
    messages.insert(
        0,
        Message {
            role: "system".to_string(),
            content: Some(prompt),
            tool_call_id: None,
            tool_calls: None,
        },
    );
    Ok(())
}

async fn handle_lead_turn_error(
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &ActivityHandle,
    interaction_mode: &InteractionMode,
    user_input: &str,
    err: &anyhow::Error,
) {
    let error_text = format_error_chain(err);
    let issues = vec![
        "lead turn failed".to_string(),
        format!("provider={}", llm.provider),
        format!("model={}", llm.model),
        truncate_text(&error_text, 240),
    ];
    let issue_refs = issues.iter().map(|item| item.as_str()).collect::<Vec<_>>();
    let summary = format!(
        "lead turn failed while handling input '{}'",
        truncate_text(user_input.trim(), 120)
    );

    let _ = project.events().emit(
        "lead.error",
        serde_json::json!({
            "agent": "lead",
            "input": user_input,
            "focus": interaction_mode.label(),
            "provider": llm.provider,
            "model": llm.model,
        }),
        serde_json::json!({}),
        Some(error_text.clone()),
    );
    let _ = project
        .decisions()
        .append("lead", "lead.error", None, None, &summary, &error_text);
    let _ = project.reflections().append(
        "lead",
        "lead.error",
        None,
        &summary,
        &issue_refs,
        Some("inspect the diagnostic proposal and retry with the smallest possible prompt"),
        true,
    );

    let diagnostic_prompt =
        build_lead_error_prompt(llm, messages, interaction_mode, user_input, &error_text);
    let diagnosis = match diagnose_agent_failure(client, llm, &diagnostic_prompt).await {
        Ok(text) => text,
        Err(diag_err) => format!(
            "Automatic diagnosis failed.\nRoot error:\n{}\n\nFallback:\n1. Retry the same request.\n2. Check network and provider credentials.\n3. Lower prompt size or wait and retry.\n\nDiagnostic error:\n{}",
            error_text,
            format_error_chain(&diag_err)
        ),
    };

    let _ = project.proposals().create(
        "lead",
        "lead.error",
        None,
        "investigate lead runtime failure",
        &diagnosis,
        &issue_refs,
        Some("apply the suggested checks, then retry the command"),
    );
    let _ = project.mailbox().send_typed(
        "lead",
        "reviewer",
        "proposal.request",
        &format!("Lead runtime failure detected.\n\n{}", diagnosis),
        None,
        None,
        false,
        None,
    );
    let _ = project.budgets().record_usage("lead", 40);
    maybe_reflect_energy(project, "lead", "lead.error", None, &summary);

    println!();
    println!("agent error recorded");
    println!("{}", error_text);
    println!();
    println!("recovery analysis:");
    println!("{}", diagnosis);
    println!();

    messages.push(Message {
        role: "assistant".to_string(),
        content: Some(format!(
            "[recovery-analysis]\nThe previous turn failed.\n\nError:\n{}\n\nSuggested recovery:\n{}",
            error_text, diagnosis
        )),
        tool_call_id: None,
        tool_calls: None,
    });
    let _ = project.agents().set_state(
        "lead",
        "idle",
        None,
        Some("cli"),
        Some("main"),
        Some("last turn failed; recovery advice generated"),
    );
    let _ = project.events().emit(
        "lead.recovery",
        serde_json::json!({"agent": "lead"}),
        serde_json::json!({}),
        None,
    );
    set_activity(progress, 0, "error handled", None);
}

fn build_lead_error_prompt(
    llm: &LlmConfig,
    messages: &[Message],
    interaction_mode: &InteractionMode,
    user_input: &str,
    error_text: &str,
) -> String {
    let recent_context = messages
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|message| {
            format!(
                "- role={}: {}",
                message.role,
                truncate_text(message.content.as_deref().unwrap_or(""), 180)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "Runtime failure in rustpilot lead agent.\n\
Provider: {}\n\
Model: {}\n\
Focus: {}\n\
User input: {}\n\
\n\
Error chain:\n{}\n\
\n\
Recent conversation context:\n{}\n\
\n\
Give:\n\
1. most likely cause\n\
2. exact checks to run next\n\
3. minimal workaround to keep the session moving",
        llm.provider,
        llm.model,
        interaction_mode.label(),
        truncate_text(user_input.trim(), 240),
        error_text,
        if recent_context.is_empty() {
            "- none".to_string()
        } else {
            recent_context
        }
    )
}

fn format_error_chain(err: &anyhow::Error) -> String {
    err.chain()
        .enumerate()
        .map(|(idx, cause)| format!("{idx}: {cause}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn truncate_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx < max)
        .last()
        .unwrap_or(0);
    format!("{}...", &text[..end])
}
