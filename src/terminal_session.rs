use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Running,
    Exited(i32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSource {
    Live,
    Restored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSessionInfo {
    pub id: String,
    pub shell: String,
    pub cwd: PathBuf,
    pub log_path: PathBuf,
    pub source: SessionSource,
    pub read_only: bool,
    pub state: SessionState,
    pub created_at: u64,
    pub output_len: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalRead {
    pub session_id: String,
    pub from: usize,
    pub next_offset: usize,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct TerminalCreateRequest {
    pub cwd: Option<PathBuf>,
    pub shell: Option<String>,
    pub env: Vec<(String, String)>,
}

impl TerminalCreateRequest {
    pub fn new() -> Self {
        Self {
            cwd: None,
            shell: None,
            env: Vec::new(),
        }
    }
}

struct SessionEntry {
    id: String,
    shell: String,
    cwd: PathBuf,
    log_path: PathBuf,
    created_at: u64,
    child: Child,
    stdin: ChildStdin,
    output: Arc<Mutex<Vec<u8>>>,
    state: SessionState,
}

pub struct TerminalManager {
    log_dir: PathBuf,
    index_path: PathBuf,
    sessions: Mutex<HashMap<String, SessionEntry>>,
    next_id: Mutex<u64>,
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalManager {
    pub fn new() -> Self {
        Self::with_log_dir(default_log_dir())
    }

    pub fn with_log_dir(log_dir: PathBuf) -> Self {
        let index_path = log_dir.join("index.json");
        let next_id = load_next_session_id(&index_path).unwrap_or(1);
        Self {
            log_dir,
            index_path,
            sessions: Mutex::new(HashMap::new()),
            next_id: Mutex::new(next_id),
        }
    }

    pub fn create(&self, request: TerminalCreateRequest) -> anyhow::Result<TerminalSessionInfo> {
        let cwd = request
            .cwd
            .unwrap_or(std::env::current_dir().context("failed to resolve current dir")?);
        let shell = request.shell.unwrap_or_else(default_shell);
        let mut command = shell_command(&shell);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&cwd);

        for (key, value) in request.env {
            command.env(key, value);
        }

        let id = self.next_session_id()?;
        fs::create_dir_all(&self.log_dir)
            .with_context(|| format!("failed to create log dir '{}'", self.log_dir.display()))?;
        let log_path = self.log_dir.join(format!("{}.log", id));
        fs::write(&log_path, [])?;

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn shell '{}'", shell))?;
        let stdin = child.stdin.take().context("shell stdin missing")?;
        let stdout = child.stdout.take().context("shell stdout missing")?;
        let stderr = child.stderr.take().context("shell stderr missing")?;
        let output = Arc::new(Mutex::new(Vec::new()));

        spawn_reader(stdout, output.clone(), log_path.clone());
        spawn_reader(stderr, output.clone(), log_path.clone());

        let created_at = now_secs();
        let info = TerminalSessionInfo {
            id: id.clone(),
            shell: shell.clone(),
            cwd: cwd.clone(),
            log_path: log_path.clone(),
            source: SessionSource::Live,
            read_only: false,
            state: SessionState::Running,
            created_at,
            output_len: 0,
        };

        let entry = SessionEntry {
            id,
            shell,
            cwd,
            log_path,
            created_at,
            child,
            stdin,
            output,
            state: SessionState::Running,
        };

        self.sessions
            .lock()
            .map_err(lock_error)?
            .insert(info.id.clone(), entry);
        self.save_session_info(&info)?;
        self.bump_next_id_from(&info.id)?;

        Ok(info)
    }

    pub fn write(&self, session_id: &str, input: &str) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().map_err(lock_error)?;
        let entry = sessions.get_mut(session_id);
        if let Some(entry) = entry {
            refresh_state(entry)?;
            if !matches!(entry.state, SessionState::Running) {
                anyhow::bail!("session has exited: {}", session_id);
            }
            entry.stdin.write_all(input.as_bytes())?;
            entry.stdin.flush()?;
            return Ok(());
        }
        drop(sessions);

        if self.load_session_info(session_id)?.is_some() {
            anyhow::bail!("session is restored and read-only: {}", session_id);
        }

        anyhow::bail!("unknown session: {}", session_id);
    }

    pub fn read(&self, session_id: &str, from: usize) -> anyhow::Result<TerminalRead> {
        let sessions = self.sessions.lock().map_err(lock_error)?;
        if let Some(entry) = sessions.get(session_id) {
            let output = entry.output.lock().map_err(lock_error)?;
            let clamped = from.min(output.len());
            let data = String::from_utf8_lossy(&output[clamped..]).to_string();
            return Ok(TerminalRead {
                session_id: session_id.to_string(),
                from: clamped,
                next_offset: output.len(),
                data,
            });
        }
        drop(sessions);

        let info = self
            .load_session_info(session_id)?
            .ok_or_else(|| anyhow::anyhow!("unknown session: {}", session_id))?;
        let bytes = fs::read(&info.log_path).unwrap_or_default();
        let clamped = from.min(bytes.len());
        let data = String::from_utf8_lossy(&bytes[clamped..]).to_string();
        Ok(TerminalRead {
            session_id: session_id.to_string(),
            from: clamped,
            next_offset: bytes.len(),
            data,
        })
    }

    pub fn list(&self) -> anyhow::Result<Vec<TerminalSessionInfo>> {
        let mut sessions = self.sessions.lock().map_err(lock_error)?;
        let mut items = self.load_index()?;
        for entry in sessions.values_mut() {
            refresh_state(entry)?;
            let info = build_info(entry)?;
            self.save_session_info(&info)?;
            upsert_session(&mut items, info);
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    }

    pub fn status(&self, session_id: &str) -> anyhow::Result<TerminalSessionInfo> {
        let mut sessions = self.sessions.lock().map_err(lock_error)?;
        if let Some(entry) = sessions.get_mut(session_id) {
            refresh_state(entry)?;
            let info = build_info(entry)?;
            self.save_session_info(&info)?;
            return Ok(info);
        }
        drop(sessions);
        self.load_session_info(session_id)?
            .map(mark_restored)
            .ok_or_else(|| anyhow::anyhow!("unknown session: {}", session_id))
    }

    pub fn kill(&self, session_id: &str) -> anyhow::Result<TerminalSessionInfo> {
        let mut sessions = self.sessions.lock().map_err(lock_error)?;
        let entry = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("unknown session: {}", session_id))?;

        if matches!(entry.state, SessionState::Running) {
            let _ = entry.child.kill();
            let _ = entry.child.wait();
            refresh_state(entry)?;
        }

        let info = build_info(entry)?;
        self.save_session_info(&info)?;
        Ok(info)
    }

    fn next_session_id(&self) -> anyhow::Result<String> {
        let mut next = self.next_id.lock().map_err(lock_error)?;
        let id = format!("term-{}", *next);
        *next += 1;
        Ok(id)
    }

    pub fn reset(&self) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().map_err(lock_error)?;
        for entry in sessions.values_mut() {
            if matches!(entry.state, SessionState::Running) {
                let _ = entry.child.kill();
                let _ = entry.child.wait();
            }
        }
        sessions.clear();
        *self.next_id.lock().map_err(lock_error)? = 1;
        if self.log_dir.exists() {
            fs::remove_dir_all(&self.log_dir).with_context(|| {
                format!("failed to remove log dir '{}'", self.log_dir.display())
            })?;
        }
        Ok(())
    }

    pub fn clear_live_sessions(&self) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().map_err(lock_error)?;
        for entry in sessions.values_mut() {
            if matches!(entry.state, SessionState::Running) {
                let _ = entry.child.kill();
                let _ = entry.child.wait();
            }
        }
        sessions.clear();
        *self.next_id.lock().map_err(lock_error)? =
            load_next_session_id(&self.index_path).unwrap_or(1);
        Ok(())
    }

    fn bump_next_id_from(&self, session_id: &str) -> anyhow::Result<()> {
        let value = session_id
            .strip_prefix("term-")
            .and_then(|raw| raw.parse::<u64>().ok())
            .map(|n| n + 1)
            .unwrap_or(1);
        let mut next = self.next_id.lock().map_err(lock_error)?;
        if *next < value {
            *next = value;
        }
        Ok(())
    }

    fn load_index(&self) -> anyhow::Result<Vec<TerminalSessionInfo>> {
        if !self.index_path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&self.index_path)?;
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let items: Vec<TerminalSessionInfo> = serde_json::from_str(&text)?;
        Ok(items.into_iter().map(mark_restored).collect())
    }

    fn save_index(&self, items: &[TerminalSessionInfo]) -> anyhow::Result<()> {
        fs::create_dir_all(&self.log_dir)?;
        fs::write(&self.index_path, serde_json::to_string_pretty(items)?)?;
        Ok(())
    }

    fn save_session_info(&self, info: &TerminalSessionInfo) -> anyhow::Result<()> {
        let mut items = self.load_index()?;
        upsert_session(&mut items, info.clone());
        self.save_index(&items)
    }

    fn load_session_info(&self, session_id: &str) -> anyhow::Result<Option<TerminalSessionInfo>> {
        Ok(self
            .load_index()?
            .into_iter()
            .find(|item| item.id == session_id))
    }
}

fn build_info(entry: &SessionEntry) -> anyhow::Result<TerminalSessionInfo> {
    let output_len = entry.output.lock().map_err(lock_error)?.len();
    Ok(TerminalSessionInfo {
        id: entry.id.clone(),
        shell: entry.shell.clone(),
        cwd: entry.cwd.clone(),
        log_path: entry.log_path.clone(),
        source: SessionSource::Live,
        read_only: !matches!(entry.state, SessionState::Running),
        state: entry.state.clone(),
        created_at: entry.created_at,
        output_len,
    })
}

fn mark_restored(mut info: TerminalSessionInfo) -> TerminalSessionInfo {
    info.source = SessionSource::Restored;
    info.read_only = true;
    info
}

fn upsert_session(items: &mut Vec<TerminalSessionInfo>, info: TerminalSessionInfo) {
    if let Some(existing) = items.iter_mut().find(|item| item.id == info.id) {
        *existing = info;
    } else {
        items.push(info);
    }
}

fn refresh_state(entry: &mut SessionEntry) -> anyhow::Result<()> {
    if matches!(entry.state, SessionState::Running) {
        if let Some(status) = entry.child.try_wait()? {
            entry.state = SessionState::Exited(status.code().unwrap_or(-1));
        }
    }
    Ok(())
}

fn spawn_reader<R>(mut reader: R, output: Arc<Mutex<Vec<u8>>>, log_path: PathBuf)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut shared) = output.lock() {
                        shared.extend_from_slice(&buf[..n]);
                    } else {
                        break;
                    }
                    let _ = append_log_chunk(&log_path, &buf[..n]);
                }
                Err(_) => break,
            }
        }
    });
}

fn append_log_chunk(path: &PathBuf, chunk: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().append(true).create(true).open(path)?;
    file.write_all(chunk)?;
    file.flush()?;
    Ok(())
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> anyhow::Error {
    anyhow::anyhow!("terminal manager lock poisoned")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn default_log_dir() -> PathBuf {
    std::env::current_dir()
        .map(|dir| dir.join(".terminal_sessions"))
        .unwrap_or_else(|_| std::env::temp_dir().join("rustpilot_terminal_sessions"))
}

fn load_next_session_id(index_path: &PathBuf) -> Option<u64> {
    let text = fs::read_to_string(index_path).ok()?;
    let items: Vec<TerminalSessionInfo> = serde_json::from_str(&text).ok()?;
    let max = items
        .into_iter()
        .filter_map(|item| item.id.strip_prefix("term-")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    Some(max + 1)
}

#[cfg(target_os = "windows")]
fn default_shell() -> String {
    "powershell".to_string()
}

#[cfg(not(target_os = "windows"))]
fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string())
}

#[cfg(target_os = "windows")]
fn shell_command(shell: &str) -> Command {
    let mut command = Command::new(shell);
    command.args(["-NoLogo", "-NoProfile"]);
    command
}

#[cfg(not(target_os = "windows"))]
fn shell_command(shell: &str) -> Command {
    let mut command = Command::new(shell);
    command.arg("-i");
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir()
            .join("rustpilot_terminal_tests")
            .join(format!("{}_{}", name, unique));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn wait_for_output(manager: &TerminalManager, session_id: &str, needle: &str) -> TerminalRead {
        for _ in 0..40 {
            let read = manager.read(session_id, 0).expect("read output");
            if read.data.contains(needle) {
                return read;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for output: {}", needle);
    }

    #[test]
    fn create_and_list_session() {
        let manager = TerminalManager::with_log_dir(temp_dir("create_list_logs"));
        let info = manager
            .create(TerminalCreateRequest::new())
            .expect("create");
        assert_eq!(info.id, "term-1");
        assert_eq!(info.state, SessionState::Running);
        assert_eq!(info.source, SessionSource::Live);
        assert!(!info.read_only);
        assert!(info.log_path.ends_with("term-1.log"));

        let sessions = manager.list().expect("list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, info.id);
        let reloaded = TerminalManager::with_log_dir(manager.log_dir.clone());
        let sessions = reloaded.list().expect("reload list");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, info.id);
        assert_eq!(sessions[0].source, SessionSource::Restored);
        assert!(sessions[0].read_only);

        let _ = manager.kill(&info.id);
        manager.reset().expect("reset");
    }

    #[test]
    fn write_and_read_session_output() {
        let log_dir = temp_dir("write_read_logs");
        let manager = TerminalManager::with_log_dir(log_dir.clone());
        let cwd = temp_dir("write_read");
        let info = manager
            .create(TerminalCreateRequest {
                cwd: Some(cwd),
                shell: None,
                env: Vec::new(),
            })
            .expect("create");

        #[cfg(target_os = "windows")]
        manager
            .write(&info.id, "Write-Output 'rustpilot-ready'\n")
            .expect("write");

        #[cfg(not(target_os = "windows"))]
        manager
            .write(&info.id, "printf 'rustpilot-ready\\n'\n")
            .expect("write");

        let read = wait_for_output(&manager, &info.id, "rustpilot-ready");
        assert!(read.data.contains("rustpilot-ready"));
        let persisted = std::fs::read_to_string(log_dir.join("term-1.log")).expect("read log");
        assert!(persisted.contains("rustpilot-ready"));

        let _ = manager.kill(&info.id);
        manager.reset().expect("reset");
    }

    #[test]
    fn kill_updates_session_state() {
        let manager = TerminalManager::with_log_dir(temp_dir("kill_logs"));
        let info = manager
            .create(TerminalCreateRequest::new())
            .expect("create");
        let killed = manager.kill(&info.id).expect("kill");
        assert!(matches!(killed.state, SessionState::Exited(_)));
        manager.reset().expect("reset");
    }

    #[test]
    fn persisted_metadata_supports_reload_and_read() {
        let log_dir = temp_dir("metadata_reload_logs");
        let manager = TerminalManager::with_log_dir(log_dir.clone());
        let info = manager
            .create(TerminalCreateRequest::new())
            .expect("create");

        #[cfg(target_os = "windows")]
        manager
            .write(&info.id, "Write-Output 'persisted-ok'\n")
            .expect("write");
        #[cfg(not(target_os = "windows"))]
        manager
            .write(&info.id, "printf 'persisted-ok\\n'\n")
            .expect("write");

        let _ = wait_for_output(&manager, &info.id, "persisted-ok");
        let _ = manager.kill(&info.id).expect("kill");

        let reloaded = TerminalManager::with_log_dir(log_dir);
        let status = reloaded.status(&info.id).expect("status");
        assert!(matches!(status.state, SessionState::Exited(_)));
        assert_eq!(status.source, SessionSource::Restored);
        assert!(status.read_only);
        let read = reloaded.read(&info.id, 0).expect("read");
        assert!(read.data.contains("persisted-ok"));
        let next = reloaded
            .create(TerminalCreateRequest::new())
            .expect("create next");
        assert_eq!(next.id, "term-2");
        let _ = reloaded.kill(&next.id);

        reloaded.reset().expect("reset");
    }

    #[test]
    fn write_rejects_restored_and_unknown_sessions() {
        let log_dir = temp_dir("write_error_logs");
        let manager = TerminalManager::with_log_dir(log_dir.clone());
        let info = manager
            .create(TerminalCreateRequest::new())
            .expect("create");
        let _ = manager.kill(&info.id).expect("kill");

        let exited = manager
            .write(&info.id, "echo should-fail\n")
            .unwrap_err()
            .to_string();
        assert!(exited.contains("session has exited"));

        let reloaded = TerminalManager::with_log_dir(log_dir);
        let restored = reloaded
            .write(&info.id, "echo should-fail\n")
            .unwrap_err()
            .to_string();
        assert!(restored.contains("restored and read-only"));

        let unknown = reloaded
            .write("term-999", "echo should-fail\n")
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("unknown session"));

        reloaded.reset().expect("reset");
    }

    #[test]
    fn cleared_live_sessions_make_entries_restored() {
        let log_dir = temp_dir("clear_live_logs");
        let manager = TerminalManager::with_log_dir(log_dir.clone());
        let info = manager
            .create(TerminalCreateRequest::new())
            .expect("create");
        let _ = manager.kill(&info.id).expect("kill");

        manager.clear_live_sessions().expect("clear live sessions");
        let listed = manager.list().expect("list");
        let session = listed
            .into_iter()
            .find(|item| item.id == info.id)
            .expect("session");
        assert_eq!(session.source, SessionSource::Restored);
        assert!(session.read_only);

        let reloaded = TerminalManager::with_log_dir(log_dir);
        reloaded.reset().expect("reset");
    }
}
