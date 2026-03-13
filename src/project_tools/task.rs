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
    #[serde(default = "default_task_priority")]
    pub priority: String,
    #[serde(default = "default_task_role_hint")]
    pub role_hint: String,
    pub status: String,
    pub owner: String,
    pub worktree: String,
    #[serde(default)]
    pub parent_task_id: Option<u64>,
    #[serde(default)]
    pub depth: u32,
    #[serde(rename = "blockedBy")]
    pub blocked_by: Vec<u64>,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Default)]
pub struct TaskCreateOptions {
    pub priority: Option<String>,
    pub role_hint: Option<String>,
    pub parent_task_id: Option<u64>,
    pub depth: Option<u32>,
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
        self.create_detailed(subject, description, TaskCreateOptions::default())
    }

    pub fn create_with_priority(
        &self,
        subject: &str,
        description: &str,
        priority: &str,
    ) -> anyhow::Result<String> {
        self.create_detailed(
            subject,
            description,
            TaskCreateOptions {
                priority: Some(priority.to_string()),
                role_hint: Some(infer_task_role_hint(subject, description)),
                ..TaskCreateOptions::default()
            },
        )
    }

    pub fn create_with_priority_and_role(
        &self,
        subject: &str,
        description: &str,
        priority: &str,
        role_hint: &str,
    ) -> anyhow::Result<String> {
        self.create_detailed(
            subject,
            description,
            TaskCreateOptions {
                priority: Some(priority.to_string()),
                role_hint: Some(role_hint.to_string()),
                ..TaskCreateOptions::default()
            },
        )
    }

    pub fn create_detailed(
        &self,
        subject: &str,
        description: &str,
        options: TaskCreateOptions,
    ) -> anyhow::Result<String> {
        self.with_lock(|| {
            let now = now_secs_f64();
            let inferred_role = infer_task_role_hint(subject, description);
            let task = TaskRecord {
                id: self.max_id()? + 1,
                subject: subject.to_string(),
                description: description.to_string(),
                priority: normalize_task_priority(options.priority.as_deref().unwrap_or("medium")),
                role_hint: normalize_task_role_hint(
                    options.role_hint.as_deref().unwrap_or(&inferred_role),
                ),
                status: "pending".to_string(),
                owner: String::new(),
                worktree: String::new(),
                parent_task_id: options.parent_task_id,
                depth: options.depth.unwrap_or(0),
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

    pub fn get_record(&self, task_id: u64) -> anyhow::Result<TaskRecord> {
        self.load(task_id)
    }

    pub fn exists(&self, task_id: u64) -> bool {
        self.path(task_id).exists()
    }

    pub fn update(
        &self,
        task_id: u64,
        status: Option<&str>,
        owner: Option<&str>,
        priority: Option<&str>,
    ) -> anyhow::Result<String> {
        self.with_lock(|| {
            let mut task = self.load(task_id)?;
            if let Some(status) = status {
                if !is_valid_task_status(status) {
                    anyhow::bail!("invalid task status: {}", status);
                }
                task.status = status.to_string();
            }
            if let Some(owner) = owner {
                task.owner = owner.to_string();
            }
            if let Some(priority) = priority {
                task.priority = normalize_task_priority(priority);
            }
            task.updated_at = now_secs_f64();
            self.save(&task)?;
            Ok(serde_json::to_string_pretty(&task)?)
        })
    }

    pub fn claim_next_pending(&self, owner: &str) -> anyhow::Result<Option<TaskRecord>> {
        self.claim_next_pending_with_min_priority(owner, "low")
    }

    pub fn claim_next_pending_with_min_priority(
        &self,
        owner: &str,
        min_priority: &str,
    ) -> anyhow::Result<Option<TaskRecord>> {
        self.with_lock(|| {
            let mut tasks = self.load_all()?;
            tasks.sort_by(|a, b| {
                task_priority_rank(&b.priority)
                    .cmp(&task_priority_rank(&a.priority))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let min_rank = task_priority_rank(min_priority);

            let Some(mut task) = tasks.into_iter().find(|task| {
                task.status == "pending" && task_priority_rank(&task.priority) >= min_rank
            }) else {
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
            return Ok("no tasks".to_string());
        }

        let mut lines = Vec::new();
        for task in tasks {
            let marker = match task.status.as_str() {
                "pending" => "[ ]",
                "in_progress" => "[>]",
                "blocked" => "[!]",
                "paused" => "[||]",
                "cancelled" => "[-]",
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
            let parent = task
                .parent_task_id
                .map(|id| format!(" parent={id}"))
                .unwrap_or_default();
            let depth = if task.depth > 0 {
                format!(" depth={}", task.depth)
            } else {
                String::new()
            };
            let priority = format!(" priority={}", task.priority);
            let role = format!(" role={}", task.role_hint);
            lines.push(format!(
                "{marker} #{}: {}{}{}{}{}{}{}",
                task.id, task.subject, priority, role, owner, worktree, parent, depth
            ));
        }
        Ok(lines.join("\n"))
    }

    pub fn list_records(&self) -> anyhow::Result<Vec<TaskRecord>> {
        let mut tasks = self.load_all()?;
        tasks.sort_by_key(|task| task.id);
        Ok(tasks)
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

    pub fn active_child_count(&self, parent_task_id: Option<u64>) -> anyhow::Result<usize> {
        self.with_lock(|| {
            Ok(self
                .load_all()?
                .into_iter()
                .filter(|task| {
                    task.parent_task_id == parent_task_id
                        && matches!(
                            task.status.as_str(),
                            "pending" | "in_progress" | "blocked" | "paused"
                        )
                })
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
                anyhow::bail!("reply cannot be empty");
            }
            if !matches!(next_status, "pending" | "in_progress" | "blocked" | "paused") {
                anyhow::bail!("invalid task status: {}", next_status);
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

    pub fn has_active_subject(&self, subject: &str) -> anyhow::Result<bool> {
        let subject = subject.trim();
        if subject.is_empty() {
            return Ok(false);
        }
        self.with_lock(|| {
            Ok(self.load_all()?.into_iter().any(|task| {
                task.subject == subject
                    && matches!(
                        task.status.as_str(),
                        "pending" | "in_progress" | "blocked" | "paused"
                    )
            }))
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
                        anyhow::bail!("timed out acquiring task lock: {}", lock_path.display());
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

fn default_task_priority() -> String {
    "medium".to_string()
}

fn default_task_role_hint() -> String {
    "developer".to_string()
}

fn is_valid_task_status(status: &str) -> bool {
    matches!(
        status,
        "pending" | "in_progress" | "blocked" | "paused" | "cancelled" | "completed" | "failed"
    )
}

fn normalize_task_priority(priority: &str) -> String {
    match priority.trim().to_lowercase().as_str() {
        "critical" => "critical".to_string(),
        "high" => "high".to_string(),
        "low" => "low".to_string(),
        _ => "medium".to_string(),
    }
}

fn normalize_task_role_hint(role_hint: &str) -> String {
    match role_hint.trim().to_lowercase().as_str() {
        "critic" => "critic".to_string(),
        "ui" => "ui".to_string(),
        "design" => "design".to_string(),
        _ => "developer".to_string(),
    }
}

fn infer_task_role_hint(subject: &str, description: &str) -> String {
    let text = format!("{} {}", subject, description).to_lowercase();
    if text.contains("design") || text.contains("ui") || text.contains("ux") || text.contains("界面") || text.contains("设计") {
        return "design".to_string();
    }
    if text.contains("proposal")
        || text.contains("reflect")
        || text.contains("reflection")
        || text.contains("review")
        || text.contains("audit")
    {
        return "critic".to_string();
    }
    "developer".to_string()
}

pub fn task_priority_rank(priority: &str) -> u8 {
    match priority {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{TaskCreateOptions, TaskManager};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fresh_tasks_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("rustpilot-{name}-{unique}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn create_detailed_persists_parent_and_depth() {
        let manager = TaskManager::new(fresh_tasks_dir("create")).expect("manager");
        let created = manager
            .create_detailed(
                "child task",
                "delegated",
                TaskCreateOptions {
                    priority: Some("high".to_string()),
                    role_hint: Some("developer".to_string()),
                    parent_task_id: Some(7),
                    depth: Some(2),
                },
            )
            .expect("create");
        let parsed: serde_json::Value = serde_json::from_str(&created).expect("json");
        assert_eq!(parsed["parent_task_id"], 7);
        assert_eq!(parsed["depth"], 2);
        assert_eq!(parsed["priority"], "high");
    }

    #[test]
    fn update_accepts_paused_and_cancelled() {
        let manager = TaskManager::new(fresh_tasks_dir("update")).expect("manager");
        let created = manager.create("task", "").expect("create");
        let task_id = serde_json::from_str::<serde_json::Value>(&created)
            .expect("json")["id"]
            .as_u64()
            .expect("id");

        let paused = manager
            .update(task_id, Some("paused"), None, Some("critical"))
            .expect("pause");
        let paused_json: serde_json::Value = serde_json::from_str(&paused).expect("json");
        assert_eq!(paused_json["status"], "paused");
        assert_eq!(paused_json["priority"], "critical");

        let cancelled = manager
            .update(task_id, Some("cancelled"), None, None)
            .expect("cancel");
        let cancelled_json: serde_json::Value =
            serde_json::from_str(&cancelled).expect("json");
        assert_eq!(cancelled_json["status"], "cancelled");
    }
}
