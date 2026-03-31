use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::shell_file_tools::{
    is_dangerous_command, is_likely_expensive_command, is_likely_long_running_command,
    run_shell_command,
};

use super::event::EventBus;
use super::task::{TaskManager, TaskRecord};
use super::util::{is_git_repo, now_secs_f64};

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

fn reject_blocking_parent_worktree_run(command: &str) -> anyhow::Result<()> {
    if !current_node_is_parent()? {
        return Ok(());
    }
    if is_likely_long_running_command(command) {
        anyhow::bail!(
            "worktree_run refused: long-running commands must be delegated to a child agent/worker-owned terminal session; use delegate_long_running instead. Suggested recovery: call delegate_long_running with a concise goal plus this command: `{}`",
            command.trim()
        );
    }
    if is_likely_expensive_command(command) {
        anyhow::bail!(
            "worktree_run refused: expensive commands must be delegated when this node is acting as a parent; create a child task instead of blocking the parent. Suggested recovery: call task_create with a concise task/deliverable for this command: `{}`",
            command.trim()
        );
    }
    Ok(())
}

fn current_node_is_parent() -> anyhow::Result<bool> {
    let agent_id = std::env::var("RUSTPILOT_AGENT_ID").unwrap_or_else(|_| "lead".to_string());
    let current_task_id = std::env::var("RUSTPILOT_TASK_ID")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    if current_task_id.is_none() && agent_id == "lead" {
        return Ok(true);
    }
    let repo_root = std::env::var("RUSTPILOT_REPO_ROOT")
        .ok()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let tasks_dir = repo_root.join(".tasks");
    if !tasks_dir.is_dir() {
        return Ok(agent_id == "lead");
    }
    let tasks = TaskManager::new(tasks_dir)?;
    Ok(tasks.active_child_count(current_task_id)? > 0)
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
    pub fn new(
        repo_root: PathBuf,
        dir: PathBuf,
        tasks: TaskManager,
        events: EventBus,
    ) -> anyhow::Result<Self> {
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

    pub fn create(
        &self,
        name: &str,
        task_id: Option<u64>,
        base_ref: &str,
    ) -> anyhow::Result<String> {
        self.validate_name(name)?;
        if let Some(task_id) = task_id
            && !self.tasks.exists(task_id)
        {
            anyhow::bail!("任务 {} 不存在", task_id);
        }

        // All index checks and writes happen inside the lock to prevent
        // concurrent agents from creating multiple worktrees simultaneously.
        let record = self.with_lock(|| {
            if self.find(name)?.is_some() {
                anyhow::bail!("索引中已存在 worktree '{}'", name);
            }
            if let Some(active) = self.find_active()? {
                anyhow::bail!(
                    "worktree_create refused: active worktree '{}' already exists (path: {}, branch: {}). \
                     Each user request uses exactly one worktree. Do not create additional worktrees \
                     for follow-up work or supplemental tasks — use the existing worktree instead. \
                     Run worktree_run with name='{}' to execute commands in it.",
                    active.name, active.path, active.branch, active.name
                );
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

            if let Err(err) = self.add_worktree_with_branch_fallback(&path, &branch, base_ref) {
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
            Ok(record)
        })?;

        if let Some(task_id) = task_id {
            self.tasks.bind_worktree(task_id, &record.name, "")?;
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

    fn with_lock<T>(&self, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let lock_path = self.dir.join(".lock");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => break,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("timed out acquiring worktree lock: {}", lock_path.display());
                    }
                    thread::sleep(Duration::from_millis(30));
                }
                Err(err) => return Err(err.into()),
            }
        }
        let result = f();
        let _ = fs::remove_file(lock_path);
        result
    }

    fn add_worktree_with_branch_fallback(
        &self,
        path: &std::path::Path,
        branch: &str,
        base_ref: &str,
    ) -> anyhow::Result<String> {
        let first_try = self.run_git(&[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_string_lossy().as_ref(),
            base_ref,
        ]);
        if let Ok(output) = first_try {
            return Ok(output);
        }
        let first_err = first_try.unwrap_err();
        let first_text = first_err.to_string();

        if first_text.contains("already exists") || first_text.contains("已经存在") {
            let second_try =
                self.run_git(&["worktree", "add", path.to_string_lossy().as_ref(), branch]);
            if let Ok(output) = second_try {
                return Ok(output);
            }
            let second_err = second_try.unwrap_err();
            let second_text = second_err.to_string();
            if Self::is_missing_registered_worktree_error(&second_text) {
                let _ = self.run_git(&["worktree", "prune"]);
                return self.run_git(&[
                    "worktree",
                    "add",
                    "-f",
                    path.to_string_lossy().as_ref(),
                    branch,
                ]);
            }
            return Err(second_err);
        }

        if Self::is_missing_registered_worktree_error(&first_text) {
            let _ = self.run_git(&["worktree", "prune"]);
            return self.run_git(&[
                "worktree",
                "add",
                "-f",
                "-b",
                branch,
                path.to_string_lossy().as_ref(),
                base_ref,
            ]);
        }

        Err(first_err)
    }

    fn is_missing_registered_worktree_error(text: &str) -> bool {
        (text.contains("丢失") && text.contains("注册的工作区"))
            || text.contains("丢失但已经注册")
            || text.contains("missing but already registered")
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

        reject_blocking_parent_worktree_run(command)?;
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

        if complete_task && let Some(task_id) = wt.task_id {
            let before: TaskRecord = serde_json::from_str(&self.tasks.get(task_id)?)?;
            self.tasks.update(task_id, Some("completed"), None, None)?;
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

    pub fn find_active(&self) -> anyhow::Result<Option<WorktreeRecord>> {
        Ok(self
            .load_index()?
            .worktrees
            .into_iter()
            .find(|record| record.status == "active"))
    }

    fn load_index(&self) -> anyhow::Result<WorktreeIndex> {
        Ok(serde_json::from_str(&fs::read_to_string(
            &self.index_path,
        )?)?)
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
