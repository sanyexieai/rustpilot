use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::project_tools::{LaunchPresentationMode, LaunchRecord};

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub cwd: PathBuf,
    pub window_title: String,
    pub command_line: Vec<String>,
    pub env: Vec<(String, String)>,
    pub launch_id: String,
    pub agent_id: String,
    pub kind: String,
    pub log_path: String,
}

#[derive(Debug, Clone)]
pub struct LaunchExecution {
    pub pid: u32,
    pub channel: String,
    pub target: String,
    pub window_title: String,
}

#[derive(Debug, Clone)]
pub struct LaunchBackendStatus {
    pub requested_mode: LaunchPresentationMode,
    pub effective_mode: LaunchPresentationMode,
    pub backend: String,
    pub note: String,
}

pub fn launch(plan: &LaunchPlan, mode: LaunchPresentationMode) -> anyhow::Result<LaunchExecution> {
    let status = backend_status(mode);
    #[cfg(windows)]
    {
        spawn_windows_agent_window(plan, status.effective_mode)
    }
    #[cfg(not(windows))]
    {
        let mut command = launch_host_command(plan, &status);
        let child = spawn_new_console(&mut command)?;
        Ok(match status.effective_mode {
            LaunchPresentationMode::MultiWindow => LaunchExecution {
                pid: child.id(),
                channel: "window".to_string(),
                target: plan.window_title.clone(),
                window_title: plan.window_title.clone(),
            },
            LaunchPresentationMode::SingleWindow => LaunchExecution {
                pid: child.id(),
                channel: "single_window".to_string(),
                target: "root-console".to_string(),
                window_title: String::new(),
            },
            LaunchPresentationMode::ImplicitMultiWindow => LaunchExecution {
                pid: child.id(),
                channel: "implicit_window".to_string(),
                target: plan.window_title.clone(),
                window_title: String::new(),
            },
        })
    }
}

pub fn backend_status(mode: LaunchPresentationMode) -> LaunchBackendStatus {
    #[cfg(windows)]
    {
        match mode {
            LaunchPresentationMode::MultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "windows_start_process".to_string(),
                note: "visible windows are launched through Start-Process cmd.exe hosts"
                    .to_string(),
            },
            LaunchPresentationMode::SingleWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "direct_spawn".to_string(),
                note: "child launches run without opening extra windows".to_string(),
            },
            LaunchPresentationMode::ImplicitMultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "direct_spawn".to_string(),
                note: "launches stay isolated as background processes with per-launch logs"
                    .to_string(),
            },
        }
    }
    #[cfg(target_os = "linux")]
    {
        match mode {
            LaunchPresentationMode::MultiWindow => linux_terminal_backend()
                .map(|backend| LaunchBackendStatus {
                    requested_mode: mode,
                    effective_mode: LaunchPresentationMode::MultiWindow,
                    backend: backend.to_string(),
                    note: "visible windows are launched through a detected Linux terminal host"
                        .to_string(),
                })
                .unwrap_or_else(|| LaunchBackendStatus {
                    requested_mode: mode,
                    effective_mode: LaunchPresentationMode::ImplicitMultiWindow,
                    backend: "detached_process".to_string(),
                    note: "no supported Linux terminal host was detected; falling back to implicit multi-window isolation".to_string(),
                }),
            LaunchPresentationMode::SingleWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "direct_spawn".to_string(),
                note: "child launches run without opening extra windows".to_string(),
            },
            LaunchPresentationMode::ImplicitMultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "detached_process".to_string(),
                note: "launches stay isolated as background processes with per-launch logs"
                    .to_string(),
            },
        }
    }
    #[cfg(target_os = "macos")]
    {
        match mode {
            LaunchPresentationMode::MultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: LaunchPresentationMode::ImplicitMultiWindow,
                backend: "detached_process".to_string(),
                note: "macOS visible terminal-window backend is not implemented yet; falling back to implicit multi-window isolation".to_string(),
            },
            LaunchPresentationMode::SingleWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "direct_spawn".to_string(),
                note: "child launches run without opening extra windows".to_string(),
            },
            LaunchPresentationMode::ImplicitMultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "detached_process".to_string(),
                note: "launches stay isolated as background processes with per-launch logs"
                    .to_string(),
            },
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        match mode {
            LaunchPresentationMode::MultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: LaunchPresentationMode::ImplicitMultiWindow,
                backend: "detached_process".to_string(),
                note: "this platform does not yet expose a visible terminal host; falling back to implicit multi-window isolation".to_string(),
            },
            LaunchPresentationMode::SingleWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "direct_spawn".to_string(),
                note: "child launches run without opening extra windows".to_string(),
            },
            LaunchPresentationMode::ImplicitMultiWindow => LaunchBackendStatus {
                requested_mode: mode,
                effective_mode: mode,
                backend: "detached_process".to_string(),
                note: "launches stay isolated as background processes with per-launch logs"
                    .to_string(),
            },
        }
    }
}

pub fn plan_for_record(
    cwd: PathBuf,
    window_title: String,
    command_line: Vec<String>,
    record: &LaunchRecord,
) -> LaunchPlan {
    let mut env = Vec::new();
    if !record.log_path.trim().is_empty() {
        env.push(("RUSTPILOT_LAUNCH_LOG".to_string(), record.log_path.clone()));
    }
    env.push(("RUSTPILOT_LAUNCH_ID".to_string(), record.launch_id.clone()));
    LaunchPlan {
        cwd,
        window_title,
        command_line,
        env,
        launch_id: record.launch_id.clone(),
        agent_id: record.agent_id.clone(),
        kind: record.kind.clone(),
        log_path: record.log_path.clone(),
    }
}

#[cfg(not(windows))]
fn launch_host_command(plan: &LaunchPlan, status: &LaunchBackendStatus) -> Command {
    #[cfg(target_os = "linux")]
    if status.effective_mode == LaunchPresentationMode::MultiWindow {
        return linux_terminal_command(plan, &status.backend);
    }
    let mut iter = plan.command_line.iter();
    let program = iter.next().cloned().unwrap_or_default();
    let mut command = Command::new(program);
    command.args(iter);
    command.current_dir(&plan.cwd);
    for (key, value) in &plan.env {
        command.env(key, value);
    }
    if status.effective_mode != LaunchPresentationMode::SingleWindow {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    command
}

#[cfg(not(windows))]
fn spawn_new_console(command: &mut Command) -> anyhow::Result<std::process::Child> {
    Ok(command.spawn()?)
}

#[cfg(windows)]
fn spawn_windows_agent_window(
    plan: &LaunchPlan,
    mode: LaunchPresentationMode,
) -> anyhow::Result<LaunchExecution> {
    if mode != LaunchPresentationMode::MultiWindow {
        let mut command = Command::new(plan.command_line.first().cloned().unwrap_or_default());
        if plan.command_line.len() > 1 {
            command.args(&plan.command_line[1..]);
        }
        command.current_dir(&plan.cwd);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        for (key, value) in &plan.env {
            command.env(key, value);
        }
        let child = command.spawn()?;
        return Ok(match mode {
            LaunchPresentationMode::SingleWindow => LaunchExecution {
                pid: child.id(),
                channel: "single_window".to_string(),
                target: "root-console".to_string(),
                window_title: String::new(),
            },
            LaunchPresentationMode::ImplicitMultiWindow => LaunchExecution {
                pid: child.id(),
                channel: "implicit_window".to_string(),
                target: plan.window_title.clone(),
                window_title: String::new(),
            },
            LaunchPresentationMode::MultiWindow => unreachable!(),
        });
    }
    let script = build_windows_launch_script(plan);
    let output = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-Command",
            &format!(
                "$proc = Start-Process -FilePath 'cmd.exe' -ArgumentList @('/d','/s','/c','{}') -WorkingDirectory '{}' -WindowStyle Normal -PassThru; $proc.Id",
                powershell_quote(&script),
                powershell_quote(&plan.cwd.display().to_string())
            ),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "failed to launch window host: {}",
            if stderr.is_empty() {
                "unknown windows start-process error".to_string()
            } else {
                stderr
            }
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid = stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().parse::<u32>().ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "failed to parse launched window pid from '{}'",
                stdout.trim()
            )
        })?;
    Ok(LaunchExecution {
        pid,
        channel: "window".to_string(),
        target: plan.window_title.clone(),
        window_title: plan.window_title.clone(),
    })
}

#[cfg(windows)]
pub(crate) fn build_windows_launch_script(plan: &LaunchPlan) -> String {
    let title = sanitize_cmd_text(&plan.window_title);
    let launch_id = sanitize_cmd_text(&plan.launch_id);
    let agent_id = sanitize_cmd_text(&plan.agent_id);
    let kind = sanitize_cmd_text(&plan.kind);
    let log_path = sanitize_cmd_text(&plan.log_path);
    let env_commands = plan
        .env
        .iter()
        .map(|(key, value)| {
            format!(
                "set \"{}={}\"",
                sanitize_cmd_text(key),
                sanitize_cmd_text(value)
            )
        })
        .collect::<Vec<_>>()
        .join(" & ");
    let command = plan
        .command_line
        .iter()
        .map(|item| cmd_quote(item))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "title {title} & {env_commands} & echo [launch] id={launch_id} agent={agent_id} kind={kind} & echo [launch] log={log_path} & echo [launch] command={command} & {command}"
    )
}

#[cfg(windows)]
fn sanitize_cmd_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' | '&' | '|' | '<' | '>' | '^' => ' ',
            _ => ch,
        })
        .collect()
}

#[cfg(windows)]
fn cmd_quote(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '&' | '|' | '<' | '>' | '^' | '(' | ')'))
    {
        return format!("\"{}\"", value.replace('"', "\\\""));
    }
    value.to_string()
}

#[cfg(windows)]
fn powershell_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(target_os = "linux")]
fn linux_terminal_backend() -> Option<&'static str> {
    [
        "x-terminal-emulator",
        "gnome-terminal",
        "konsole",
        "xfce4-terminal",
        "xterm",
        "kitty",
        "alacritty",
    ]
    .into_iter()
    .find(|candidate| command_exists(candidate))
}

#[cfg(target_os = "linux")]
fn linux_terminal_command(plan: &LaunchPlan, backend: &str) -> Command {
    let script = posix_launch_script(plan);
    match backend {
        "gnome-terminal" => {
            let mut command = Command::new("gnome-terminal");
            command.args(["--", "sh", "-lc", &script]);
            command
        }
        "konsole" => {
            let mut command = Command::new("konsole");
            command.args(["-e", "sh", "-lc", &script]);
            command
        }
        "xfce4-terminal" => {
            let mut command = Command::new("xfce4-terminal");
            command.args([
                "--title",
                &plan.window_title,
                "--command",
                &format!("sh -lc {}", shell_single_quote(&script)),
            ]);
            command
        }
        "kitty" => {
            let mut command = Command::new("kitty");
            command.args(["sh", "-lc", &script]);
            command
        }
        "alacritty" => {
            let mut command = Command::new("alacritty");
            command.args(["-t", &plan.window_title, "-e", "sh", "-lc", &script]);
            command
        }
        "xterm" => {
            let mut command = Command::new("xterm");
            command.args(["-T", &plan.window_title, "-e", "sh", "-lc", &script]);
            command
        }
        _ => {
            let mut command = Command::new(backend);
            command.args(["-e", "sh", "-lc", &script]);
            command
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", unix))]
fn posix_launch_script(plan: &LaunchPlan) -> String {
    let exports = plan
        .env
        .iter()
        .map(|(key, value)| {
            format!(
                "export {}={}",
                shell_env_key(key),
                shell_single_quote(value)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let command = plan
        .command_line
        .iter()
        .map(|item| shell_single_quote(item))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "cd {}; {}; printf '%s\\n' '[launch] id={} agent={} kind={}' '[launch] log={}' '[launch] command={}'; exec {}",
        shell_single_quote(&plan.cwd.display().to_string()),
        exports,
        shell_single_quote(&plan.launch_id),
        shell_single_quote(&plan.agent_id),
        shell_single_quote(&plan.kind),
        shell_single_quote(&plan.log_path),
        shell_single_quote(&command),
        command
    )
}

#[cfg(any(target_os = "linux", target_os = "macos", unix))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(any(target_os = "linux", target_os = "macos", unix))]
fn shell_env_key(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn command_exists(program: &str) -> bool {
    Command::new("sh")
        .args(["-lc", &format!("command -v {} >/dev/null 2>&1", program)])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    #[cfg(not(windows))]
    use super::backend_status;
    #[cfg(windows)]
    use super::{LaunchPlan, build_windows_launch_script};
    #[cfg(not(windows))]
    use crate::project_tools::LaunchPresentationMode;

    #[test]
    #[cfg(windows)]
    fn windows_launch_script_prints_launch_metadata_before_exec() {
        let plan = LaunchPlan {
            cwd: PathBuf::from("D:\\code\\rustpilot\\rustpilot"),
            window_title: "Rustpilot resident concierge".to_string(),
            command_line: vec![
                "D:\\code\\rustpilot\\rustpilot\\target\\debug\\rustpilot.exe".to_string(),
                "resident-agent-run".to_string(),
            ],
            env: vec![
                (
                    "RUSTPILOT_LAUNCH_LOG".to_string(),
                    "D:\\code\\rustpilot\\rustpilot\\.team\\launch_logs\\launch-123.log"
                        .to_string(),
                ),
                ("RUSTPILOT_LAUNCH_ID".to_string(), "launch-123".to_string()),
            ],
            launch_id: "launch-123".to_string(),
            agent_id: "concierge".to_string(),
            kind: "resident".to_string(),
            log_path: "D:\\code\\rustpilot\\rustpilot\\.team\\launch_logs\\launch-123.log"
                .to_string(),
        };
        let script = build_windows_launch_script(&plan);
        assert!(script.contains("echo [launch] id=launch-123 agent=concierge kind=resident"));
        assert!(script.contains(
            "echo [launch] log=D:\\code\\rustpilot\\rustpilot\\.team\\launch_logs\\launch-123.log"
        ));
        assert!(script.contains(
            "D:\\code\\rustpilot\\rustpilot\\target\\debug\\rustpilot.exe resident-agent-run"
        ));
    }

    #[test]
    #[cfg(not(windows))]
    fn non_windows_single_window_mode_stays_single_window() {
        let status = backend_status(LaunchPresentationMode::SingleWindow);
        assert_eq!(status.effective_mode, LaunchPresentationMode::SingleWindow);
    }
}
