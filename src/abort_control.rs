use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;

#[derive(Debug)]
struct SessionAbortState {
    current_generation: AtomicU64,
    active_generation: AtomicU64,
    cancelled_generation: AtomicU64,
    notify: Notify,
}

impl SessionAbortState {
    fn new() -> Self {
        Self {
            current_generation: AtomicU64::new(0),
            active_generation: AtomicU64::new(0),
            cancelled_generation: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionAbortLease {
    state: Arc<SessionAbortState>,
    generation: u64,
}

impl SessionAbortLease {
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled_generation.load(Ordering::SeqCst) >= self.generation
    }

    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            self.state.notify.notified().await;
        }
    }
}

impl Drop for SessionAbortLease {
    fn drop(&mut self) {
        let _ = self.state.active_generation.compare_exchange(
            self.generation,
            0,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

fn registry() -> &'static Mutex<HashMap<String, Arc<SessionAbortState>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<SessionAbortState>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state_for(session_id: &str) -> Arc<SessionAbortState> {
    let mut guard = registry().lock().unwrap_or_else(|err| err.into_inner());
    guard
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(SessionAbortState::new()))
        .clone()
}

pub fn begin_session_request(session_id: &str) -> SessionAbortLease {
    let state = state_for(session_id);
    let generation = state.current_generation.fetch_add(1, Ordering::SeqCst) + 1;
    state.active_generation.store(generation, Ordering::SeqCst);
    SessionAbortLease { state, generation }
}

pub fn abort_session(session_id: &str) -> bool {
    let state = state_for(session_id);
    let generation = state.active_generation.load(Ordering::SeqCst);
    if generation == 0 {
        return false;
    }
    state
        .cancelled_generation
        .store(generation, Ordering::SeqCst);
    state.notify.notify_waiters();
    true
}

pub fn has_active_request(session_id: &str) -> bool {
    state_for(session_id)
        .active_generation
        .load(Ordering::SeqCst)
        != 0
}

#[cfg(test)]
mod tests {
    use super::{abort_session, begin_session_request, has_active_request};

    #[tokio::test]
    async fn abort_notifies_current_generation() {
        let lease = begin_session_request("session-a");
        assert!(has_active_request("session-a"));
        assert!(!lease.is_cancelled());
        assert!(abort_session("session-a"));
        lease.cancelled().await;
        assert!(lease.is_cancelled());
    }

    #[test]
    fn abort_without_active_request_returns_false() {
        assert!(!abort_session("session-never-started"));
    }

    #[test]
    fn completed_request_is_no_longer_abortable() {
        let lease = begin_session_request("session-finished");
        assert!(has_active_request("session-finished"));
        drop(lease);
        assert!(!has_active_request("session-finished"));
        assert!(!abort_session("session-finished"));
    }
}
