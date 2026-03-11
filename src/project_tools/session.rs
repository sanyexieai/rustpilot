use crate::openai_compat::Message;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub focus: String,
    pub status: String,
    pub updated_at: u64,
}

#[derive(Debug, Clone)]
pub struct SessionManager {
    dir: PathBuf,
    index_path: PathBuf,
}

impl SessionManager {
    pub fn new(team_dir: PathBuf) -> anyhow::Result<Self> {
        let dir = team_dir.join("sessions");
        fs::create_dir_all(&dir)?;
        Ok(Self {
            index_path: dir.join("index.json"),
            dir,
        })
    }

    pub fn ensure_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        focus: &str,
        status: &str,
    ) -> anyhow::Result<SessionRecord> {
        let mut sessions = self.load_index()?;
        let now = now_secs();
        if let Some(existing) = sessions
            .iter_mut()
            .find(|item| item.session_id == session_id)
        {
            existing.label = normalize_label(label);
            existing.focus = focus.to_string();
            existing.status = status.to_string();
            existing.updated_at = now;
            let record = existing.clone();
            self.save_index(&sessions)?;
            return Ok(record);
        }

        let record = SessionRecord {
            session_id: session_id.to_string(),
            label: normalize_label(label),
            focus: focus.to_string(),
            status: status.to_string(),
            updated_at: now,
        };
        sessions.push(record.clone());
        self.save_index(&sessions)?;
        Ok(record)
    }

    pub fn create(
        &self,
        label: Option<&str>,
        focus: &str,
        status: &str,
    ) -> anyhow::Result<SessionRecord> {
        let mut sessions = self.load_index()?;
        let record = SessionRecord {
            session_id: next_session_id(&sessions),
            label: normalize_label(label),
            focus: focus.to_string(),
            status: status.to_string(),
            updated_at: now_secs(),
        };
        sessions.push(record.clone());
        self.save_index(&sessions)?;
        Ok(record)
    }

    pub fn list(&self) -> anyhow::Result<Vec<SessionRecord>> {
        let mut sessions = self.load_index()?;
        sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(sessions)
    }

    pub fn get(&self, session_id: &str) -> anyhow::Result<Option<SessionRecord>> {
        Ok(self
            .load_index()?
            .into_iter()
            .find(|item| item.session_id == session_id))
    }

    pub fn update_state(
        &self,
        session_id: &str,
        label: Option<&str>,
        focus: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let mut sessions = self.load_index()?;
        let now = now_secs();
        let Some(existing) = sessions
            .iter_mut()
            .find(|item| item.session_id == session_id)
        else {
            anyhow::bail!("unknown session: {}", session_id);
        };
        if label.is_some() {
            existing.label = normalize_label(label);
        }
        existing.focus = focus.to_string();
        existing.status = status.to_string();
        existing.updated_at = now;
        self.save_index(&sessions)
    }

    pub fn load_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        let path = self.messages_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    pub fn save_messages(&self, session_id: &str, messages: &[Message]) -> anyhow::Result<()> {
        fs::write(
            self.messages_path(session_id),
            serde_json::to_string_pretty(messages)?,
        )?;
        Ok(())
    }

    fn messages_path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", session_id))
    }

    fn load_index(&self) -> anyhow::Result<Vec<SessionRecord>> {
        if !self.index_path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.index_path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&content)?)
    }

    fn save_index(&self, sessions: &[SessionRecord]) -> anyhow::Result<()> {
        fs::write(&self.index_path, serde_json::to_string_pretty(sessions)?)?;
        Ok(())
    }
}

fn normalize_label(label: Option<&str>) -> Option<String> {
    label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn next_session_id(existing: &[SessionRecord]) -> String {
    let mut max_id = 0u64;
    for item in existing {
        if let Some(raw) = item.session_id.strip_prefix("session-")
            && let Ok(value) = raw.parse::<u64>()
        {
            max_id = max_id.max(value);
        }
    }
    format!("session-{}", max_id + 1)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::SessionManager;
    use crate::openai_compat::Message;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "{}_{}_{}",
                name,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("time")
                    .as_nanos()
            );
            let path = std::env::temp_dir()
                .join("rustpilot_sessions_tests")
                .join(unique);
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

    #[test]
    fn create_list_and_persist_messages() {
        let temp = TestDir::new("session_manager");
        let manager = SessionManager::new(temp.path().join(".team")).expect("manager");

        let primary = manager
            .ensure_session("cli-main", Some("primary"), "lead", "active")
            .expect("ensure");
        assert_eq!(primary.session_id, "cli-main");

        let created = manager
            .create(Some("scratch"), "team", "idle")
            .expect("create");
        assert_eq!(created.session_id, "session-1");

        manager
            .save_messages(
                &created.session_id,
                &[Message {
                    role: "user".to_string(),
                    content: Some("hello".to_string()),
                    tool_call_id: None,
                    tool_calls: None,
                }],
            )
            .expect("save messages");
        let loaded = manager
            .load_messages(&created.session_id)
            .expect("load messages");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content.as_deref(), Some("hello"));

        let listed = manager.list().expect("list");
        assert_eq!(listed.len(), 2);

        manager
            .update_state(&created.session_id, None, "worker(1)", "active")
            .expect("update");
        let updated = manager
            .get(&created.session_id)
            .expect("get")
            .expect("session");
        assert_eq!(updated.focus, "worker(1)");
        assert_eq!(updated.status, "active");
    }
}
