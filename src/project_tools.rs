use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::openai_compat::{Tool, ToolCall, ToolFunction};
use crate::tools::{is_dangerous_command, run_shell_command};

pub fn project_tool_definitions() -> Vec<Tool> {
    vec![
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

pub fn handle_project_tool_call(repo_root: &Path, call: &ToolCall) -> anyhow::Result<Option<String>> {
    let tasks = TaskManager::new(repo_root.join(".tasks"))?;
    let events = EventBus::new(repo_root.join(".worktrees").join("events.jsonl"))?;
    let worktrees = WorktreeManager::new(repo_root.to_path_buf(), tasks.clone(), events.clone())?;

    let output = match call.function.name.as_str() {
        "task_create" => {
            let args: TaskCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_create arguments")?;
            tasks.create(&args.subject, args.description.as_deref().unwrap_or(""))?
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
            tasks.update(args.task_id, args.status.as_deref(), args.owner.as_deref())?
        }
        "task_bind_worktree" => {
            let args: TaskBindArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid task_bind_worktree arguments")?;
            tasks.bind_worktree(args.task_id, &args.worktree, args.owner.as_deref().unwrap_or(""))?
        }
        "worktree_create" => {
            let args: WorktreeCreateArgs = serde_json::from_str(&call.function.arguments)
                .context("invalid worktree_create arguments")?;
            worktrees.create(&args.name, args.task_id, args.base_ref.as_deref().unwrap_or("HEAD"))?
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

fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    event: String,
    ts: f64,
    task: serde_json::Value,
    worktree: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventBus {
    path: PathBuf,
}

impl EventBus {
    pub fn new(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(&path, "")?;
        }
        Ok(Self { path })
    }

    pub fn emit(
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

    pub fn list_recent(&self, limit: usize) -> anyhow::Result<String> {
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
pub struct TaskRecord {
    pub id: u64,
    pub subject: String,
    pub description: String,
    pub status: String,
    pub owner: String,
    pub worktree: String,
    #[serde(rename = "blockedBy")]
    pub blocked_by: Vec<u64>,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone)]
pub struct TaskManager {
    dir: PathBuf,
}

impl TaskManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn create(&self, subject: &str, description: &str) -> anyhow::Result<String> {
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

    pub fn get(&self, task_id: u64) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.load(task_id)?)?)
    }

    pub fn exists(&self, task_id: u64) -> bool {
        self.path(task_id).exists()
    }

    pub fn update(&self, task_id: u64, status: Option<&str>, owner: Option<&str>) -> anyhow::Result<String> {
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

    pub fn bind_worktree(&self, task_id: u64, worktree: &str, owner: &str) -> anyhow::Result<String> {
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

    pub fn unbind_worktree(&self, task_id: u64) -> anyhow::Result<String> {
        let mut task = self.load(task_id)?;
        task.worktree.clear();
        task.updated_at = now_secs_f64();
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    pub fn list_all(&self) -> anyhow::Result<String> {
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
pub struct WorktreeRecord {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub task_id: Option<u64>,
    pub status: String,
    pub created_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed_at: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct WorktreeIndex {
    worktrees: Vec<WorktreeRecord>,
}

#[derive(Debug, Clone)]
pub struct WorktreeManager {
    repo_root: PathBuf,
    tasks: TaskManager,
    events: EventBus,
    dir: PathBuf,
    index_path: PathBuf,
    pub git_available: bool,
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf, tasks: TaskManager, events: EventBus) -> anyhow::Result<Self> {
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

    pub fn create(&self, name: &str, task_id: Option<u64>, base_ref: &str) -> anyhow::Result<String> {
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

    pub fn list_all(&self) -> anyhow::Result<String> {
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

    pub fn status(&self, name: &str) -> anyhow::Result<String> {
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

    pub fn run(&self, name: &str, command: &str) -> anyhow::Result<String> {
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

    pub fn remove(&self, name: &str, force: bool, complete_task: bool) -> anyhow::Result<String> {
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

    pub fn keep(&self, name: &str) -> anyhow::Result<String> {
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
