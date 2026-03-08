use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use super::util::now_secs_f64;

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
        self.with_lock(|| {
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
        })
    }

    pub fn get(&self, task_id: u64) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.load(task_id)?)?)
    }

    pub fn exists(&self, task_id: u64) -> bool {
        self.path(task_id).exists()
    }

    pub fn update(
        &self,
        task_id: u64,
        status: Option<&str>,
        owner: Option<&str>,
    ) -> anyhow::Result<String> {
        self.with_lock(|| {
            let mut task = self.load(task_id)?;
            if let Some(status) = status {
                if !matches!(
                    status,
                    "pending" | "in_progress" | "blocked" | "completed" | "failed"
                ) {
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
        })
    }

    pub fn claim_next_pending(&self, owner: &str) -> anyhow::Result<Option<TaskRecord>> {
        self.with_lock(|| {
            let mut tasks = self.load_all()?;
            tasks.sort_by_key(|task| task.id);

            let Some(mut task) = tasks.into_iter().find(|task| task.status == "pending") else {
                return Ok(None);
            };

            task.status = "in_progress".to_string();
            task.owner = owner.to_string();
            task.updated_at = now_secs_f64();
            self.save(&task)?;
            Ok(Some(task))
        })
    }

    pub fn bind_worktree(
        &self,
        task_id: u64,
        worktree: &str,
        owner: &str,
    ) -> anyhow::Result<String> {
        self.with_lock(|| {
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
        })
    }

    pub fn unbind_worktree(&self, task_id: u64) -> anyhow::Result<String> {
        self.with_lock(|| {
            let mut task = self.load(task_id)?;
            task.worktree.clear();
            task.updated_at = now_secs_f64();
            self.save(&task)?;
            Ok(serde_json::to_string_pretty(&task)?)
        })
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
                "blocked" => "[!]",
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

    pub fn pending_count(&self) -> anyhow::Result<usize> {
        self.with_lock(|| {
            Ok(self
                .load_all()?
                .into_iter()
                .filter(|task| task.status == "pending")
                .count())
        })
    }

    pub fn append_user_reply(
        &self,
        task_id: u64,
        reply: &str,
        next_status: &str,
    ) -> anyhow::Result<String> {
        self.with_lock(|| {
            let mut task = self.load(task_id)?;
            let message = reply.trim();
            if message.is_empty() {
                anyhow::bail!("reply 不能为空");
            }
            if !matches!(next_status, "pending" | "in_progress" | "blocked") {
                anyhow::bail!("非法状态: {}", next_status);
            }
            if !task.description.trim().is_empty() {
                task.description.push_str("\n\n");
            }
            task.description
                .push_str(&format!("[USER_REPLY @ {}]\n{}", now_secs_f64(), message));
            task.status = next_status.to_string();
            task.updated_at = now_secs_f64();
            self.save(&task)?;
            Ok(serde_json::to_string_pretty(&task)?)
        })
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
                        anyhow::bail!("获取任务锁超时: {}", lock_path.display());
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
}
