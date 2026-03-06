use anyhow::Context;
use rustpilot::activity::{
    new_activity_handle, render_activity, set_activity, ActivityHandle, WaitHeartbeat,
};
use rustpilot::agent_tools::{
    builtin_tool_definitions, clear_terminal_manager_live_sessions, handle_builtin_tool_call,
    reset_terminal_manager,
};
use rustpilot::config::LlmConfig;
use rustpilot::openai_compat::{
    ChatRequest, ChatResponse, Message, Tool, ToolCall, ToolChoice,
};
use rustpilot::project_tools::{
    handle_project_tool_call, project_tool_definitions, EventBus, ProjectContext, TaskManager,
    TaskRecord, WorktreeManager,
};
use rustpilot::skills::SkillRegistry;
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::MutexGuard;

const LLM_TIMEOUT_SECS: u64 = 120;
const MAX_AGENT_TURNS: usize = 24;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let cwd = std::env::current_dir()?;
    let repo_root = detect_repo_root(&cwd).unwrap_or_else(|| cwd.clone());
    let llm = LlmConfig::from_env()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(llm_timeout_secs()))
        .build()?;

    let system_prompt = format!(
        "你是位于 {} 的编码代理。优先使用 task_* 和 worktree_* 工具处理并行或高风险工作；任务是控制面，worktree 是执行面。需要查看生命周期时使用 worktree_events。",
        cwd.display()
    );

    let project = ProjectContext::new(repo_root.clone())?;
    let skills = SkillRegistry::load().unwrap_or_else(|_| SkillRegistry::empty());
    let progress = new_activity_handle();

    println!("s12 仓库根目录: {}", repo_root.display());
    if !project.worktrees().git_available {
        println!("提示: 当前目录不是 git 仓库，worktree_* 工具会返回错误。");
    }

    let mut messages = vec![Message {
        role: "system".to_string(),
        content: Some(system_prompt),
        tool_call_id: None,
        tool_calls: None,
    }];

    let tools = tool_definitions();
    loop {
        let mut input = String::new();
        print!("> ");
        io::stdout().flush().ok();
        let bytes = io::stdin().read_line(&mut input).context("failed to read input")?;
        if bytes == 0 {
            break;
        }

        let trimmed = input.trim();
        if matches!(trimmed, "q" | "quit" | "exit") {
            break;
        }
        if trimmed == "/tasks" {
            println!("{}", project.tasks().list_all()?);
            continue;
        }
        if trimmed == "/worktrees" {
            println!("{}", project.worktrees().list_all()?);
            continue;
        }
        if trimmed == "/events" {
            println!("{}", project.events().list_recent(20)?);
            continue;
        }
        if trimmed == "/status" {
            println!("{}", render_activity(&progress));
            continue;
        }
        if trimmed == "/skills" {
            if skills.list().is_empty() {
                println!("没有可用 skills。");
            } else {
                println!("skills dir: {}", skills.base_dir().display());
                for skill in skills.list() {
                    println!("- {}: {}", skill.name, skill.description);
                }
            }
            continue;
        }
        if let Some(name) = trimmed.strip_prefix("/skill ").map(str::trim) {
            if name.is_empty() {
                println!("用法: /skill <name>");
            } else {
                match skills.get(name) {
                    Ok(content) => println!("{}", content),
                    Err(err) => println!("错误: {}", err),
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }

        messages.push(Message {
            role: "user".to_string(),
            content: Some(trimmed.to_string()),
            tool_call_id: None,
            tool_calls: None,
        });

        run_agent_loop(&client, &llm, &project, &mut messages, &tools, progress.clone()).await?;
        println!();
    }

    Ok(())
}

async fn run_agent_loop(
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

        let url = format!("{}/chat/completions", config.api_base_url.trim_end_matches('/'));
        println!("> [模型] 第 {} 轮", turn + 1);
        let heartbeat = WaitHeartbeat::start(progress.clone(), format!("模型第 {} 轮", turn + 1));

        let response = client
            .post(url)
            .bearer_auth(&config.api_key)
            .json(&request)
            .send()
            .await
            .context("LLM request failed")?;
        drop(heartbeat);

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM request failed with {}: {}", status, body);
        }

        let parsed: ChatResponse = response.json().await.context("failed to parse LLM response")?;
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

fn truncate_for_print(text: &str) -> String {
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

fn tool_definitions() -> Vec<Tool> {
    let mut tools = builtin_tool_definitions();
    tools.extend(project_tool_definitions());
    tools
}

fn handle_tool_call(project: &ProjectContext, call: &ToolCall) -> anyhow::Result<String> {
    if let Some(output) = handle_builtin_tool_call(call)? {
        return Ok(output);
    }
    if let Some(output) = handle_project_tool_call(project, call)? {
        return Ok(output);
    }
    anyhow::bail!("unknown tool: {}", call.function.name)
}

fn detect_repo_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    let path = PathBuf::from(text);
    path.exists().then_some(path)
}

fn llm_timeout_secs() -> u64 {
    std::env::var("S12_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(LLM_TIMEOUT_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::thread;
    use std::time::Duration;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!("{}_{}_{}", name, std::process::id(), now_nanos());
            let path = std::env::temp_dir().join("s12_tests").join(unique);
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

    fn global_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_global() -> MutexGuard<'static, ()> {
        global_lock().lock().unwrap_or_else(|err| err.into_inner())
    }

    fn now_nanos() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos()
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

    fn tool_call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("call-{}", name),
            r#type: "function".to_string(),
            function: rustpilot::openai_compat::ToolCallFunction {
                name: name.to_string(),
                arguments: serde_json::to_string(&arguments).expect("serialize arguments"),
            },
        }
    }

    fn project_context(repo_root: &Path) -> ProjectContext {
        ProjectContext::new(repo_root.to_path_buf()).expect("project context")
    }

    fn wait_for_terminal_output(
        project: &ProjectContext,
        session_id: &str,
        needle: &str,
    ) -> serde_json::Value {
        for _ in 0..40 {
            let output = handle_tool_call(
                project,
                &tool_call(
                    "terminal_read",
                    json!({ "session_id": session_id, "from": 0 }),
                ),
            )
            .expect("terminal_read");
            let parsed: Value = serde_json::from_str(&output).expect("parse terminal_read");
            let data = parsed["data"].as_str().unwrap_or_default();
            if data.contains(needle) {
                return parsed;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for terminal output: {}", needle);
    }

    #[test]
    fn detect_repo_root_finds_parent_repo() {
        let temp = TestDir::new("detect_repo_root");
        init_git_repo(temp.path());
        let nested = temp.path().join("nested").join("child");
        fs::create_dir_all(&nested).expect("create nested");

        let root = detect_repo_root(&nested).expect("detect root");
        assert_eq!(root, temp.path());
    }

    #[test]
    fn llm_timeout_uses_default_when_env_missing() {
        unsafe {
            std::env::remove_var("S12_LLM_TIMEOUT_SECS");
        }
        assert_eq!(llm_timeout_secs(), LLM_TIMEOUT_SECS);
    }

    #[test]
    fn activity_state_renders_current_tool() {
        let progress = new_activity_handle();
        set_activity(&progress, 3, "执行工具中", Some("worktree_create".to_string()));
        let rendered = render_activity(&progress);
        assert!(rendered.contains("阶段: 执行工具中"));
        assert!(rendered.contains("轮次: 3"));
        assert!(rendered.contains("当前工具: worktree_create"));
    }

    #[test]
    fn task_manager_create_bind_and_list() {
        let temp = TestDir::new("task_manager");
        let tasks = TaskManager::new(temp.path().join(".tasks")).expect("tasks");

        let created = tasks.create("refactor auth", "move to service").expect("create");
        let task: TaskRecord = serde_json::from_str(&created).expect("parse task");
        assert_eq!(task.id, 1);
        assert_eq!(task.status, "pending");

        tasks
            .bind_worktree(1, "auth-refactor", "alice")
            .expect("bind");
        let bound: TaskRecord =
            serde_json::from_str(&tasks.get(1).expect("get")).expect("parse bound");
        assert_eq!(bound.worktree, "auth-refactor");
        assert_eq!(bound.owner, "alice");
        assert_eq!(bound.status, "in_progress");

        let listed = tasks.list_all().expect("list");
        assert!(listed.contains("#1: refactor auth"));
        assert!(listed.contains("owner=alice"));
        assert!(listed.contains("wt=auth-refactor"));
    }

    #[test]
    fn event_bus_emits_and_lists_recent() {
        let temp = TestDir::new("event_bus");
        let bus =
            EventBus::new(temp.path().join(".worktrees").join("events.jsonl")).expect("bus");
        bus.emit(
            "worktree.create.before",
            json!({ "id": 7 }),
            json!({ "name": "wt-a" }),
            None,
        )
        .expect("emit before");
        bus.emit(
            "worktree.create.failed",
            json!({ "id": 7 }),
            json!({ "name": "wt-a" }),
            Some("boom".to_string()),
        )
        .expect("emit failed");

        let recent = bus.list_recent(10).expect("recent");
        assert!(recent.contains("worktree.create.before"));
        assert!(recent.contains("worktree.create.failed"));
        assert!(recent.contains("boom"));
    }

    #[test]
    fn tool_errors_are_returned_without_crashing_loop() {
        let temp = TestDir::new("tool_error");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        let call = ToolCall {
            id: "call-1".to_string(),
            r#type: "function".to_string(),
            function: rustpilot::openai_compat::ToolCallFunction {
                name: "task_get".to_string(),
                arguments: serde_json::to_string(&json!({ "task_id": 999 })).expect("args"),
            },
        };
        let output = handle_tool_call(&project, &call).unwrap_err().to_string();
        assert!(output.contains("任务 999 不存在"));
    }

    #[test]
    fn terminal_tools_support_session_lifecycle() {
        let _guard = lock_global();
        reset_terminal_manager().expect("reset terminal manager");
        let temp = TestDir::new("terminal_tool_lifecycle");
        init_git_repo(temp.path());
        let project = project_context(temp.path());
        assert_eq!(project.repo_root(), temp.path());

        let created = handle_tool_call(
            &project,
            &tool_call(
                "terminal_create",
                json!({
                    "cwd": temp.path().display().to_string()
                }),
            ),
        )
        .expect("terminal_create");
        let created: Value = serde_json::from_str(&created).expect("parse terminal_create");
        let session_id = created["id"]
            .as_str()
            .expect("session id")
            .to_string();
        let log_path = PathBuf::from(
            created["log_path"]
                .as_str()
                .expect("session log path"),
        );

        #[cfg(target_os = "windows")]
        let input = "Write-Output 'tool-session-ok'\n";
        #[cfg(not(target_os = "windows"))]
        let input = "printf 'tool-session-ok\\n'\n";

        let write_output = handle_tool_call(
            &project,
            &tool_call(
                "terminal_write",
                json!({
                    "session_id": session_id.clone(),
                    "input": input
                }),
            ),
        )
        .expect("terminal_write");
        assert!(write_output.contains("已写入会话"));

        let read = wait_for_terminal_output(&project, &session_id, "tool-session-ok");
        assert_eq!(read["session_id"].as_str(), Some(session_id.as_str()));
        assert!(
            read["data"]
                .as_str()
                .unwrap_or_default()
                .contains("tool-session-ok")
        );
        let persisted = fs::read_to_string(&log_path).expect("read terminal log");
        assert!(persisted.contains("tool-session-ok"));

        let listed = handle_tool_call(&project, &tool_call("terminal_list", json!({})))
            .expect("terminal_list");
        let listed: Value = serde_json::from_str(&listed).expect("parse terminal_list");
        assert!(listed
            .as_array()
            .expect("list array")
            .iter()
            .any(|item| item["id"].as_str() == Some(session_id.as_str())));
        let listed_item = listed
            .as_array()
            .expect("list array")
            .iter()
            .find(|item| item["id"].as_str() == Some(session_id.as_str()))
            .expect("session in list");
        assert_eq!(listed_item["source"].as_str(), Some("Live"));
        assert_eq!(listed_item["read_only"].as_bool(), Some(false));

        let status = handle_tool_call(
            &project,
            &tool_call("terminal_status", json!({ "session_id": session_id })),
        )
        .expect("terminal_status");
        let status: Value = serde_json::from_str(&status).expect("parse terminal_status");
        assert_eq!(status["id"].as_str(), Some(session_id.as_str()));
        assert_eq!(status["source"].as_str(), Some("Live"));
        assert_eq!(status["read_only"].as_bool(), Some(false));

        let killed = handle_tool_call(
            &project,
            &tool_call("terminal_kill", json!({ "session_id": session_id })),
        )
        .expect("terminal_kill");
        let killed: Value = serde_json::from_str(&killed).expect("parse terminal_kill");
        assert!(killed["state"].get("Exited").and_then(Value::as_i64).is_some());

        reset_terminal_manager().expect("reset terminal manager");
    }

    #[test]
    fn terminal_tools_mark_restored_sessions_read_only() {
        let _guard = lock_global();
        reset_terminal_manager().expect("reset terminal manager");
        let temp = TestDir::new("terminal_tool_restored");
        init_git_repo(temp.path());
        let project = project_context(temp.path());

        let created = handle_tool_call(
            &project,
            &tool_call(
                "terminal_create",
                json!({
                    "cwd": temp.path().display().to_string()
                }),
            ),
        )
        .expect("terminal_create");
        let created: Value = serde_json::from_str(&created).expect("parse terminal_create");
        let session_id = created["id"].as_str().expect("session id").to_string();
        let log_path = PathBuf::from(created["log_path"].as_str().expect("log path"));

        #[cfg(target_os = "windows")]
        let input = "Write-Output 'restored-tool-ok'\n";
        #[cfg(not(target_os = "windows"))]
        let input = "printf 'restored-tool-ok\\n'\n";

        handle_tool_call(
            &project,
            &tool_call(
                "terminal_write",
                json!({
                    "session_id": session_id.clone(),
                    "input": input
                }),
            ),
        )
        .expect("terminal_write");
        let _ = wait_for_terminal_output(&project, &session_id, "restored-tool-ok");
        let _ = handle_tool_call(
            &project,
            &tool_call("terminal_kill", json!({ "session_id": session_id.clone() })),
        )
        .expect("terminal_kill");

        clear_terminal_manager_live_sessions().expect("clear live terminal sessions");

        let listed = handle_tool_call(&project, &tool_call("terminal_list", json!({})))
            .expect("terminal_list");
        let listed: Value = serde_json::from_str(&listed).expect("parse terminal_list");
        let listed_item = listed
            .as_array()
            .expect("list array")
            .iter()
            .find(|item| item["id"].as_str() == Some(session_id.as_str()))
            .expect("restored session in list");
        assert_eq!(listed_item["source"].as_str(), Some("Restored"));
        assert_eq!(listed_item["read_only"].as_bool(), Some(true));

        let status = handle_tool_call(
            &project,
            &tool_call("terminal_status", json!({ "session_id": session_id.clone() })),
        )
        .expect("terminal_status");
        let status: Value = serde_json::from_str(&status).expect("parse terminal_status");
        assert_eq!(status["source"].as_str(), Some("Restored"));
        assert_eq!(status["read_only"].as_bool(), Some(true));

        let write_error = handle_tool_call(
            &project,
            &tool_call(
                "terminal_write",
                json!({
                    "session_id": session_id,
                    "input": "echo should-fail\n"
                }),
            ),
        )
        .unwrap_err()
        .to_string();
        assert!(write_error.contains("restored and read-only"));

        let persisted = fs::read_to_string(log_path).expect("read terminal log");
        assert!(persisted.contains("restored-tool-ok"));

        reset_terminal_manager().expect("reset terminal manager");
    }

    #[test]
    fn truncate_for_print_handles_multibyte_text() {
        let text = "你".repeat(100);
        let truncated = truncate_for_print(&text);
        assert!(truncated.ends_with("..."));
        assert!(!truncated.is_empty());
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn worktree_keep_updates_index_and_logs_event() {
        let _guard = lock_global();
        let temp = TestDir::new("worktree_keep");
        init_git_repo(temp.path());
        let tasks = TaskManager::new(temp.path().join(".tasks")).expect("tasks");
        tasks.create("demo", "").expect("create task");
        let events =
            EventBus::new(temp.path().join(".worktrees").join("events.jsonl")).expect("events");
        let manager = WorktreeManager::new(temp.path().to_path_buf(), tasks.clone(), events.clone())
            .expect("manager");

        manager
            .create("demo-wt", Some(1), "HEAD")
            .expect("create worktree");
        let kept = manager.keep("demo-wt").expect("keep");
        assert!(kept.contains("\"status\": \"kept\""));

        let index = fs::read_to_string(temp.path().join(".worktrees").join("index.json"))
            .expect("read index");
        assert!(index.contains("\"status\": \"kept\""));
        let recent = events.list_recent(10).expect("events");
        assert!(recent.contains("worktree.keep"));
    }

    #[test]
    fn worktree_remove_can_complete_task() {
        let _guard = lock_global();
        let temp = TestDir::new("worktree_remove");
        init_git_repo(temp.path());
        let tasks = TaskManager::new(temp.path().join(".tasks")).expect("tasks");
        tasks.create("implement auth", "").expect("create task");
        let events =
            EventBus::new(temp.path().join(".worktrees").join("events.jsonl")).expect("events");
        let manager = WorktreeManager::new(temp.path().to_path_buf(), tasks.clone(), events.clone())
            .expect("manager");

        manager
            .create("auth-wt", Some(1), "HEAD")
            .expect("create worktree");
        let removed = manager
            .remove("auth-wt", true, true)
            .expect("remove worktree");
        assert_eq!(removed, "已移除 worktree 'auth-wt'");

        let task: TaskRecord =
            serde_json::from_str(&tasks.get(1).expect("get task")).expect("parse task");
        assert_eq!(task.status, "completed");
        assert!(task.worktree.is_empty());

        let index = fs::read_to_string(temp.path().join(".worktrees").join("index.json"))
            .expect("read index");
        assert!(index.contains("\"status\": \"removed\""));
        let recent = events.list_recent(20).expect("events");
        assert!(recent.contains("task.completed"));
        assert!(recent.contains("worktree.remove.after"));
    }
}
