use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

pub fn emit(line: impl AsRef<str>) {
    let line = line.as_ref();
    println!("{}", line);
    append(line);
}

pub fn append(line: &str) {
    let Some(path) = current_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", line);
    }
}

pub fn read_tail(path: &str, max_lines: usize) -> String {
    let Ok(content) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines = content.lines().collect::<Vec<_>>();
    let take = max_lines.max(1);
    let start = lines.len().saturating_sub(take);
    lines[start..].join("\n")
}

fn current_log_path() -> Option<PathBuf> {
    std::env::var("RUSTPILOT_LAUNCH_LOG")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
