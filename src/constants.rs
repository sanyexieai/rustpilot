pub const LLM_TIMEOUT_SECS: u64 = 120;
pub const MAX_AGENT_TURNS: usize = 24;

// LLM 请求重试配置
pub const RETRY_MAX_ATTEMPTS: usize = 4;          // 最多重试 4 次
pub const RETRY_INITIAL_DELAY_MS: u64 = 1000;      // 初始延迟 1 秒
pub const RETRY_MAX_DELAY_MS: u64 = 10000;        // 最大延迟 10 秒
