use anyhow::Context;
use serde::Deserialize;
use serde_json::json;
use crate::openai_compat::{Tool, ToolCall, ToolFunction};
use crate::skills::create_prompt_skill;

use super::{ProjectContext, TaskCreateOptions};

pub fn project_tool_definitions() -> Vec<Tool> {
    vec![
        tool(
            "team_send",
            "Send a mailbox message to another team member.",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" },
                    "msg_type": { "type": "string" },
                    "trace_id": { "type": "string" },
                    "requires_ack": { "type": "boolean" },
                    "in_reply_to": { "type": "string" },
                    "message": { "type": "string" },
                    "task_id": { "type": "integer" }
                },
                "required": ["to", "message"]
            }),
        ),
        tool(
            "team_ack",
            "Acknowledge a mailbox message.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "msg_id": { "type": "string" },
                    "note": { "type": "string" }
                },
                "required": ["owner", "msg_id"]
            }),
        ),
        tool(
            "team_poll",
            "Poll for new mailbox messages after a cursor.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "after_cursor": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["owner"]
            }),
        ),
        tool(
            "team_inbox",
            "Read mailbox messages for a team member.",
            json!({
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["owner"]
            }),
        ),
        tool(
            "task_create",
            "Create a shared task, optionally linking it to a parent task and delegation depth.",
            json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string" },
                    "description": { "type": "string" },
                    "priority": { "type": "string", "enum": ["critical", "high", "medium", "low"] },
                    "role_hint": { "type": "string", "enum": ["developer", "design", "critic", "ui"] },
                    "parent_task_id": { "type": "integer" },
                    "depth": { "type": "integer", "minimum": 0 }
                },
                "required": ["subject"]
            }),
        ),
        tool(
            "delegate_long_running",
            "Delegate long-running work like dev servers, watch processes, and log-following to a worker-owned task.",
            json!({
                "type": "object",
                "properties": {
                    "goal": { "type": "string" },
                    "command": { "type": "string" },
                    "cwd": { "type": "string" },
                    "priority": { "type": "string", "enum": ["critical", "high", "medium", "low"] },
                    "role_hint": { "type": "string", "enum": ["developer", "design", "critic", "ui"] }
                },
                "required": ["goal", "command"]
            }),
        ),
        tool(
            "skill_create",
            "Create a prompt skill under skills/<name>/SKILL.md. After creation, a live LLM test is run: the skill is used as system prompt and `test_prompt` is sent as the user message; the response must contain all strings in `expect_response_contains`.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "body": { "type": "string" },
                    "test_prompt": {
                        "type": "string",
                        "description": "A representative user request that exercises the skill's core capability."
                    },
                    "expect_response_contains": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Keywords or phrases that must appear in the LLM response to test_prompt."
                    }
                },
                "required": ["name", "description", "body", "test_prompt", "expect_response_contains"]
            }),
        ),
        tool(
            "skill_validate",
            "Run all tests for an existing skill: (1) static checks on SKILL.md structure, (2) LLM functional tests using the skill as system prompt, (3) integration test (tests/integration.py) which can run real browser automation and pause for human interaction. Integration tests run in the background — you will be notified via mail when human action is needed or when complete.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Skill name to validate." }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "skill_test_signal",
            "Resume a paused integration test that is waiting for human action (e.g. QR code scan). Call this after completing the required action.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Skill name whose integration test is waiting." }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "task_list",
            "List all tasks with owner and worktree routing data.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "launch_list",
            "List launch registry entries for agent windows and launch-backed processes.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "launch_stop",
            "Stop a launch-backed agent process by launch id.",
            json!({
                "type": "object",
                "properties": {
                    "launch_id": { "type": "string" }
                },
                "required": ["launch_id"]
            }),
        ),
        tool(
            "launch_restart",
            "Restart a launch-backed resident or worker process by launch id.",
            json!({
                "type": "object",
                "properties": {
                    "launch_id": { "type": "string" }
                },
                "required": ["launch_id"]
            }),
        ),
        tool(
            "launch_log_read",
            "Read the recent log tail for a launch-backed process by launch id.",
            json!({
                "type": "object",
                "properties": {
                    "launch_id": { "type": "string" },
                    "lines": { "type": "integer", "minimum": 1 }
                },
                "required": ["launch_id"]
            }),
        ),
        tool(
            "task_get",
            "Get task details by id.",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "integer" } },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_update",
            "Update task status, owner, or priority. Supports paused and cancelled for sub-task control.",
            json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "blocked", "paused", "cancelled", "completed", "failed"] },
                    "owner": { "type": "string" },
                    "priority": { "type": "string", "enum": ["critical", "high", "medium", "low"] }
                },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_bind_worktree",
            "Bind a task to a worktree.",
            json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "worktree": { "type": "string" },
                    "owner": { "type": "string" }
                },
                "required": ["task_id", "worktree"]
            }),
        ),
        tool(
            "worktree_create",
            "Create a git worktree and optionally bind it to a task.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "task_id": { "type": "integer" },
                    "base_ref": { "type": "string" }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_list",
            "List registered worktrees.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "worktree_status",
            "Inspect git status for a worktree.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_run",
            "Run a shell command inside a worktree.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "command": { "type": "string" }
                },
                "required": ["name", "command"]
            }),
        ),
        tool(
            "worktree_keep",
            "Mark a worktree to keep.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_remove",
            "Remove a worktree and optionally complete the bound task.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "force": { "type": "boolean" },
                    "complete_task": { "type": "boolean" }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_events",
            "List recent worktree lifecycle events.",
            json!({
                "type": "object",
                "properties": { "limit": { "type": "integer" } }
            }),
        ),
    ]
}

pub fn handle_project_tool_call(
    context: &ProjectContext,
    call: &ToolCall,
) -> anyhow::Result<Option<String>> {
    let tasks = context.tasks();
    let events = context.events();
    let mailbox = context.mailbox();
    let worktrees = context.worktrees();

    let output = match call.function.name.as_str() {
        "team_send" => {
            let args: TeamSendArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_send arguments")?;
            mailbox.send_typed(
                args.from
                    .as_deref()
                    .unwrap_or(&current_agent_id()),
                &args.to,
                args.msg_type.as_deref().unwrap_or("message"),
                &args.message,
                args.task_id,
                args.trace_id.as_deref(),
                args.requires_ack.unwrap_or(false),
                args.in_reply_to.as_deref(),
            )?
        }
        "team_ack" => {
            let args: TeamAckArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_ack arguments")?;
            mailbox.ack(
                &args.owner,
                &args.msg_id,
                args.note.as_deref().unwrap_or(""),
            )?
        }
        "team_poll" => {
            let args: TeamPollArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_poll arguments")?;
            mailbox.poll(
                &args.owner,
                args.after_cursor.unwrap_or(0),
                args.limit.unwrap_or(20),
            )?
        }
        "team_inbox" => {
            let args: TeamInboxArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid team_inbox arguments")?;
            mailbox.inbox(&args.owner, args.limit.unwrap_or(20))?
        }
        "task_create" => {
            let args: TaskCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_create arguments")?;
            let inherited = current_task_hierarchy(tasks);
            tasks.create_detailed(
                &args.subject,
                args.description.as_deref().unwrap_or(""),
                TaskCreateOptions {
                    priority: args.priority,
                    role_hint: args.role_hint,
                    parent_task_id: args.parent_task_id.or(inherited.parent_task_id),
                    depth: args.depth.or(inherited.depth),
                },
            )?
        }
        "delegate_long_running" => {
            let args: DelegateLongRunningArgs =
                serde_json::from_str(&call.function.arguments)
                    .context("invalid delegate_long_running arguments")?;
            let priority = args.priority.unwrap_or_else(|| "medium".to_string());
            let role_hint = args.role_hint.unwrap_or_else(|| "developer".to_string());
            let inherited = current_task_hierarchy(tasks);
            let cwd_line = args
                .cwd
                .as_deref()
                .map(|cwd| format!("\nWorking directory:\n{}", cwd.trim()))
                .unwrap_or_default();
            let description = format!(
                "[SOURCE=delegate_long_running][PRIORITY={}]\nGoal:\n{}\n\nLong-running command:\n{}\n{}\n\nExecution notes:\n- This work must run in a worker-owned terminal session\n- The parent node must not hold the blocking process\n- Report back startup status, endpoint/port if relevant, and any blocker",
                priority,
                args.goal.trim(),
                args.command.trim(),
                cwd_line,
            );
            let created = tasks.create_detailed(
                &args.goal,
                &description,
                TaskCreateOptions {
                    priority: Some(priority.clone()),
                    role_hint: Some(role_hint),
                    parent_task_id: inherited.parent_task_id,
                    depth: inherited.depth,
                    ..TaskCreateOptions::default()
                },
            )?;
            format!(
                "delegated long-running work as task:\n{}\n\nnext: let the worker own the terminal session for `{}`",
                created,
                args.command.trim()
            )
        }
        "skill_create" => {
            let args: SkillCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid skill_create arguments")?;
            let created = create_prompt_skill(
                &args.name,
                &args.description,
                &args.body,
                Some(&args.test_prompt),
                &args.expect_response_contains,
            )?;

            // Static validation
            let static_result = crate::skills::validate_skill(&created);

            // LLM functional test
            let llm_result = match crate::config::LlmConfig::from_env() {
                Ok(config) => {
                    let client = reqwest::Client::new();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(
                            crate::skills::run_skill_llm_tests(&created, &client, &config),
                        )
                    })
                }
                Err(err) => Err(anyhow::anyhow!("LLM config unavailable: {}", err)),
            };

            match (static_result, llm_result) {
                (Ok(static_count), Ok(llm_count)) => format!(
                    "skill created and all tests passed: {}\nstatic tests: {}, llm tests: {}",
                    created.join("SKILL.md").display(),
                    static_count,
                    llm_count
                ),
                (Err(err), _) => format!(
                    "skill created but static validation FAILED: {}\nerror: {}\nFix the skill content or tests.",
                    created.join("SKILL.md").display(),
                    err
                ),
                (Ok(_), Err(err)) => format!(
                    "skill created but LLM test FAILED: {}\nerror: {}\nRevise the skill body or the test expectations.",
                    created.join("SKILL.md").display(),
                    err
                ),
            }
        }
        "skill_validate" => {
            #[derive(Deserialize)]
            struct SkillValidateArgs {
                name: String,
            }
            let args: SkillValidateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid skill_validate arguments")?;
            let registry = crate::skills::SkillRegistry::load()
                .context("failed to load skill registry")?;
            let skill_dir = registry.base_dir().join(&args.name);
            if !skill_dir.is_dir() {
                format!("skill '{}' not found", args.name)
            } else {
                // 1. Static validation
                let static_result = crate::skills::validate_skill(&skill_dir);

                // 2. LLM functional tests
                let llm_result = match crate::config::LlmConfig::from_env() {
                    Ok(config) => {
                        let client = reqwest::Client::new();
                        tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(
                                crate::skills::run_skill_llm_tests(&skill_dir, &client, &config),
                            )
                        })
                    }
                    Err(err) => Err(anyhow::anyhow!("LLM config unavailable: {}", err)),
                };

                let base_summary = match (&static_result, &llm_result) {
                    (Ok(sc), Ok(lc)) => format!(
                        "skill '{}': static tests: {}, llm tests: {}",
                        args.name, sc, lc
                    ),
                    (Err(err), _) => {
                        return Ok(Some(format!(
                            "skill '{}': static validation FAILED\nerror: {}",
                            args.name, err
                        )))
                    }
                    (Ok(_), Err(err)) => {
                        return Ok(Some(format!(
                            "skill '{}': LLM test FAILED\nerror: {}",
                            args.name, err
                        )))
                    }
                };

                // 3. Integration test — launch in background if present
                if crate::skill_integration::has_integration_test(&skill_dir) {
                    let signal_file = crate::skill_integration::signal_file_path(
                        context.repo_root(),
                        &args.name,
                    );
                    let mailbox = context.mailbox().clone();
                    let skill_name = args.name.clone();
                    let skill_dir2 = skill_dir.clone();

                    // Clone values needed by the on_waiting closure AND the result handler.
                    let mailbox_waiting = mailbox.clone();
                    let mailbox_result = mailbox.clone();
                    let skill_name_waiting = skill_name.clone();
                    let skill_name_result = skill_name.clone();

                    tokio::spawn(async move {
                        let agent_id_waiting = std::env::var("RUSTPILOT_AGENT_ID")
                            .unwrap_or_else(|_| "system".to_string());
                        let agent_id_result = agent_id_waiting.clone();
                        let result = crate::skill_integration::run_integration_script(
                            &skill_dir2,
                            &signal_file,
                            move |msg| {
                                let _ = mailbox_waiting.send(
                                    "system",
                                    &agent_id_waiting,
                                    &format!(
                                        "[skill-test][{}] 需要人工操作: {}\n完成后调用: skill_test_signal {{\"name\": \"{}\"}}",
                                        skill_name_waiting, msg, skill_name_waiting
                                    ),
                                    None,
                                );
                            },
                        )
                        .await;

                        let mailbox2 = mailbox_result;
                        match result {
                            Ok(out) if out.exit_code == 0 => {
                                let _ = mailbox2.send(
                                    "system",
                                    &agent_id_result,
                                    &format!("[skill-test][{}] integration test PASSED ✓", skill_name_result),
                                    None,
                                );
                            }
                            Ok(out) => {
                                let tail: Vec<_> = out.lines.iter().rev().take(10).rev().collect();
                                let _ = mailbox2.send(
                                    "system",
                                    &agent_id_result,
                                    &format!(
                                        "[skill-test][{}] integration test FAILED (exit {})\n{}",
                                        skill_name_result,
                                        out.exit_code,
                                        tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
                                    ),
                                    None,
                                );
                            }
                            Err(e) => {
                                let _ = mailbox2.send(
                                    "system",
                                    &agent_id_result,
                                    &format!("[skill-test][{}] integration test ERROR: {}", skill_name_result, e),
                                    None,
                                );
                            }
                        }
                    });

                    format!(
                        "{}\nintegration test: started in background — you will receive a mail notification when human action is needed or when complete.",
                        base_summary
                    )
                } else {
                    format!("{}\nintegration test: none (add tests/integration.py to enable)", base_summary)
                }
            }
        }
        "skill_test_signal" => {
            #[derive(Deserialize)]
            struct SkillTestSignalArgs {
                name: String,
            }
            let args: SkillTestSignalArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid skill_test_signal arguments")?;
            let signal_file =
                crate::skill_integration::signal_file_path(context.repo_root(), &args.name);
            std::fs::create_dir_all(signal_file.parent().unwrap()).ok();
            std::fs::write(&signal_file, b"")
                .with_context(|| format!("failed to write signal file: {}", signal_file.display()))?;
            format!(
                "signal sent for skill '{}' — integration test will resume shortly.",
                args.name
            )
        }
        "launch_list" => {
            let launches = context.launches().list()?;
            if launches.is_empty() {
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
            }
        }
        "launch_stop" => {
            let args: LaunchIdArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid launch_stop arguments")?;
            crate::resident_agents::stop_launch(context, &args.launch_id)?;
            format!("stopped launch {}", args.launch_id)
        }
        "launch_restart" => {
            let args: LaunchIdArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid launch_restart arguments")?;
            let restarted = crate::resident_agents::restart_launch(context, &args.launch_id)?;
            format!(
                "restarted launch {} as {}",
                args.launch_id, restarted.launch_id
            )
        }
        "launch_log_read" => {
            let args: LaunchLogReadArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid launch_log_read arguments")?;
            let Some(launch) = context.launches().get(&args.launch_id)? else {
                anyhow::bail!("launch not found: {}", args.launch_id);
            };
            if launch.log_path.trim().is_empty() {
                anyhow::bail!("launch {} has no log path", args.launch_id);
            }
            let tail = crate::launch_log::read_tail(&launch.log_path, args.lines.unwrap_or(80));
            if tail.trim().is_empty() {
                format!("no log output for launch {}", args.launch_id)
            } else {
                tail
            }
        }
        "task_list" => tasks.list_all()?,
        "task_get" => {
            let args: TaskIdArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_get arguments")?;
            tasks.get(args.task_id)?
        }
        "task_update" => {
            let args: TaskUpdateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_update arguments")?;
            tasks.update(
                args.task_id,
                args.status.as_deref(),
                args.owner.as_deref(),
                args.priority.as_deref(),
            )?
        }
        "task_bind_worktree" => {
            let args: TaskBindArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_bind_worktree arguments")?;
            tasks.bind_worktree(
                args.task_id,
                &args.worktree,
                args.owner.as_deref().unwrap_or(""),
            )?
        }
        "worktree_create" => {
            let args: WorktreeCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_create arguments")?;
            reject_child_worktree_create(tasks)?;
            worktrees.create(
                &args.name,
                args.task_id,
                args.base_ref.as_deref().unwrap_or("HEAD"),
            )?
        }
        "worktree_list" => worktrees.list_all()?,
        "worktree_status" => {
            let args: WorktreeNameArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_status arguments")?;
            worktrees.status(&args.name)?
        }
        "worktree_run" => {
            let args: WorktreeRunArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_run arguments")?;
            worktrees.run(&args.name, &args.command)?
        }
        "worktree_keep" => {
            let args: WorktreeNameArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_keep arguments")?;
            worktrees.keep(&args.name)?
        }
        "worktree_remove" => {
            let args: WorktreeRemoveArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_remove arguments")?;
            worktrees.remove(
                &args.name,
                args.force.unwrap_or(false),
                args.complete_task.unwrap_or(false),
            )?
        }
        "worktree_events" => {
            let args: EventsArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_events arguments")?;
            events.list_recent(args.limit.unwrap_or(20))?
        }
        _ => return Ok(None),
    };

    Ok(Some(output))
}

fn current_agent_id() -> String {
    std::env::var("RUSTPILOT_AGENT_ID").unwrap_or_else(|_| "lead".to_string())
}

fn reject_child_worktree_create(tasks: &super::TaskManager) -> anyhow::Result<()> {
    let Some(task_id) = std::env::var("RUSTPILOT_TASK_ID")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return Ok(()); // root process — allowed
    };
    let Ok(task) = tasks.get_record(task_id) else {
        return Ok(()); // can't read task record — allow
    };
    if let Some(parent_id) = task.parent_task_id {
        anyhow::bail!(
            "worktree_create refused: only top-level tasks may create worktrees. \
             This is a child task (parent task: #{parent_id}). \
             Each task gets at most one worktree, created by the top-level task. \
             Use the worktree already assigned to your task, or report completion back to the parent."
        );
    }
    Ok(())
}

fn current_task_hierarchy(tasks: &super::TaskManager) -> TaskCreateOptions {
    let Some(task_id) = std::env::var("RUSTPILOT_TASK_ID")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return TaskCreateOptions::default();
    };

    match tasks.get_record(task_id) {
        Ok(task) => TaskCreateOptions {
            parent_task_id: Some(task.id),
            depth: Some(task.depth.saturating_add(1)),
            ..TaskCreateOptions::default()
        },
        Err(_) => TaskCreateOptions::default(),
    }
}

fn tool(name: &str, description: &str, parameters: serde_json::Value) -> Tool {
    Tool {
        r#type: "function".to_string(),
        function: ToolFunction {
            name: name.to_string(),
            description: description.to_string(),
            parameters,
        },
    }
}

#[derive(Debug, Deserialize)]
struct TaskCreateArgs {
    subject: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    role_hint: Option<String>,
    #[serde(default)]
    parent_task_id: Option<u64>,
    #[serde(default)]
    depth: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct LaunchIdArgs {
    launch_id: String,
}

#[derive(Debug, Deserialize)]
struct LaunchLogReadArgs {
    launch_id: String,
    #[serde(default)]
    lines: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct DelegateLongRunningArgs {
    goal: String,
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    role_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillCreateArgs {
    name: String,
    description: String,
    body: String,
    /// A representative user request that exercises the skill's core capability.
    /// Used to run a live LLM test after creation.
    test_prompt: String,
    /// Keywords/phrases that must appear in the LLM's response to the test_prompt.
    #[serde(default)]
    expect_response_contains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TeamSendArgs {
    #[serde(default)]
    from: Option<String>,
    to: String,
    #[serde(default)]
    msg_type: Option<String>,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    requires_ack: Option<bool>,
    #[serde(default)]
    in_reply_to: Option<String>,
    message: String,
    #[serde(default)]
    task_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TeamAckArgs {
    owner: String,
    msg_id: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TeamPollArgs {
    owner: String,
    #[serde(default)]
    after_cursor: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TeamInboxArgs {
    owner: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TaskIdArgs {
    task_id: u64,
}

#[derive(Debug, Deserialize)]
struct TaskUpdateArgs {
    task_id: u64,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    priority: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskBindArgs {
    task_id: u64,
    worktree: String,
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorktreeCreateArgs {
    name: String,
    #[serde(default)]
    task_id: Option<u64>,
    #[serde(default)]
    base_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorktreeNameArgs {
    name: String,
}

#[derive(Debug, Deserialize)]
struct WorktreeRunArgs {
    name: String,
    command: String,
}

#[derive(Debug, Deserialize)]
struct WorktreeRemoveArgs {
    name: String,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    complete_task: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct EventsArgs {
    #[serde(default)]
    limit: Option<usize>,
}
