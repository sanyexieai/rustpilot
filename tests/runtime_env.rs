use rustpilot::constants::LLM_TIMEOUT_SECS;
use rustpilot::runtime_env::{detect_repo_root, ensure_env_guidance, llm_timeout_secs};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
        .join("tests")
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

fn make_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let dir = std::env::temp_dir().join("tests").join(format!(
        "{}_{}_{}",
        name,
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn ensure_env_guidance_creates_file_when_missing() {
    let dir = make_temp_dir("ensure_env_creates");
    let update = ensure_env_guidance(&dir).expect("ensure env guidance");
    let env_text = fs::read_to_string(dir.join(".env")).expect("read .env");

    assert!(update.created);
    assert!(update.added_keys.iter().any(|key| key == "LLM_API_KEY"));
    assert!(env_text.contains("LLM_API_KEY=your_api_key_here"));
    assert!(env_text.contains("LLM_TIMEOUT_SECS=120"));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ensure_env_guidance_appends_missing_keys_without_overwrite() {
    let dir = make_temp_dir("ensure_env_appends");
    let env_path = dir.join(".env");
    fs::write(
        &env_path,
        "LLM_API_KEY=custom_key\nLLM_MODEL=custom_model\n",
    )
    .expect("write .env");

    let update = ensure_env_guidance(&dir).expect("ensure env guidance");
    let env_text = fs::read_to_string(&env_path).expect("read .env");

    assert!(!update.created);
    assert!(update.added_keys.iter().any(|key| key == "LLM_PROVIDER"));
    assert!(env_text.contains("LLM_API_KEY=custom_key"));
    assert!(env_text.contains("LLM_MODEL=custom_model"));
    assert!(env_text.contains("LLM_PROVIDER=minimax"));
    assert!(env_text.contains("LLM_API_BASE_URL=https://api.minimaxi.com/v1"));
    assert!(env_text.contains("LLM_TIMEOUT_SECS=120"));

    let _ = fs::remove_dir_all(dir);
}
