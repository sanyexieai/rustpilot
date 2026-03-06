use anyhow::Context;
use rustpilot::config::LlmConfig;
use rustpilot::openai_compat::{
    ChatRequest, ChatResponse, Message, Tool, ToolCall, ToolChoice, ToolFunction,
};
use rustpilot::tools::{
    edit_file, is_dangerous_command, read_file, run_shell_command, write_file, BashArgs,
    BashTool, EditFileArgs, ReadFileArgs, WriteFileArgs,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use std::sync::{MutexGuard, OnceLock};

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

    let tasks = TaskManager::new(repo_root.join(".tasks"))?;
    let events = EventBus::new(repo_root.join(".worktrees").join("events.jsonl"))?;
    let worktrees = WorktreeManager::new(repo_root.clone(), tasks.clone(), events.clone())?;
    let progress = Arc::new(Mutex::new(ActivityState::idle()));

    println!("s12 仓库根目录: {}", repo_root.display());
    if !worktrees.git_available {
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
            println!("{}", tasks.list_all()?);
            continue;
        }
        if trimmed == "/worktrees" {
            println!("{}", worktrees.list_all()?);
            continue;
        }
        if trimmed == "/events" {
            println!("{}", events.list_recent(20)?);
            continue;
        }
        if trimmed == "/status" {
            println!("{}", render_activity(&progress));
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

        run_agent_loop(&client, &llm, &repo_root, &mut messages, &tools, progress.clone()).await?;
        println!();
    }

    Ok(())
}

async fn run_agent_loop(
    client: &reqwest::Client,
    config: &LlmConfig,
    repo_root: &Path,
    messages: &mut Vec<Message>,
    tools: &[Tool],
    progress: Arc<Mutex<ActivityState>>,
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
            let output = match handle_tool_call(repo_root, &call) {
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
    vec![
        tool(
            "bash",
            "在当前工作目录执行 shell 命令。",
            json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
        ),
        tool(
            "read_file",
            "读取文件内容。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "max_lines": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "write_file",
            "写入文件。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        tool(
            "edit_file",
            "替换文件中的精确文本。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old": { "type": "string" },
                    "new": { "type": "string" }
                },
                "required": ["path", "old", "new"]
            }),
        ),
        tool(
            "task_create",
            "在共享任务板上创建任务。",
            json!({
                "type": "object",
                "properties": {
                    "subject": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["subject"]
            }),
        ),
        tool(
            "task_list",
            "列出所有任务及其 owner/worktree。",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "task_get",
            "按 ID 获取任务详情。",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "integer" } },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_update",
            "更新任务状态或 owner。",
            json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "integer" },
                    "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                    "owner": { "type": "string" }
                },
                "required": ["task_id"]
            }),
        ),
        tool(
            "task_bind_worktree",
            "将任务绑定到一个 worktree 名称。",
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
            "创建 git worktree 并可选绑定任务。",
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
            "列出 .worktrees/index.json 中的 worktree。",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "worktree_status",
            "查看某个 worktree 的 git 状态。",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_run",
            "在指定 worktree 中执行命令。",
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
            "将 worktree 标记为保留。",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
        ),
        tool(
            "worktree_remove",
            "移除 worktree，可选同时完成任务。",
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
            "查看最近的 worktree 生命周期事件。",
            json!({
                "type": "object",
                "properties": { "limit": { "type": "integer" } }
            }),
        ),
    ]
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

fn handle_tool_call(repo_root: &Path, call: &ToolCall) -> anyhow::Result<String> {
    let tasks = TaskManager::new(repo_root.join(".tasks"))?;
    let events = EventBus::new(repo_root.join(".worktrees").join("events.jsonl"))?;
    let worktrees = WorktreeManager::new(repo_root.to_path_buf(), tasks.clone(), events.clone())?;

    match call.function.name.as_str() {
        "bash" => {
            let args: BashArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid bash arguments")?;
            BashTool::run(&args.command)
        }
        "read_file" => {
            let args: ReadFileArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid read_file arguments")?;
            read_file(&args)
        }
        "write_file" => {
            let args: WriteFileArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid write_file arguments")?;
            write_file(&args)
        }
        "edit_file" => {
            let args: EditFileArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid edit_file arguments")?;
            edit_file(&args)
        }
        "task_create" => {
            let args: TaskCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_create arguments")?;
            tasks.create(&args.subject, args.description.as_deref().unwrap_or(""))
        }
        "task_list" => tasks.list_all(),
        "task_get" => {
            let args: TaskIdArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_get arguments")?;
            tasks.get(args.task_id)
        }
        "task_update" => {
            let args: TaskUpdateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_update arguments")?;
            tasks.update(args.task_id, args.status.as_deref(), args.owner.as_deref())
        }
        "task_bind_worktree" => {
            let args: TaskBindArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_bind_worktree arguments")?;
            tasks.bind_worktree(args.task_id, &args.worktree, args.owner.as_deref().unwrap_or(""))
        }
        "worktree_create" => {
            let args: WorktreeCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_create arguments")?;
            worktrees.create(&args.name, args.task_id, args.base_ref.as_deref().unwrap_or("HEAD"))
        }
        "worktree_list" => worktrees.list_all(),
        "worktree_status" => {
            let args: WorktreeNameArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_status arguments")?;
            worktrees.status(&args.name)
        }
        "worktree_run" => {
            let args: WorktreeRunArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_run arguments")?;
            worktrees.run(&args.name, &args.command)
        }
        "worktree_keep" => {
            let args: WorktreeNameArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_keep arguments")?;
            worktrees.keep(&args.name)
        }
        "worktree_remove" => {
            let args: WorktreeRemoveArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_remove arguments")?;
            worktrees.remove(
                &args.name,
                args.force.unwrap_or(false),
                args.complete_task.unwrap_or(false),
            )
        }
        "worktree_events" => {
            let args: EventsArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_events arguments")?;
            events.list_recent(args.limit.unwrap_or(20))
        }
        _ => anyhow::bail!("unknown tool: {}", call.function.name),
    }
}

#[derive(Debug, Deserialize)]
struct TaskCreateArgs {
    subject: String,
    #[serde(default)]
    description: Option<String>,
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

fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone)]
struct ActivityState {
    round: usize,
    stage: String,
    active_tool: Option<String>,
    last_update: f64,
}

impl ActivityState {
    fn idle() -> Self {
        Self {
            round: 0,
            stage: "空闲".to_string(),
            active_tool: None,
            last_update: now_secs_f64(),
        }
    }
}

fn set_activity(
    progress: &Arc<Mutex<ActivityState>>,
    round: usize,
    stage: &str,
    active_tool: Option<String>,
) {
    if let Ok(mut state) = progress.lock() {
        state.round = round;
        state.stage = stage.to_string();
        state.active_tool = active_tool;
        state.last_update = now_secs_f64();
    }
}

fn render_activity(progress: &Arc<Mutex<ActivityState>>) -> String {
    match progress.lock() {
        Ok(state) => {
            let age = (now_secs_f64() - state.last_update).max(0.0);
            let tool = state
                .active_tool
                .as_ref()
                .map(|name| format!("\n当前工具: {}", name))
                .unwrap_or_default();
            format!(
                "阶段: {}\n轮次: {}\n距上次更新秒数: {:.1}{}",
                state.stage, state.round, age, tool
            )
        }
        Err(_) => "阶段: 未知\n错误: 活动状态锁已损坏".to_string(),
    }
}

struct WaitHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl WaitHeartbeat {
    fn start(progress: Arc<Mutex<ActivityState>>, label: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = thread::spawn(move || {
            let started = now_secs_f64();
            while !stop_flag.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(5));
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let elapsed = (now_secs_f64() - started).max(0.0);
                println!(
                    "> [心跳] {} 仍在运行，已持续 {:.1}s\n{}",
                    label,
                    elapsed,
                    render_activity(&progress)
                );
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for WaitHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn llm_timeout_secs() -> u64 {
    std::env::var("S12_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(LLM_TIMEOUT_SECS)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EventRecord {
    event: String,
    ts: f64,
    task: serde_json::Value,
    worktree: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct EventBus {
    path: PathBuf,
}

impl EventBus {
    fn new(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(&path, "")?;
        }
        Ok(Self { path })
    }

    fn emit(
        &self,
        event: &str,
        task: serde_json::Value,
        worktree: serde_json::Value,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let payload = EventRecord {
            event: event.to_string(),
            ts: now_secs_f64(),
            task,
            worktree,
            error,
        };
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&payload)?)?;
        Ok(())
    }

    fn list_recent(&self, limit: usize) -> anyhow::Result<String> {
        let count = limit.clamp(1, 200);
        let content = fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = content.lines().collect();
        let mut items = Vec::new();
        for line in lines
            .into_iter()
            .rev()
            .take(count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => items.push(value),
                Err(_) => items.push(json!({ "event": "parse_error", "raw": line })),
            }
        }
        Ok(serde_json::to_string_pretty(&items)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TaskRecord {
    id: u64,
    subject: String,
    description: String,
    status: String,
    owner: String,
    worktree: String,
    #[serde(rename = "blockedBy")]
    blocked_by: Vec<u64>,
    created_at: f64,
    updated_at: f64,
}

#[derive(Debug, Clone)]
struct TaskManager {
    dir: PathBuf,
}

impl TaskManager {
    fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn create(&self, subject: &str, description: &str) -> anyhow::Result<String> {
        let now = now_secs_f64();
        let task = TaskRecord {
            id: self.max_id()? + 1,
            subject: subject.to_string(),
            description: description.to_string(),
            status: "pending".to_string(),
            owner: String::new(),
            worktree: String::new(),
            blocked_by: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    fn get(&self, task_id: u64) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.load(task_id)?)?)
    }

    fn exists(&self, task_id: u64) -> bool {
        self.path(task_id).exists()
    }

    fn update(&self, task_id: u64, status: Option<&str>, owner: Option<&str>) -> anyhow::Result<String> {
        let mut task = self.load(task_id)?;
        if let Some(status) = status {
            if !matches!(status, "pending" | "in_progress" | "completed") {
                anyhow::bail!("非法状态: {}", status);
            }
            task.status = status.to_string();
        }
        if let Some(owner) = owner {
            task.owner = owner.to_string();
        }
        task.updated_at = now_secs_f64();
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    fn bind_worktree(&self, task_id: u64, worktree: &str, owner: &str) -> anyhow::Result<String> {
        let mut task = self.load(task_id)?;
        task.worktree = worktree.to_string();
        if !owner.is_empty() {
            task.owner = owner.to_string();
        }
        if task.status == "pending" {
            task.status = "in_progress".to_string();
        }
        task.updated_at = now_secs_f64();
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    fn unbind_worktree(&self, task_id: u64) -> anyhow::Result<String> {
        let mut task = self.load(task_id)?;
        task.worktree.clear();
        task.updated_at = now_secs_f64();
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    fn list_all(&self) -> anyhow::Result<String> {
        let mut tasks = self.load_all()?;
        tasks.sort_by_key(|task| task.id);
        if tasks.is_empty() {
            return Ok("没有任务。".to_string());
        }

        let mut lines = Vec::new();
        for task in tasks {
            let marker = match task.status.as_str() {
                "pending" => "[ ]",
                "in_progress" => "[>]",
                "completed" => "[x]",
                _ => "[?]",
            };
            let owner = if task.owner.is_empty() {
                String::new()
            } else {
                format!(" owner={}", task.owner)
            };
            let worktree = if task.worktree.is_empty() {
                String::new()
            } else {
                format!(" wt={}", task.worktree)
            };
            lines.push(format!(
                "{marker} #{}: {}{}{}",
                task.id, task.subject, owner, worktree
            ));
        }
        Ok(lines.join("\n"))
    }

    fn path(&self, task_id: u64) -> PathBuf {
        self.dir.join(format!("task_{}.json", task_id))
    }

    fn max_id(&self) -> anyhow::Result<u64> {
        let mut max_id = 0u64;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(id) = stem
                .strip_prefix("task_")
                .and_then(|value| value.parse::<u64>().ok())
            {
                max_id = max_id.max(id);
            }
        }
        Ok(max_id)
    }

    fn load(&self, task_id: u64) -> anyhow::Result<TaskRecord> {
        let path = self.path(task_id);
        if !path.exists() {
            anyhow::bail!("任务 {} 不存在", task_id);
        }
        let content = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    fn save(&self, task: &TaskRecord) -> anyhow::Result<()> {
        fs::write(self.path(task.id), serde_json::to_string_pretty(task)?)?;
        Ok(())
    }

    fn load_all(&self) -> anyhow::Result<Vec<TaskRecord>> {
        let mut tasks = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !name.starts_with("task_") || !name.ends_with(".json") {
                continue;
            }
            let content = fs::read_to_string(path)?;
            tasks.push(serde_json::from_str(&content)?);
        }
        Ok(tasks)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct WorktreeRecord {
    name: String,
    path: String,
    branch: String,
    task_id: Option<u64>,
    status: String,
    created_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kept_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct WorktreeIndex {
    worktrees: Vec<WorktreeRecord>,
}

#[derive(Debug, Clone)]
struct WorktreeManager {
    repo_root: PathBuf,
    tasks: TaskManager,
    events: EventBus,
    dir: PathBuf,
    index_path: PathBuf,
    git_available: bool,
}

impl WorktreeManager {
    fn new(repo_root: PathBuf, tasks: TaskManager, events: EventBus) -> anyhow::Result<Self> {
        let dir = repo_root.join(".worktrees");
        fs::create_dir_all(&dir)?;
        let index_path = dir.join("index.json");
        if !index_path.exists() {
            fs::write(
                &index_path,
                serde_json::to_string_pretty(&WorktreeIndex::default())?,
            )?;
        }
        let git_available = is_git_repo(&repo_root);
        Ok(Self {
            repo_root,
            tasks,
            events,
            dir,
            index_path,
            git_available,
        })
    }

    fn create(&self, name: &str, task_id: Option<u64>, base_ref: &str) -> anyhow::Result<String> {
        self.validate_name(name)?;
        if self.find(name)?.is_some() {
            anyhow::bail!("索引中已存在 worktree '{}'", name);
        }
        if let Some(task_id) = task_id {
            if !self.tasks.exists(task_id) {
                anyhow::bail!("任务 {} 不存在", task_id);
            }
        }

        let path = self.dir.join(name);
        let branch = format!("wt/{}", name);
        let task_payload = task_id
            .map(|id| json!({ "id": id }))
            .unwrap_or_else(|| json!({}));
        self.events.emit(
            "worktree.create.before",
            task_payload.clone(),
            json!({ "name": name, "base_ref": base_ref }),
            None,
        )?;

        if let Err(err) = self.run_git(&[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_string_lossy().as_ref(),
            base_ref,
        ]) {
            self.events.emit(
                "worktree.create.failed",
                task_payload,
                json!({ "name": name, "base_ref": base_ref }),
                Some(err.to_string()),
            )?;
            return Err(err);
        }

        let record = WorktreeRecord {
            name: name.to_string(),
            path: path.display().to_string(),
            branch,
            task_id,
            status: "active".to_string(),
            created_at: now_secs_f64(),
            removed_at: None,
            kept_at: None,
        };

        let mut index = self.load_index()?;
        index.worktrees.push(record.clone());
        self.save_index(&index)?;
        if let Some(task_id) = task_id {
            self.tasks.bind_worktree(task_id, name, "")?;
        }

        self.events.emit(
            "worktree.create.after",
            record
                .task_id
                .map(|id| json!({ "id": id }))
                .unwrap_or_else(|| json!({})),
            json!({
                "name": record.name,
                "path": record.path,
                "branch": record.branch,
                "status": record.status,
            }),
            None,
        )?;
        Ok(serde_json::to_string_pretty(&record)?)
    }

    fn list_all(&self) -> anyhow::Result<String> {
        let index = self.load_index()?;
        if index.worktrees.is_empty() {
            return Ok("索引中没有 worktree。".to_string());
        }

        let mut lines = Vec::new();
        for wt in index.worktrees {
            let suffix = wt
                .task_id
                .map(|id| format!(" task={}", id))
                .unwrap_or_default();
            lines.push(format!(
                "[{}] {} -> {} ({}){}",
                wt.status, wt.name, wt.path, wt.branch, suffix
            ));
        }
        Ok(lines.join("\n"))
    }

    fn status(&self, name: &str) -> anyhow::Result<String> {
        let wt = self
            .find(name)?
            .ok_or_else(|| anyhow::anyhow!("未知的 worktree '{}'", name))?;
        let path = PathBuf::from(&wt.path);
        if !path.exists() {
            anyhow::bail!("worktree 路径不存在: {}", path.display());
        }

        let output = Command::new("git")
            .args(["status", "--short", "--branch"])
            .current_dir(&path)
            .output()?;
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let text = text.trim();
        Ok(if text.is_empty() {
            "worktree 干净".to_string()
        } else {
            text.to_string()
        })
    }

    fn run(&self, name: &str, command: &str) -> anyhow::Result<String> {
        if is_dangerous_command(command) {
            return Ok("错误: 已拦截危险命令".to_string());
        }

        let wt = self
            .find(name)?
            .ok_or_else(|| anyhow::anyhow!("未知的 worktree '{}'", name))?;
        let path = PathBuf::from(&wt.path);
        if !path.exists() {
            anyhow::bail!("worktree 路径不存在: {}", path.display());
        }

        run_shell_command(command, Some(&path))
    }

    fn remove(&self, name: &str, force: bool, complete_task: bool) -> anyhow::Result<String> {
        let wt = self
            .find(name)?
            .ok_or_else(|| anyhow::anyhow!("未知的 worktree '{}'", name))?;

        self.events.emit(
            "worktree.remove.before",
            wt.task_id
                .map(|id| json!({ "id": id }))
                .unwrap_or_else(|| json!({})),
            json!({ "name": wt.name, "path": wt.path }),
            None,
        )?;

        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&wt.path);

        if let Err(err) = self.run_git(&args) {
            self.events.emit(
                "worktree.remove.failed",
                wt.task_id
                    .map(|id| json!({ "id": id }))
                    .unwrap_or_else(|| json!({})),
                json!({ "name": wt.name, "path": wt.path }),
                Some(err.to_string()),
            )?;
            return Err(err);
        }

        if complete_task {
            if let Some(task_id) = wt.task_id {
                let before: TaskRecord = serde_json::from_str(&self.tasks.get(task_id)?)?;
                self.tasks.update(task_id, Some("completed"), None)?;
                self.tasks.unbind_worktree(task_id)?;
                self.events.emit(
                    "task.completed",
                    json!({
                        "id": task_id,
                        "subject": before.subject,
                        "status": "completed",
                    }),
                    json!({ "name": wt.name }),
                    None,
                )?;
            }
        }

        let mut index = self.load_index()?;
        for item in &mut index.worktrees {
            if item.name == name {
                item.status = "removed".to_string();
                item.removed_at = Some(now_secs_f64());
            }
        }
        self.save_index(&index)?;

        self.events.emit(
            "worktree.remove.after",
            wt.task_id
                .map(|id| json!({ "id": id }))
                .unwrap_or_else(|| json!({})),
            json!({ "name": wt.name, "path": wt.path, "status": "removed" }),
            None,
        )?;

        Ok(format!("已移除 worktree '{}'", name))
    }

    fn keep(&self, name: &str) -> anyhow::Result<String> {
        let mut index = self.load_index()?;
        let mut kept = None;
        for item in &mut index.worktrees {
            if item.name == name {
                item.status = "kept".to_string();
                item.kept_at = Some(now_secs_f64());
                kept = Some(item.clone());
            }
        }
        let kept = kept.ok_or_else(|| anyhow::anyhow!("未知的 worktree '{}'", name))?;
        self.save_index(&index)?;

        self.events.emit(
            "worktree.keep",
            kept.task_id
                .map(|id| json!({ "id": id }))
                .unwrap_or_else(|| json!({})),
            json!({ "name": kept.name, "path": kept.path, "status": kept.status }),
            None,
        )?;
        Ok(serde_json::to_string_pretty(&kept)?)
    }

    fn find(&self, name: &str) -> anyhow::Result<Option<WorktreeRecord>> {
        Ok(self
            .load_index()?
            .worktrees
            .into_iter()
            .find(|record| record.name == name))
    }

    fn load_index(&self) -> anyhow::Result<WorktreeIndex> {
        Ok(serde_json::from_str(&fs::read_to_string(&self.index_path)?)?)
    }

    fn save_index(&self, index: &WorktreeIndex) -> anyhow::Result<()> {
        fs::write(&self.index_path, serde_json::to_string_pretty(index)?)?;
        Ok(())
    }

    fn validate_name(&self, name: &str) -> anyhow::Result<()> {
        let valid = !name.is_empty()
            && name.len() <= 40
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
        if !valid {
            anyhow::bail!("非法的 worktree 名称。请使用 1-40 个字符：字母、数字、.、_、-");
        }
        Ok(())
    }

    fn run_git(&self, args: &[&str]) -> anyhow::Result<String> {
        if !self.git_available {
            anyhow::bail!("当前目录不是 git 仓库，worktree 工具需要 git。");
        }
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo_root)
            .output()?;
        if !output.status.success() {
            let mut text = String::new();
            text.push_str(&String::from_utf8_lossy(&output.stdout));
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            anyhow::bail!("{}", text.trim());
        }
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let text = text.trim();
        Ok(if text.is_empty() {
            "(no output)".to_string()
        } else {
            text.to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let progress = Arc::new(Mutex::new(ActivityState::idle()));
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
        let call = ToolCall {
            id: "call-1".to_string(),
            r#type: "function".to_string(),
            function: rustpilot::openai_compat::ToolCallFunction {
                name: "task_get".to_string(),
                arguments: serde_json::to_string(&json!({ "task_id": 999 })).expect("args"),
            },
        };
        let output = handle_tool_call(temp.path(), &call).unwrap_err().to_string();
        assert!(output.contains("任务 999 不存在"));
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
