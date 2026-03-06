use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct ActivityState {
    round: usize,
    stage: String,
    active_tool: Option<String>,
    last_update: f64,
}

impl ActivityState {
    pub fn idle() -> Self {
        Self {
            round: 0,
            stage: "空闲".to_string(),
            active_tool: None,
            last_update: now_secs_f64(),
        }
    }
}

pub type ActivityHandle = Arc<Mutex<ActivityState>>;

pub fn new_activity_handle() -> ActivityHandle {
    Arc::new(Mutex::new(ActivityState::idle()))
}

pub fn set_activity(
    progress: &ActivityHandle,
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

pub fn render_activity(progress: &ActivityHandle) -> String {
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

pub struct WaitHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl WaitHeartbeat {
    pub fn start(progress: ActivityHandle, label: String) -> Self {
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

fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
