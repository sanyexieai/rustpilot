pub const TASK_PRIORITIES: &[&str] = &["critical", "high", "medium", "low"];
pub const TASK_ROLE_HINTS: &[&str] = &["developer", "design", "critic", "ui"];
pub const DEFAULT_TASK_PRIORITY: &str = "medium";
pub const DEFAULT_TASK_ROLE_HINT: &str = "developer";
pub const TASK_STATUSES: &[&str] = &[
    "pending",
    "in_progress",
    "blocked",
    "paused",
    "cancelled",
    "completed",
    "failed",
];

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_IN_PROGRESS: &str = "in_progress";
pub const STATUS_BLOCKED: &str = "blocked";
pub const STATUS_PAUSED: &str = "paused";
pub const STATUS_CANCELLED: &str = "cancelled";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const SESSION_STATUS_ACTIVE: &str = "active";
pub const SESSION_STATUS_IDLE: &str = "idle";
pub const FOCUS_ROOT: &str = "root";
pub const FOCUS_SHELL: &str = "shell";
pub const FOCUS_TEAM: &str = "team";
pub const ROOT_ACTOR_DEFAULT: &str = "lead";

pub fn default_task_priority() -> String {
    DEFAULT_TASK_PRIORITY.to_string()
}

pub fn default_task_role_hint() -> String {
    DEFAULT_TASK_ROLE_HINT.to_string()
}

pub fn is_valid_task_status(status: &str) -> bool {
    TASK_STATUSES.contains(&status)
}

pub fn normalize_task_priority(priority: &str) -> String {
    match priority.trim().to_lowercase().as_str() {
        "critical" => "critical".to_string(),
        "high" => "high".to_string(),
        "low" => "low".to_string(),
        _ => default_task_priority(),
    }
}

pub fn normalize_task_role_hint(role_hint: &str) -> String {
    match role_hint.trim().to_lowercase().as_str() {
        "critic" => "critic".to_string(),
        "ui" => "ui".to_string(),
        "design" => "design".to_string(),
        _ => default_task_role_hint(),
    }
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
