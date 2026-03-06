use rustpilot::activity::{new_activity_handle, render_activity, set_activity};
use rustpilot::agent::{handle_tool_call, truncate_for_print};
use rustpilot::agent_tools::{clear_terminal_manager_live_sessions, reset_terminal_manager};
use rustpilot::openai_compat::{ToolCall, ToolCallFunction};
use rustpilot::project_tools::{
    EventBus, ProjectContext, TaskManager, TaskRecord, WorktreeManager,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        function: ToolCallFunction {
            name: name.to_string(),
            arguments: serde_json::to_string(&arguments).expect("serialize arguments"),
        },
    }
}

fn project_context(repo_root: &Path) -> ProjectContext {
    ProjectContext::new(repo_root.to_path_buf()).expect("project context")
}

fn wait_for_terminal_output(project: &ProjectContext, session_id: &str, needle: &str) -> Value {
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
fn activity_state_renders_current_tool() {
    let progress = new_activity_handle();
    set_activity(
        &progress,
        3,
        "执行工具中",
        Some("worktree_create".to_string()),
    );
    let rendered = render_activity(&progress);
    assert!(rendered.contains("阶段: 执行工具中"));
    assert!(rendered.contains("轮次: 3"));
    assert!(rendered.contains("当前工具: worktree_create"));
}

#[test]
fn task_manager_create_bind_and_list() {
    let temp = TestDir::new("task_manager");
    let tasks = TaskManager::new(temp.path().join(".tasks")).expect("tasks");

    let created = tasks
        .create("refactor auth", "move to service")
        .expect("create");
    let task: TaskRecord = serde_json::from_str(&created).expect("parse task");
    assert_eq!(task.id, 1);
    assert_eq!(task.status, "pending");

    tasks
        .bind_worktree(1, "auth-refactor", "alice")
        .expect("bind");
    let bound: TaskRecord = serde_json::from_str(&tasks.get(1).expect("get")).expect("parse bound");
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
    let bus = EventBus::new(temp.path().join(".worktrees").join("events.jsonl")).expect("bus");
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
        function: ToolCallFunction {
            name: "task_get".to_string(),
            arguments: serde_json::to_string(&json!({ "task_id": 999 })).expect("args"),
        },
    };
    let output = handle_tool_call(&project, &call).unwrap_err().to_string();
    assert!(output.contains("任务 999 不存在"));
}

#[test]
fn builtin_tool_input_errors_are_classified() {
    let temp = TestDir::new("builtin_input_error");
    init_git_repo(temp.path());
    let project = project_context(temp.path());

    let error = handle_tool_call(&project, &tool_call("read_file", json!({ "max_lines": 3 })))
        .unwrap_err()
        .to_string();

    assert!(error.contains("builtin tool 'read_file' failed [input]"));
}

#[test]
fn builtin_tool_filesystem_errors_are_classified() {
    let temp = TestDir::new("builtin_fs_error");
    init_git_repo(temp.path());
    let project = project_context(temp.path());
    let file_path = temp.path().join("sample.txt");
    fs::write(&file_path, "hello\n").expect("write sample file");

    let error = handle_tool_call(
        &project,
        &tool_call(
            "edit_file",
            json!({
                "path": file_path.display().to_string(),
                "old": "missing",
                "new": "updated"
            }),
        ),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("builtin tool 'edit_file' failed [filesystem]"));
    assert!(error.contains("未找到目标文本"));
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
    let session_id = created["id"].as_str().expect("session id").to_string();
    let log_path = PathBuf::from(created["log_path"].as_str().expect("session log path"));

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

    let listed =
        handle_tool_call(&project, &tool_call("terminal_list", json!({}))).expect("terminal_list");
    let listed: Value = serde_json::from_str(&listed).expect("parse terminal_list");
    assert!(
        listed
            .as_array()
            .expect("list array")
            .iter()
            .any(|item| item["id"].as_str() == Some(session_id.as_str()))
    );
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
    assert!(
        killed["state"]
            .get("Exited")
            .and_then(Value::as_i64)
            .is_some()
    );

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

    let listed =
        handle_tool_call(&project, &tool_call("terminal_list", json!({}))).expect("terminal_list");
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
        &tool_call(
            "terminal_status",
            json!({ "session_id": session_id.clone() }),
        ),
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

    let index =
        fs::read_to_string(temp.path().join(".worktrees").join("index.json")).expect("read index");
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

    let index =
        fs::read_to_string(temp.path().join(".worktrees").join("index.json")).expect("read index");
    assert!(index.contains("\"status\": \"removed\""));
    let recent = events.list_recent(20).expect("events");
    assert!(recent.contains("task.completed"));
    assert!(recent.contains("worktree.remove.after"));
}
