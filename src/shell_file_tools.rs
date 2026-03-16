use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MAX_OUTPUT_BYTES: usize = 50_000;

#[derive(Debug, Deserialize)]
pub struct BashArgs {
    pub command: String,
}

#[derive(Debug, Deserialize)]
pub struct ReadFileArgs {
    pub path: String,
    #[serde(default)]
    pub max_lines: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct EditFileArgs {
    pub path: String,
    pub old: String,
    pub new: String,
}

pub struct BashTool;

impl BashTool {
    pub fn run(command: &str) -> anyhow::Result<String> {
        if is_dangerous_command(command) {
            anyhow::bail!("refused dangerous shell command");
        }
        run_shell_command(command, None)
    }
}

pub fn is_likely_long_running_command(command: &str) -> bool {
    let normalized = command.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }

    const LONG_RUNNING_PATTERNS: &[&str] = &[
        "npm run dev",
        "npm start",
        "pnpm dev",
        "pnpm start",
        "yarn dev",
        "yarn start",
        "bun dev",
        "vite",
        "webpack serve",
        "next dev",
        "nuxt dev",
        "astro dev",
        "cargo run",
        "cargo watch",
        "python -m http.server",
        "python -m uvicorn",
        "uvicorn",
        "flask run",
        "django-admin runserver",
        "npm create",
        "npx create-",
        "tail -f",
        "ping ",
    ];

    if LONG_RUNNING_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
    {
        return true;
    }

    normalized.ends_with(" --watch")
        || normalized.contains(" --watch ")
        || normalized.ends_with(" watch")
        || normalized.starts_with("watch ")
        || normalized.contains(" serve ")
        || normalized.ends_with(" serve")
}

pub fn is_likely_expensive_command(command: &str) -> bool {
    let normalized = command.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if is_likely_long_running_command(&normalized) {
        return true;
    }

    const EXPENSIVE_PATTERNS: &[&str] = &[
        "cargo test",
        "cargo bench",
        "cargo build",
        "cargo clippy",
        "cargo doc",
        "npm install",
        "npm ci",
        "npm run build",
        "npm test",
        "pnpm install",
        "pnpm build",
        "pnpm test",
        "yarn install",
        "yarn build",
        "yarn test",
        "bun install",
        "bun test",
        "pytest",
        "playwright test",
        "vitest",
        "jest",
        "go test",
        "mvn test",
        "gradle test",
        "docker build",
        "docker compose up",
        "git clone",
    ];

    EXPENSIVE_PATTERNS
        .iter()
        .any(|pattern| normalized.contains(pattern))
}

pub fn run_shell_command(command: &str, current_dir: Option<&Path>) -> anyhow::Result<String> {
    let normalized = normalize_command(command);
    let output = shell_command(&normalized, current_dir)?.output()?;
    Ok(format_command_output(output))
}

pub fn is_dangerous_command(command: &str) -> bool {
    const COMMON_TOKENS: &[&str] = &["shutdown", "reboot"];
    const UNIX_TOKENS: &[&str] = &["rm -rf /", "sudo", "> /dev/"];
    const WINDOWS_TOKENS: &[&str] = &["Remove-Item", "rd /s /q", "del /f /s /q", "format "];

    COMMON_TOKENS
        .iter()
        .chain(UNIX_TOKENS.iter())
        .chain(WINDOWS_TOKENS.iter())
        .any(|token| command.contains(token))
}

pub fn is_read_only_command(command: &str) -> bool {
    let normalized = command.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if normalized.contains("&&")
        || normalized.contains("||")
        || normalized.contains(';')
        || normalized.contains('|')
        || normalized.contains('>')
        || normalized.contains(">>")
    {
        return false;
    }

    const PREFIXES: &[&str] = &[
        "pwd",
        "cd ",
        "ls",
        "dir",
        "find ",
        "rg ",
        "git status",
        "git diff",
        "git log",
        "git show",
        "git branch",
        "git rev-parse",
        "git remote -v",
        "cat ",
        "type ",
        "get-content ",
        "echo ",
        "which ",
        "where ",
    ];

    PREFIXES.iter().any(|prefix| {
        normalized == *prefix || normalized.starts_with(&format!("{} ", prefix.trim_end()))
    })
}

pub fn read_file(args: &ReadFileArgs) -> anyhow::Result<String> {
    let path = safe_path(&args.path)?;
    let content = fs::read_to_string(&path)?;

    let limited = if let Some(max_lines) = args.max_lines {
        let mut lines: Vec<&str> = content.lines().collect();
        if lines.len() > max_lines {
            let omitted = lines.len() - max_lines;
            lines.truncate(max_lines);
            let mut out = lines.join("\n");
            out.push_str(&format!("\n...(还有 {} 行)", omitted));
            out
        } else {
            lines.join("\n")
        }
    } else {
        content
    };

    Ok(truncate_output(limited))
}

pub fn write_file(args: &WriteFileArgs) -> anyhow::Result<String> {
    let path = safe_path(&args.path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &args.content)?;
    Ok(format!(
        "已写入 {} 字节到 {}",
        args.content.len(),
        path.display()
    ))
}

pub fn edit_file(args: &EditFileArgs) -> anyhow::Result<String> {
    let path = safe_path(&args.path)?;
    let content = fs::read_to_string(&path)?;

    if let Some(index) = content.find(&args.old) {
        let mut updated = String::with_capacity(content.len() - args.old.len() + args.new.len());
        updated.push_str(&content[..index]);
        updated.push_str(&args.new);
        updated.push_str(&content[index + args.old.len()..]);
        fs::write(&path, updated)?;
        Ok(format!("已更新 {}", path.display()))
    } else {
        anyhow::bail!("在 {} 中未找到目标文本", path.display());
    }
}

fn safe_path(input: &str) -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    let raw = PathBuf::from(input);
    let candidate = if raw.is_absolute() {
        raw
    } else {
        cwd.join(raw)
    };

    match candidate.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) => Ok(candidate),
    }
}

fn truncate_output(mut text: String) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text;
    }
    text.truncate(MAX_OUTPUT_BYTES);
    text.push_str("\n...(输出已截断)");
    text
}

fn format_command_output(output: Output) -> String {
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    if combined.trim().is_empty() {
        combined = "(no output)".to_string();
    }

    truncate_output(combined)
}

fn normalize_command(command: &str) -> String {
    normalize_command_impl(command)
}

#[cfg(target_os = "windows")]
fn normalize_command_impl(command: &str) -> String {
    let mut normalized = command.replace("2>/dev/null", "2>$null");
    normalized = normalized.replace("ls -la", "Get-ChildItem -Force");
    normalized = normalized.replace("&&", "; if (-not $?) { exit 1 };");

    if let Some((left, right)) = normalized.split_once("||") {
        let right = right.trim();
        if let Some(message) = right.strip_prefix("echo ").map(str::trim) {
            normalized = format!(
                "{}; if (-not $?) {{ Write-Output {} }}",
                left.trim_end(),
                message
            );
        }
    }

    normalized
}

#[cfg(not(target_os = "windows"))]
fn normalize_command_impl(command: &str) -> String {
    command.to_string()
}

#[cfg(target_os = "windows")]
fn shell_command(command: &str, current_dir: Option<&Path>) -> anyhow::Result<Command> {
    let wrapped = wrap_powershell_command(command);
    let mut process = Command::new("powershell");
    process.args(["-NoProfile", "-Command", &wrapped]);
    if let Some(dir) = current_dir {
        process.current_dir(dir);
    }
    Ok(process)
}

#[cfg(not(target_os = "windows"))]
fn shell_command(command: &str, current_dir: Option<&Path>) -> anyhow::Result<Command> {
    let mut process = Command::new("sh");
    process.args(["-lc", command]);
    if let Some(dir) = current_dir {
        process.current_dir(dir);
    }
    Ok(process)
}

#[cfg(target_os = "windows")]
fn wrap_powershell_command(command: &str) -> String {
    format!(
        "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(); \
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); \
$OutputEncoding = [Console]::OutputEncoding; {}",
        command
    )
}

#[cfg(test)]
mod tests {
    use super::{
        is_likely_expensive_command, is_likely_long_running_command, is_read_only_command,
    };

    #[test]
    fn read_only_command_detection_accepts_common_queries() {
        assert!(is_read_only_command("pwd"));
        assert!(is_read_only_command("git status"));
        assert!(is_read_only_command("rg todo src"));
        assert!(is_read_only_command("Get-Content README.md"));
    }

    #[test]
    fn read_only_command_detection_rejects_writes_and_chaining() {
        assert!(!is_read_only_command("git add README.md"));
        assert!(!is_read_only_command("echo hi > out.txt"));
        assert!(!is_read_only_command("pwd && git status"));
    }

    #[test]
    fn long_running_command_detection_flags_dev_servers() {
        assert!(is_likely_long_running_command("npm run dev"));
        assert!(is_likely_long_running_command("cargo run"));
        assert!(is_likely_long_running_command("python -m uvicorn app:app --reload"));
        assert!(is_likely_long_running_command("vite --watch"));
        assert!(!is_likely_long_running_command("cargo test"));
        assert!(!is_likely_long_running_command("git status"));
    }

    #[test]
    fn expensive_command_detection_flags_builds_and_tests() {
        assert!(is_likely_expensive_command("cargo test"));
        assert!(is_likely_expensive_command("npm install"));
        assert!(is_likely_expensive_command("docker build ."));
        assert!(!is_likely_expensive_command("git status"));
        assert!(!is_likely_expensive_command("Get-Content README.md"));
    }
}
