use crate::abort_control::begin_session_request;
use crate::activity::{ActivityHandle, set_activity};
use crate::agent::{diagnose_agent_failure, run_agent_loop, tool_definitions};
use crate::app_commands::AppRuntime;
use crate::app_support::{InteractionMode, current_agent_id, pump_lead_mailbox, trim_messages};
use crate::config::LlmConfig;
use crate::openai_compat::Message;
use crate::project_tools::{EnergyMode, ProjectContext};
use crate::prompt_manager::{
    PromptAdaptation, adapt_root_prompt_detailed, render_root_system_prompt,
};
use crate::resident_agents::AgentSupervisor;

const ROOT_ERROR_EVENT: &str = "root.error";
const ROOT_RECOVERY_EVENT: &str = "root.recovery";
const ROOT_RECOVERED_ACTION: &str = "root.recovered";

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_root_turn(
    session_id: &str,
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
    let abort = begin_session_request(session_id);
    run_agent_loop(
        client,
        llm,
        project,
        &mut lead_messages,
        &tools,
        progress.clone(),
        None,
        Some(&abort),
    )
    .await?;
    *messages = lead_messages;
    supervisor.reconcile()?;
    pump_lead_mailbox(project, lead_cursor, messages)?;
    println!();
    Ok(())
}

#[allow(dead_code)]
pub(crate) async fn run_lead_turn(
    session_id: &str,
    client: &reqwest::Client,
    llm: &LlmConfig,
    project: &ProjectContext,
    messages: &mut Vec<Message>,
    progress: &ActivityHandle,
    supervisor: &mut AgentSupervisor,
    lead_cursor: &mut usize,
) -> anyhow::Result<()> {
    run_root_turn(
        session_id, client, llm, project, messages, progress, supervisor, lead_cursor,
    )
    .await
}

pub(crate) async fn run_root_turn_with_recovery(
    user_input: &str,
    runtime: AppRuntime<'_>,
) -> anyhow::Result<()> {
    match run_root_turn(
        runtime.session_id,
        runtime.client,
        runtime.llm,
        runtime.project,
        runtime.messages,
        runtime.progress,
        runtime.supervisor,
        runtime.lead_cursor,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(err) => {
            let actor_id = current_agent_id();
            if err.to_string().contains("request aborted") {
                set_activity(runtime.progress, 0, "aborted", None);
                let _ = runtime.project.agents().set_state(
                    &actor_id,
                    "idle",
                    None,
                    Some("cli"),
                    Some("main"),
                    Some("request aborted"),
                );
                println!("request aborted");
                return Ok(());
            }
            let error_text = format_error_chain(&err);
            let adaptation = adapt_root_prompt_detailed(runtime.project.repo_root(), &error_text)
                .unwrap_or_else(|_| PromptAdaptation {
                    changed: false,
                    file_path: runtime
                        .project
                        .repo_root()
                        .join(".team")
                        .join("root_agent_prompt.md"),
                    before: String::new(),
                    after: String::new(),
                    recovery: None,
                });
            if adaptation.changed {
                if let Some(recovery) = adaptation.recovery.as_ref() {
                    let file_path = adaptation.file_path.display().to_string();
                    let _ = runtime.project.prompt_history().append(
                        "root",
                        &actor_id,
                        &file_path,
                        &recovery.strategy,
                        &recovery.trigger,
                        &adaptation.before,
                        &adaptation.after,
                    );
                }
                refresh_root_system_prompt(runtime.project.repo_root(), runtime.messages)?;
                if run_root_turn(
                    runtime.session_id,
                    runtime.client,
                    runtime.llm,
                    runtime.project,
                    runtime.messages,
                    runtime.progress,
                    runtime.supervisor,
                    runtime.lead_cursor,
                )
                .await
                .is_ok()
                {
                    let _ = runtime.project.decisions().append(
                        &actor_id,
                        ROOT_RECOVERED_ACTION,
                        None,
                        None,
                        "root turn recovered after auto-adjusting prompt",
                        &error_text,
                    );
                    return Ok(());
                }
            }
            handle_lead_turn_error(
                runtime.client,
                runtime.llm,
                runtime.project,
                runtime.messages,
                runtime.progress,
                runtime.interaction_mode,
                user_input,
                &err,
            )
            .await;
            Ok(())
        }
    }
}

#[allow(dead_code)]
pub(crate) async fn run_lead_turn_with_recovery(
    user_input: &str,
    runtime: AppRuntime<'_>,
) -> anyhow::Result<()> {
    run_root_turn_with_recovery(user_input, runtime).await
}

pub(crate) fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    chars.saturating_div(4).saturating_add(1)
}

pub(crate) fn looks_like_question(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.contains('?') || trimmed.contains('？') {
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
        "什么",
        "为什么",
        "如何",
        "怎么",
        "能不能",
        "可不可以",
        "是否",
        "有没有",
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
    match project
        .budgets()
        .energy_mode(&current_agent_id())
        .ok()
        .flatten()
    {
        Some(EnergyMode::Constrained) => trim_messages(messages, 12),
        Some(EnergyMode::Low) => trim_messages(messages, 8),
        Some(EnergyMode::Exhausted) => trim_messages(messages, 4),
        _ => messages.to_vec(),
    }
}

fn refresh_root_system_prompt(
    repo_root: &std::path::Path,
    messages: &mut Vec<Message>,
) -> anyhow::Result<()> {
    let prompt = render_root_system_prompt(repo_root)?;
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

#[allow(clippy::too_many_arguments)]
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
    let actor_id = current_agent_id();
    let error_text = format_error_chain(err);
    let issues = [
        "root turn failed".to_string(),
        format!("provider={}", llm.provider),
        format!("model={}", llm.model),
        truncate_text(&error_text, 240),
    ];
    let issue_refs = issues.iter().map(|item| item.as_str()).collect::<Vec<_>>();
    let summary = format!(
        "root turn failed while handling input '{}'",
        truncate_text(user_input.trim(), 120)
    );

    let _ = project.events().emit(
        ROOT_ERROR_EVENT,
        serde_json::json!({
            "agent": actor_id,
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
        .append(&actor_id, ROOT_ERROR_EVENT, None, None, &summary, &error_text);
    let _ = project.reflections().append(
        &actor_id,
        ROOT_ERROR_EVENT,
        None,
        &summary,
        &issue_refs,
        Some("inspect the diagnostic proposal and retry with the smallest possible prompt"),
        true,
    );

    let diagnostic_prompt =
        build_root_error_prompt(llm, messages, interaction_mode, user_input, &error_text);
    let diagnosis = match diagnose_agent_failure(client, llm, &diagnostic_prompt).await {
        Ok(text) => text,
        Err(diag_err) => format!(
            "Automatic diagnosis failed.\nRoot error:\n{}\n\nFallback:\n1. Retry the same request.\n2. Check network and provider credentials.\n3. Lower prompt size or wait and retry.\n\nDiagnostic error:\n{}",
            error_text,
            format_error_chain(&diag_err)
        ),
    };

    let _ = project.proposals().create(
        &actor_id,
        ROOT_ERROR_EVENT,
        None,
        "investigate root runtime failure",
        &diagnosis,
        &issue_refs,
        Some("apply the suggested checks, then retry the command"),
    );
    let reviewer_message = format!("Root runtime failure detected.\n\n{}", diagnosis);
    let _ = project.mailbox().send_typed(
        &actor_id,
        "reviewer",
        "proposal.request",
        &reviewer_message,
        None,
        None,
        false,
        None,
    );
    let _ = project.budgets().record_usage(&actor_id, 40);
    maybe_reflect_energy(project, &actor_id, ROOT_ERROR_EVENT, None, &summary);

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
        &actor_id,
        "idle",
        None,
        Some("cli"),
        Some("main"),
        Some("last turn failed; recovery advice generated"),
    );
    let _ = project.events().emit(
        ROOT_RECOVERY_EVENT,
        serde_json::json!({"agent": actor_id}),
        serde_json::json!({}),
        None,
    );
    set_activity(progress, 0, "error handled", None);
}

fn build_root_error_prompt(
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
        "Runtime failure in rustpilot root agent.\n\
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
