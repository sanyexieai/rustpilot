use rustpilot::external_tools::{external_tool_definitions, handle_external_tool_call};
use rustpilot::openai_compat::{ToolCall, ToolCallFunction};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn global_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_global() -> MutexGuard<'static, ()> {
    global_lock().lock().unwrap_or_else(|err| err.into_inner())
}

fn make_temp_dir(name: &str) -> PathBuf {
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

fn write_skill(dir: &Path, with_tests: bool) {
    let skill_dir = dir.join("echo-tool");
    fs::create_dir_all(skill_dir.join("tests")).expect("create tests dir");

    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: echo_external\ndescription: echo input json\ntool_language: python\ntool_runtime: python 3\ntool_command: python\ntool_args_json: [\"./tool.py\"]\n---\n",
    )
    .expect("write skill");
    fs::write(
        skill_dir.join("tool.py"),
        "#!/usr/bin/env python3\nimport sys\n\n\ndef main() -> int:\n    data = sys.stdin.read()\n    sys.stdout.write(data)\n    return 0\n\n\nif __name__ == \"__main__\":\n    raise SystemExit(main())\n",
    )
    .expect("write tool");

    if with_tests {
        fs::write(
            skill_dir.join("tests").join("smoke.json"),
            "{\n  \"name\": \"smoke\",\n  \"arguments\": { \"name\": \"smoke\" },\n  \"expect_status\": 0,\n  \"expect_stdout_contains\": \"\\\"smoke\\\"\"\n}\n",
        )
        .expect("write test");
    }
}

#[test]
fn external_tool_with_tests_is_loaded_and_executable() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_skill");
    write_skill(&dir, true);

    unsafe {
        std::env::set_var("SKILLS_DIR", &dir);
    }
    let tools = external_tool_definitions();
    assert!(
        tools
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    let call = ToolCall {
        id: "call-echo".to_string(),
        r#type: "function".to_string(),
        function: ToolCallFunction {
            name: "echo_external".to_string(),
            arguments: r#"{"name":"alice"}"#.to_string(),
        },
    };
    let output = handle_external_tool_call(&call)
        .expect("run external tool")
        .expect("matched external tool");
    assert!(output.contains("\"name\":\"alice\""));

    unsafe {
        std::env::remove_var("SKILLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn external_tool_without_tests_is_skipped() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_skill_no_tests");
    write_skill(&dir, false);

    unsafe {
        std::env::set_var("SKILLS_DIR", &dir);
    }
    let tools = external_tool_definitions();
    assert!(
        !tools
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    unsafe {
        std::env::remove_var("SKILLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn external_tool_is_retested_and_unloaded_after_test_change() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_retest");
    write_skill(&dir, true);

    unsafe {
        std::env::set_var("SKILLS_DIR", &dir);
    }

    let tools_before = external_tool_definitions();
    assert!(
        tools_before
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    let smoke_path = dir.join("echo-tool").join("tests").join("smoke.json");
    fs::write(
        smoke_path,
        "{\n  \"name\": \"smoke\",\n  \"arguments\": { \"name\": \"smoke\" },\n  \"expect_status\": 0,\n  \"expect_stdout_contains\": \"NOT_MATCH\"\n}\n",
    )
    .expect("overwrite test");

    let tools_after = external_tool_definitions();
    assert!(
        !tools_after
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    unsafe {
        std::env::remove_var("SKILLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}
