use rustpilot::constants::LLM_TIMEOUT_SECS;
use rustpilot::runtime_env::{detect_repo_root, llm_timeout_secs};
use std::fs;
use std::path::Path;
use std::process::Command;

fn run(repo: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run command");
    assert!(
        output.status.success(),
        "{} {:?} failed: {}{}",
        program,
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo(path: &Path) {
    run(path, "git", &["init"]);
    run(path, "git", &["config", "user.name", "Codex"]);
    run(path, "git", &["config", "user.email", "codex@example.com"]);
    fs::write(path.join("README.md"), "hello\n").expect("write readme");
    run(path, "git", &["add", "."]);
    run(path, "git", &["commit", "-m", "init"]);
}

#[test]
fn detect_repo_root_finds_parent_repo() {
    let temp = std::env::temp_dir()
        .join("s12_tests")
        .join(format!("detect_repo_root_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).expect("create temp dir");
    init_git_repo(&temp);
    let nested = temp.join("nested").join("child");
    fs::create_dir_all(&nested).expect("create nested");

    let root = detect_repo_root(&nested).expect("detect root");
    assert_eq!(root, temp);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn llm_timeout_uses_default_when_env_missing() {
    unsafe {
        std::env::remove_var("LLM_TIMEOUT_SECS");
    }
    assert_eq!(llm_timeout_secs(), LLM_TIMEOUT_SECS);
}
