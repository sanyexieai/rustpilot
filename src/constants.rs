pub const LLM_TIMEOUT_SECS: u64 = 120;
pub const MAX_AGENT_TURNS: usize = 24;
/// 单轮 LLM 调用的墙钟超时（秒）。超时后 worker 会触发自愈流程。
pub const WORKER_TURN_TIMEOUT_SECS: u64 = 60;
/// worker 整体运行超过此时间（秒）仍未完成，父进程调度器强制杀掉并重新排队。
pub const WORKER_STUCK_NOTIFY_SECS: u64 = 120;

// LLM 请求重试配置
pub const RETRY_MAX_ATTEMPTS: usize = 4; // 最多重试 4 次
pub const RETRY_INITIAL_DELAY_MS: u64 = 1000; // 初始延迟 1 秒
pub const RETRY_MAX_DELAY_MS: u64 = 10000; // 最大延迟 10 秒
