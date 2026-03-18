use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime};

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub launch_id: String,
    pub agent_id: String,
    pub role: String,
    pub kind: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub task_id: Option<u64>,
    #[serde(default)]
    pub parent_task_id: Option<u64>,
    #[serde(default)]
    pub parent_agent_id: Option<String>,
    #[serde(default)]
    pub max_parallel: Option<usize>,
    pub status: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub process_started_at: Option<f64>,
    #[serde(default)]
    pub window_title: String,
    #[serde(default)]
    pub log_path: String,
    #[serde(default = "default_window_channel")]
    pub channel: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub created_at: f64,
    pub updated_at: f64,
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub stopped_at: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct LaunchRequest {
    pub agent_id: String,
    pub role: String,
    pub kind: String,
    pub owner: Option<String>,
    pub task_id: Option<u64>,
    pub parent_task_id: Option<u64>,
    pub parent_agent_id: Option<String>,
    pub max_parallel: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct LaunchRegistryManager {
    path: PathBuf,
}

impl LaunchRegistryManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("launch_registry.json"),
        })
    }

    pub fn list(&self) -> anyhow::Result<Vec<LaunchRecord>> {
        let _guard = FileLock::acquire(self.path.clone())?;
        self.load_unlocked()
    }

    pub fn get(&self, launch_id: &str) -> anyhow::Result<Option<LaunchRecord>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|item| item.launch_id == launch_id))
    }

    pub fn request(&self, request: LaunchRequest) -> anyhow::Result<LaunchRecord> {
        let _guard = FileLock::acquire(self.path.clone())?;
        let mut items = self.load_unlocked()?;
        let now = now_secs_f64();
        let launch_id = format!(
            "{}-{}-{}",
            request.kind.trim(),
            request.agent_id.trim(),
            now.to_bits()
        );
        let log_path = self.log_path_for(&launch_id).display().to_string();
        let record = LaunchRecord {
            launch_id,
            agent_id: request.agent_id.trim().to_string(),
            role: request.role.trim().to_string(),
            kind: request.kind.trim().to_string(),
            owner: request
                .owner
                .unwrap_or_else(|| request.agent_id.trim().to_string()),
            task_id: request.task_id,
            parent_task_id: request.parent_task_id,
            parent_agent_id: request.parent_agent_id,
            max_parallel: request.max_parallel,
            status: "requested".to_string(),
            pid: None,
            process_started_at: None,
            window_title: String::new(),
            log_path,
            channel: default_window_channel(),
            target: String::new(),
            error: None,
            exit_code: None,
            created_at: now,
            updated_at: now,
            started_at: None,
            stopped_at: None,
        };
        items.push(record.clone());
        self.save_unlocked(&items)?;
        Ok(record)
    }

    pub fn update_running(
        &self,
        launch_id: &str,
        pid: u32,
        process_started_at: Option<f64>,
        channel: &str,
        target: &str,
        window_title: &str,
    ) -> anyhow::Result<Option<LaunchRecord>> {
        let _guard = FileLock::acquire(self.path.clone())?;
        let mut items = self.load_unlocked()?;
        let now = now_secs_f64();
        let mut updated = None;
        for item in &mut items {
            if item.launch_id == launch_id {
                item.status = "running".to_string();
                item.pid = Some(pid);
                item.process_started_at = process_started_at;
                item.channel = channel.trim().to_string();
                item.window_title = window_title.trim().to_string();
                item.target = if !target.trim().is_empty() {
                    target.trim().to_string()
                } else if item.window_title.is_empty() {
                    format!("pid:{pid}")
                } else {
                    item.window_title.clone()
                };
                item.error = None;
                item.started_at = Some(now);
                item.updated_at = now;
                updated = Some(item.clone());
                break;
            }
        }
        self.save_unlocked(&items)?;
        Ok(updated)
    }

    pub fn update_failed(
        &self,
        launch_id: &str,
        error: &str,
    ) -> anyhow::Result<Option<LaunchRecord>> {
        let _guard = FileLock::acquire(self.path.clone())?;
        let mut items = self.load_unlocked()?;
        let now = now_secs_f64();
        let mut updated = None;
        for item in &mut items {
            if item.launch_id == launch_id {
                item.status = "failed".to_string();
                item.error = Some(error.trim().to_string());
                item.process_started_at = None;
                item.updated_at = now;
                item.stopped_at = Some(now);
                updated = Some(item.clone());
                break;
            }
        }
        self.save_unlocked(&items)?;
        Ok(updated)
    }

    pub fn mark_stopped(
        &self,
        launch_id: &str,
        exit_code: Option<i32>,
    ) -> anyhow::Result<Option<LaunchRecord>> {
        let _guard = FileLock::acquire(self.path.clone())?;
        let mut items = self.load_unlocked()?;
        let now = now_secs_f64();
        let mut updated = None;
        for item in &mut items {
            if item.launch_id == launch_id {
                item.status = "stopped".to_string();
                item.exit_code = exit_code;
                item.process_started_at = None;
                item.updated_at = now;
                item.stopped_at = Some(now);
                updated = Some(item.clone());
                break;
            }
        }
        self.save_unlocked(&items)?;
        Ok(updated)
    }

    pub fn list_with_status(&self, statuses: &[&str]) -> anyhow::Result<Vec<LaunchRecord>> {
        let items = self.list()?;
        Ok(items
            .into_iter()
            .filter(|item| statuses.iter().any(|status| item.status == *status))
            .collect())
    }

    fn load_unlocked(&self) -> anyhow::Result<Vec<LaunchRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_unlocked(&self, items: &[LaunchRecord]) -> anyhow::Result<()> {
        atomic_write_json(self.path.clone(), &serde_json::to_string_pretty(items)?)?;
        Ok(())
    }

    fn log_path_for(&self, launch_id: &str) -> PathBuf {
        self.path
            .parent()
            .map(|dir| dir.join("launch_logs").join(format!("{launch_id}.log")))
            .unwrap_or_else(|| PathBuf::from(format!("{launch_id}.log")))
    }
}

fn default_window_channel() -> String {
    "window".to_string()
}

struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(data_path: PathBuf) -> anyhow::Result<Self> {
        let lock_path = PathBuf::from(format!("{}.lock", data_path.display()));
        let stale_after = Duration::from_secs(5);
        for _ in 0..200 {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(Self { path: lock_path }),
                Err(err)
                    if err.kind() == std::io::ErrorKind::AlreadyExists
                        || err.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    // On Windows, a locked file can return ACCESS_DENIED (os error 5)
                    // instead of AlreadyExists — treat both as "lock is busy".
                    if err.kind() == std::io::ErrorKind::AlreadyExists
                        && is_stale_lock(&lock_path, stale_after)
                    {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(err.into()),
            }
        }
        anyhow::bail!("timed out waiting for lock {}", lock_path.display())
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn is_stale_lock(path: &PathBuf, stale_after: Duration) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified_at) = metadata.modified() else {
        return false;
    };
    let Ok(age) = SystemTime::now().duration_since(modified_at) else {
        return false;
    };
    age >= stale_after
}

fn atomic_write_json(path: PathBuf, content: &str) -> anyhow::Result<()> {
    let tmp_path = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&tmp_path, content)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    fs::rename(&tmp_path, &path)?;
    Ok(())
}
