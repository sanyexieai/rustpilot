#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiIntentMatch {
    pub desired_view: String,
    pub operator_note: String,
}

#[derive(Debug, Clone)]
pub struct UiIntentPolicy {
    direct_open_patterns: Vec<String>,
    open_tokens: Vec<String>,
    task_tokens: Vec<String>,
    session_tokens: Vec<String>,
    approval_tokens: Vec<String>,
    resident_tokens: Vec<String>,
    ui_surface_tokens: Vec<String>,
    management_tokens: Vec<String>,
}

impl Default for UiIntentPolicy {
    fn default() -> Self {
        Self {
            direct_open_patterns: env_csv(
                "RUSTPILOT_UI_DIRECT_OPEN_PATTERNS",
                &[
                    "open ui",
                    "open the ui",
                    "open dashboard",
                    "open the dashboard",
                    "open status page",
                    "open the status page",
                    "open control panel",
                    "open management page",
                    "show dashboard",
                    "show status page",
                    "show control panel",
                ],
            ),
            open_tokens: env_csv(
                "RUSTPILOT_UI_OPEN_TOKENS",
                &[
                    "open",
                    "show",
                    "launch",
                    "start",
                    "打开",
                    "开启",
                    "开个",
                    "给我开",
                ],
            ),
            task_tokens: env_csv(
                "RUSTPILOT_UI_TASK_TOKENS",
                &["task", "tasks", "任务", "工单"],
            ),
            session_tokens: env_csv(
                "RUSTPILOT_UI_SESSION_TOKENS",
                &["session", "sessions", "会话"],
            ),
            approval_tokens: env_csv(
                "RUSTPILOT_UI_APPROVAL_TOKENS",
                &["approval", "approvals", "审批"],
            ),
            resident_tokens: env_csv(
                "RUSTPILOT_UI_RESIDENT_TOKENS",
                &["resident", "residents", "agent", "agents", "驻留", "代理"],
            ),
            ui_surface_tokens: env_csv(
                "RUSTPILOT_UI_SURFACE_TOKENS",
                &[
                    "ui",
                    "dashboard",
                    "status page",
                    "control panel",
                    "management page",
                    "page",
                    "panel",
                    "页面",
                    "界面",
                    "面板",
                    "状态页",
                    "管理页",
                ],
            ),
            management_tokens: env_csv(
                "RUSTPILOT_UI_MANAGEMENT_TOKENS",
                &[
                    "status",
                    "current state",
                    "system state",
                    "project state",
                    "manage",
                    "management",
                    "current",
                    "状态",
                    "当前",
                    "管理",
                    "系统",
                    "项目",
                ],
            ),
        }
    }
}

impl UiIntentPolicy {
    pub fn load() -> Self {
        Self::default()
    }

    pub fn classify(&self, lowered: &str) -> Option<UiIntentMatch> {
        if contains_any(lowered, &self.direct_open_patterns) {
            return Some(UiIntentMatch {
                desired_view: "project_state".to_string(),
                operator_note: "intent detected from direct ui open phrasing".to_string(),
            });
        }

        let has_open = contains_any(lowered, &self.open_tokens);
        let has_management_intent = contains_any(lowered, &self.management_tokens);
        let has_ui_surface = contains_any(lowered, &self.ui_surface_tokens);
        if !((has_open || has_management_intent) && has_ui_surface) {
            return None;
        }

        let desired_view = if contains_any(lowered, &self.task_tokens) {
            "task_board"
        } else if contains_any(lowered, &self.session_tokens) {
            "session_console"
        } else if contains_any(lowered, &self.approval_tokens) {
            "approval_overview"
        } else if contains_any(lowered, &self.resident_tokens) {
            "resident_monitor"
        } else {
            "project_state"
        };

        Some(UiIntentMatch {
            desired_view: desired_view.to_string(),
            operator_note: format!("derived desired_view={}", desired_view),
        })
    }
}

fn contains_any(haystack: &str, tokens: &[String]) -> bool {
    tokens.iter().any(|token| haystack.contains(token))
}

fn env_csv(key: &str, defaults: &[&str]) -> Vec<String> {
    let parsed = std::env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_lowercase())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !parsed.is_empty() {
        parsed
    } else {
        defaults.iter().map(|value| value.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::UiIntentPolicy;

    #[test]
    fn classifies_direct_dashboard_open() {
        let policy = UiIntentPolicy::load();
        let matched = policy.classify("open dashboard").expect("intent");
        assert_eq!(matched.desired_view, "project_state");
    }

    #[test]
    fn classifies_task_dashboard() {
        let policy = UiIntentPolicy::load();
        let matched = policy
            .classify("open a task dashboard for the current project")
            .expect("intent");
        assert_eq!(matched.desired_view, "task_board");
    }
}
