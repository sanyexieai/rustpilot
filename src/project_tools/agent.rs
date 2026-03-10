use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime};

use super::{BudgetManager, classify_energy, util::now_secs_f64};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub agent_id: String,
    pub role: String,
    pub mission: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub forbidden: Vec<String>,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub agent_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub last_active_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reflection_at: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AgentManager {
    dir: PathBuf,
}

impl AgentManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn ensure_profile(
        &self,
        agent_id: &str,
        role: &str,
        mission: &str,
        scope: &[&str],
        forbidden: &[&str],
    ) -> anyhow::Result<()> {
        let _guard = FileLock::acquire(self.profiles_path())?;
        let mut profiles = self.load_profiles_unlocked()?;
        let now = now_secs_f64();
        if let Some(existing) = profiles.iter_mut().find(|item| item.agent_id == agent_id) {
            existing.role = role.trim().to_string();
            existing.mission = mission.trim().to_string();
            existing.scope = scope.iter().map(|item| item.to_string()).collect();
            existing.forbidden = forbidden.iter().map(|item| item.to_string()).collect();
            existing.updated_at = now;
        } else {
            profiles.push(AgentProfile {
                agent_id: agent_id.trim().to_string(),
                role: role.trim().to_string(),
                mission: mission.trim().to_string(),
                scope: scope.iter().map(|item| item.to_string()).collect(),
                forbidden: forbidden.iter().map(|item| item.to_string()).collect(),
                created_at: now,
                updated_at: now,
            });
        }
        profiles.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        self.save_profiles(&profiles)
    }

    pub fn set_state(
        &self,
        agent_id: &str,
        status: &str,
        current_task_id: Option<u64>,
        channel: Option<&str>,
        target: Option<&str>,
        note: Option<&str>,
    ) -> anyhow::Result<()> {
        let _guard = FileLock::acquire(self.states_path())?;
        let mut states = self.load_states_unlocked()?;
        let now = now_secs_f64();
        if let Some(existing) = states.iter_mut().find(|item| item.agent_id == agent_id) {
            existing.status = status.trim().to_string();
            existing.current_task_id = current_task_id;
            existing.channel = channel
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string);
            existing.target = target
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string);
            existing.note = note
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string);
            existing.last_active_at = now;
        } else {
            states.push(AgentState {
                agent_id: agent_id.trim().to_string(),
                status: status.trim().to_string(),
                current_task_id,
                channel: channel
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_string),
                target: target
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_string),
                note: note
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_string),
                last_active_at: now,
                last_reflection_at: None,
            });
        }
        states.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        self.save_states(&states)
    }

    pub fn list_all(&self) -> anyhow::Result<String> {
        let profiles = self.load_profiles()?;
        let states = self.load_states()?;
        if profiles.is_empty() && states.is_empty() {
            return Ok("没有 agent 记录。".to_string());
        }

        let mut lines = Vec::new();
        for profile in &profiles {
            let state = states.iter().find(|item| item.agent_id == profile.agent_id);
            let status = state.map(|item| item.status.as_str()).unwrap_or("unknown");
            let task = state
                .and_then(|item| item.current_task_id)
                .map(|item| format!(" task={item}"))
                .unwrap_or_default();
            let channel = state
                .and_then(|item| item.channel.as_deref())
                .map(|item| format!(" channel={item}"))
                .unwrap_or_default();
            let target = state
                .and_then(|item| item.target.as_deref())
                .map(|item| format!(" target={item}"))
                .unwrap_or_default();
            lines.push(format!(
                "- {} role={} status={}{}{}{} mission={}",
                profile.agent_id, profile.role, status, task, channel, target, profile.mission
            ));
        }

        for state in states.into_iter().filter(|item| {
            !profiles
                .iter()
                .any(|profile| profile.agent_id == item.agent_id)
        }) {
            let task = state
                .current_task_id
                .map(|item| format!(" task={item}"))
                .unwrap_or_default();
            let channel = state
                .channel
                .as_deref()
                .map(|item| format!(" channel={item}"))
                .unwrap_or_default();
            let target = state
                .target
                .as_deref()
                .map(|item| format!(" target={item}"))
                .unwrap_or_default();
            lines.push(format!(
                "- {} role=unknown status={}{}{}{}",
                state.agent_id, state.status, task, channel, target
            ));
        }

        Ok(lines.join("\n"))
    }

    pub fn profile(&self, agent_id: &str) -> anyhow::Result<Option<AgentProfile>> {
        Ok(self
            .load_profiles()?
            .into_iter()
            .find(|item| item.agent_id == agent_id))
    }

    pub fn state(&self, agent_id: &str) -> anyhow::Result<Option<AgentState>> {
        Ok(self
            .load_states()?
            .into_iter()
            .find(|item| item.agent_id == agent_id))
    }

    pub fn list_all_with_budgets(&self, budgets: &BudgetManager) -> anyhow::Result<String> {
        let profiles = self.load_profiles()?;
        let states = self.load_states()?;
        if profiles.is_empty() && states.is_empty() {
            return Ok("没有 agent 记录。".to_string());
        }

        let mut lines = Vec::new();
        for profile in &profiles {
            let state = states.iter().find(|item| item.agent_id == profile.agent_id);
            let status = state.map(|item| item.status.as_str()).unwrap_or("unknown");
            let task = state
                .and_then(|item| item.current_task_id)
                .map(|item| format!(" task={item}"))
                .unwrap_or_default();
            let channel = state
                .and_then(|item| item.channel.as_deref())
                .map(|item| format!(" channel={item}"))
                .unwrap_or_default();
            let target = state
                .and_then(|item| item.target.as_deref())
                .map(|item| format!(" target={item}"))
                .unwrap_or_default();
            let budget = match budgets.snapshot(&profile.agent_id)? {
                Some(item) => format!(
                    " energy={:?} budget={}/{}",
                    classify_energy(&item),
                    item.used_today,
                    item.daily_limit
                ),
                None => String::from(" energy=Unknown"),
            };
            lines.push(format!(
                "- {} role={} status={}{}{}{}{} mission={}",
                profile.agent_id,
                profile.role,
                status,
                task,
                channel,
                target,
                budget,
                profile.mission
            ));
        }

        for state in states.into_iter().filter(|item| {
            !profiles
                .iter()
                .any(|profile| profile.agent_id == item.agent_id)
        }) {
            let task = state
                .current_task_id
                .map(|item| format!(" task={item}"))
                .unwrap_or_default();
            let channel = state
                .channel
                .as_deref()
                .map(|item| format!(" channel={item}"))
                .unwrap_or_default();
            let target = state
                .target
                .as_deref()
                .map(|item| format!(" target={item}"))
                .unwrap_or_default();
            let budget = match budgets.snapshot(&state.agent_id)? {
                Some(item) => format!(
                    " energy={:?} budget={}/{}",
                    classify_energy(&item),
                    item.used_today,
                    item.daily_limit
                ),
                None => String::from(" energy=Unknown"),
            };
            lines.push(format!(
                "- {} role=unknown status={}{}{}{}{}",
                state.agent_id, state.status, task, channel, target, budget
            ));
        }

        Ok(lines.join("\n"))
    }

    fn profiles_path(&self) -> PathBuf {
        self.dir.join("agent_profiles.json")
    }

    fn states_path(&self) -> PathBuf {
        self.dir.join("agent_states.json")
    }

    fn load_profiles(&self) -> anyhow::Result<Vec<AgentProfile>> {
        let _guard = FileLock::acquire(self.profiles_path())?;
        self.load_profiles_unlocked()
    }

    fn load_profiles_unlocked(&self) -> anyhow::Result<Vec<AgentProfile>> {
        let path = self.profiles_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_profiles(&self, profiles: &[AgentProfile]) -> anyhow::Result<()> {
        atomic_write_json(
            self.profiles_path(),
            &serde_json::to_string_pretty(profiles)?,
        )?;
        Ok(())
    }

    fn load_states(&self) -> anyhow::Result<Vec<AgentState>> {
        let _guard = FileLock::acquire(self.states_path())?;
        self.load_states_unlocked()
    }

    fn load_states_unlocked(&self) -> anyhow::Result<Vec<AgentState>> {
        let path = self.states_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_states(&self, states: &[AgentState]) -> anyhow::Result<()> {
        atomic_write_json(self.states_path(), &serde_json::to_string_pretty(states)?)?;
        Ok(())
    }
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
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale_lock(&lock_path, stale_after) {
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
