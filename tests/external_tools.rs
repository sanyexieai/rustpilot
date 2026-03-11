use rustpilot::external_tools::{
    external_tool_definitions, external_tool_summaries, handle_external_tool_call,
    import_external_tool,
};
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
    write_skill_with_frontmatter(
        dir,
        with_tests,
        "schema_version = 1\n\n[tool]\nname = \"echo_external\"\ndescription = \"echo input json\"\nlevel = \"generic\"\nruntime_kind = \"script\"\nlanguage = \"python\"\nruntime = \"python 3\"\ncommand = \"python\"\nargs = [\"./tool.py\"]\n",
    );
}

fn write_skill_with_frontmatter(dir: &Path, with_tests: bool, frontmatter: &str) {
    let level = if frontmatter.contains("level = \"kernel\"") {
        "kernel"
    } else if frontmatter.contains("level = \"project\"") {
        "project"
    } else if frontmatter.contains("level = \"feature\"") {
        "feature"
    } else {
        "generic"
    };
    let skill_dir = dir.join(level).join("echo-tool");
    fs::create_dir_all(skill_dir.join("tests")).expect("create tests dir");

    fs::write(skill_dir.join("tool.toml"), frontmatter).expect("write manifest");
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
fn external_tool_summary_includes_capability_metadata() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_summary");
    write_skill(&dir, true);

    unsafe {
        std::env::set_var("TOOLS_DIR", &dir);
    }
    let tools = external_tool_summaries();
    let summary = tools
        .iter()
        .find(|tool| tool.name == "echo_external")
        .expect("external tool summary");
    assert_eq!(summary.capability_level.as_deref(), Some("generic"));
    assert_eq!(summary.runtime_kind.as_deref(), Some("script"));

    unsafe {
        std::env::remove_var("TOOLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn external_tool_with_tests_is_loaded_and_executable() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_skill");
    write_skill(&dir, true);

    unsafe {
        std::env::set_var("TOOLS_DIR", &dir);
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
        std::env::remove_var("TOOLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn external_tool_without_tests_is_skipped() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_skill_no_tests");
    write_skill(&dir, false);

    unsafe {
        std::env::set_var("TOOLS_DIR", &dir);
    }
    let tools = external_tool_definitions();
    assert!(
        !tools
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    unsafe {
        std::env::remove_var("TOOLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn external_tool_is_retested_and_unloaded_after_test_change() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_retest");
    write_skill(&dir, true);

    unsafe {
        std::env::set_var("TOOLS_DIR", &dir);
    }

    let tools_before = external_tool_definitions();
    assert!(
        tools_before
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    let smoke_path = dir
        .join("generic")
        .join("echo-tool")
        .join("tests")
        .join("smoke.json");
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
        std::env::remove_var("TOOLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn kernel_level_python_tool_is_rejected() {
    let _guard = lock_global();
    let dir = make_temp_dir("external_tools_kernel_python");
    write_skill_with_frontmatter(
        &dir,
        true,
        "schema_version = 1\n\n[tool]\nname = \"echo_external\"\ndescription = \"echo input json\"\nlevel = \"kernel\"\nruntime_kind = \"script\"\nlanguage = \"python\"\nruntime = \"python 3\"\ncommand = \"python\"\nargs = [\"./tool.py\"]\n",
    );

    unsafe {
        std::env::set_var("TOOLS_DIR", &dir);
    }
    let tools = external_tool_definitions();
    assert!(
        !tools
            .iter()
            .any(|tool| tool.function.name == "echo_external")
    );

    unsafe {
        std::env::remove_var("TOOLS_DIR");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn import_tool_copies_into_canonical_level_directory() {
    let _guard = lock_global();
    let temp = make_temp_dir("external_tools_import");
    let source = temp.join("source-tool");
    write_skill(&source, true);
    let source_dir = source.join("generic").join("echo-tool");

    let install_root = temp.join("install-root");
    unsafe {
        std::env::set_var("TOOLS_DIR", &install_root);
    }

    let imported = import_external_tool(&source_dir).expect("import tool");
    assert!(imported.ends_with(Path::new("generic").join("echo-external")));
    assert!(imported.join("tool.toml").is_file());
    assert!(imported.join("tests").join("smoke.json").is_file());

    unsafe {
        std::env::remove_var("TOOLS_DIR");
    }
    let _ = fs::remove_dir_all(temp);
}
