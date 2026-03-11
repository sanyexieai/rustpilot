use anyhow::Context;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use crate::openai_compat::{Tool, ToolCall, ToolFunction};
use crate::tool_capability::{ToolCapabilityLevel, ToolRuntimeKind};
use crate::tool_manifest::{ToolManifest, resolve_or_create_tools_dir, resolve_tools_dir};
use crate::wire::WireToolSummary;

#[derive(Debug, Clone)]
struct ExternalToolConfig {
    name: String,
    description: String,
    language: String,
    runtime: Option<String>,
    command: String,
    args: Vec<String>,
    capability_level: ToolCapabilityLevel,
    runtime_kind: ToolRuntimeKind,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalToolTestCase {
    name: String,
    arguments: serde_json::Value,
    #[serde(default)]
    expect_status: Option<i32>,
    #[serde(default)]
    expect_stdout_contains: Option<String>,
    #[serde(default)]
    expect_stderr_contains: Option<String>,
}

#[derive(Debug, Clone)]
struct LoadedExternalTool {
    config: ExternalToolConfig,
    skill_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct ExternalToolCache {
    fingerprint: u64,
    tools: Vec<LoadedExternalTool>,
}

fn cache() -> &'static Mutex<ExternalToolCache> {
    static CACHE: OnceLock<Mutex<ExternalToolCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ExternalToolCache::default()))
}

pub fn external_tool_definitions() -> Vec<Tool> {
    match with_loaded_tools(|tools| {
        let defs = tools
            .iter()
            .map(|item| Tool {
                r#type: "function".to_string(),
                function: ToolFunction {
                    name: item.config.name.clone(),
                    description: item.config.description.clone(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "additionalProperties": true
                    }),
                },
            })
            .collect();
        Ok(defs)
    }) {
        Ok(tools) => tools,
        Err(err) => {
            eprintln!("[external-tools] load failed: {}", err);
            Vec::new()
        }
    }
}

pub fn external_tool_summaries() -> Vec<WireToolSummary> {
    match with_loaded_tools(|tools| {
        Ok(tools
            .iter()
            .map(|item| WireToolSummary {
                name: item.config.name.clone(),
                source: "external".to_string(),
                description: item.config.description.clone(),
                parameters: serde_json::json!({
                    "type": "object",
                    "additionalProperties": true
                }),
                capability_level: Some(item.config.capability_level.as_str().to_string()),
                runtime_kind: Some(item.config.runtime_kind.as_str().to_string()),
            })
            .collect())
    }) {
        Ok(tools) => tools,
        Err(err) => {
            eprintln!("[external-tools] summary load failed: {}", err);
            Vec::new()
        }
    }
}

pub fn handle_external_tool_call(call: &ToolCall) -> anyhow::Result<Option<String>> {
    with_loaded_tools(|tools| {
        let Some(tool) = tools
            .iter()
            .find(|item| item.config.name == call.function.name)
        else {
            return Ok(None);
        };

        let output = execute_external_tool(tool, &call.function.arguments)
            .with_context(|| format!("external tool '{}' failed", call.function.name))?;
        Ok(Some(output))
    })
}

fn with_loaded_tools<T>(
    f: impl FnOnce(&[LoadedExternalTool]) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut guard = cache().lock().unwrap_or_else(|err| err.into_inner());
    let Some(skills_dir) = resolve_tools_dir()? else {
        guard.fingerprint = 0;
        guard.tools.clear();
        return f(&[]);
    };

    let fingerprint = compute_skills_fingerprint(&skills_dir)?;
    if fingerprint != guard.fingerprint {
        let loaded = load_external_tools(&skills_dir)?;
        guard.fingerprint = fingerprint;
        guard.tools = loaded;
    }
    f(&guard.tools)
}

fn compute_skills_fingerprint(skills_dir: &Path) -> anyhow::Result<u64> {
    let mut files = Vec::new();
    collect_files(skills_dir, skills_dir, &mut files)?;
    files.sort();

    let mut hasher = DefaultHasher::new();
    for file in files {
        file.hash(&mut hasher);
        let meta = fs::metadata(skills_dir.join(&file))?;
        meta.len().hash(&mut hasher);
        if let Ok(modified) = meta.modified()
            && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
        {
            duration.as_secs().hash(&mut hasher);
            duration.subsec_nanos().hash(&mut hasher);
        }
    }
    Ok(hasher.finish())
}

fn collect_files(root: &Path, current: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
            continue;
        }
        if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn load_external_tools(skills_dir: &Path) -> anyhow::Result<Vec<LoadedExternalTool>> {
    let mut loaded = Vec::new();
    for level in ToolCapabilityLevel::all() {
        let level_dir = skills_dir.join(level.directory_name());
        if !level_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&level_dir)? {
            let entry = entry?;
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }

            let skill_md = skill_dir.join("tool.toml");
            let tests_dir = skill_dir.join("tests");
            if !skill_md.is_file() || !tests_dir.is_dir() {
                continue;
            }

            let config = parse_tool_config_from_manifest(&skill_dir)?;
            if config.capability_level != level {
                eprintln!(
                    "[external-tools] skip '{}' ({}): tool_level '{}' does not match directory '{}'",
                    config.name,
                    skill_dir.display(),
                    config.capability_level.as_str(),
                    level.directory_name()
                );
                continue;
            }
            let tool = LoadedExternalTool {
                config,
                skill_dir: skill_dir.clone(),
            };

            if let Err(err) = validate_and_test_tool(&tool) {
                eprintln!(
                    "[external-tools] skip '{}' ({}): {}",
                    tool.config.name,
                    skill_dir.display(),
                    err
                );
                continue;
            }

            loaded.push(tool);
        }
    }

    Ok(loaded)
}

fn parse_tool_config_from_manifest(dir: &Path) -> anyhow::Result<ExternalToolConfig> {
    let manifest = ToolManifest::load_from_dir(dir)?;
    let capability_level = manifest.level()?;
    let runtime_kind = manifest.runtime_kind()?;

    Ok(ExternalToolConfig {
        name: manifest.tool.name,
        description: manifest.tool.description,
        language: manifest.tool.language,
        runtime: manifest.tool.runtime,
        command: manifest.tool.command,
        args: manifest.tool.args,
        capability_level,
        runtime_kind,
    })
}

fn validate_and_test_tool(tool: &LoadedExternalTool) -> anyhow::Result<()> {
    if tool.config.name.trim().is_empty() {
        anyhow::bail!("name is required");
    }
    if tool.config.description.trim().is_empty() {
        anyhow::bail!("description is required");
    }
    if tool.config.language.trim().is_empty() {
        anyhow::bail!("tool_language is required");
    }
    if tool.config.command.trim().is_empty() {
        anyhow::bail!("tool_command is required");
    }
    let _ = &tool.config.runtime;
    if tool.config.capability_level == ToolCapabilityLevel::Kernel
        && tool.config.runtime_kind != ToolRuntimeKind::RustBinary
    {
        anyhow::bail!("kernel-level tools must resolve to a compiled rust binary");
    }

    let tests = load_test_cases(&tool.skill_dir.join("tests"))?;
    if tests.is_empty() {
        anyhow::bail!("tests/ must contain at least one test case");
    }
    for case in &tests {
        run_test_case(tool, case).with_context(|| format!("test '{}' failed", case.name))?;
    }
    Ok(())
}

pub fn import_external_tool(source_dir: &Path) -> anyhow::Result<PathBuf> {
    let manifest = ToolManifest::load_from_dir(source_dir)?;
    let level = manifest.level()?;
    let tools_root = resolve_or_create_tools_dir()?;
    let target = tools_root
        .join(level.directory_name())
        .join(normalize_tool_dir_name(&manifest.tool.name));
    if target.exists() {
        anyhow::bail!("tool already exists: {}", target.display());
    }
    copy_dir_recursive(source_dir, &target)?;
    Ok(target)
}

fn normalize_tool_dir_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for ch in name.trim().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == '-' || ch == '_' || ch.is_whitespace() {
            Some('-')
        } else {
            None
        };
        if let Some(c) = normalized {
            if c == '-' {
                if prev_hyphen || out.is_empty() {
                    continue;
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
            out.push(c);
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn copy_dir_recursive(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let from_path = entry.path();
        let to_path = to.join(entry.file_name());
        if from_path.is_dir() {
            copy_dir_recursive(&from_path, &to_path)?;
        } else {
            fs::copy(&from_path, &to_path)?;
        }
    }
    Ok(())
}

fn load_test_cases(tests_dir: &Path) -> anyhow::Result<Vec<ExternalToolTestCase>> {
    let mut cases = Vec::new();
    for entry in fs::read_dir(tests_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let case: ExternalToolTestCase = serde_json::from_str(&content)
            .with_context(|| format!("parse test case {}", path.display()))?;
        cases.push(case);
    }
    Ok(cases)
}

fn run_test_case(tool: &LoadedExternalTool, case: &ExternalToolTestCase) -> anyhow::Result<()> {
    let arguments = serde_json::to_string(&case.arguments)?;
    let output = run_tool_process(tool, &arguments)?;
    let expected_status = case.expect_status.unwrap_or(0);
    let status_code = output.status.code().unwrap_or(-1);
    if status_code != expected_status {
        anyhow::bail!("status {} != expected {}", status_code, expected_status);
    }
    if let Some(expected) = &case.expect_stdout_contains {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains(expected) {
            anyhow::bail!("stdout missing expected fragment '{}'", expected);
        }
    }
    if let Some(expected) = &case.expect_stderr_contains {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains(expected) {
            anyhow::bail!("stderr missing expected fragment '{}'", expected);
        }
    }
    Ok(())
}

fn execute_external_tool(tool: &LoadedExternalTool, arguments: &str) -> anyhow::Result<String> {
    let output = run_tool_process(tool, arguments)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        anyhow::bail!("command exited with status {}: {}", output.status, detail);
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !stdout.trim().is_empty() {
        return Ok(stdout);
    }
    Ok(String::from_utf8_lossy(&output.stderr).to_string())
}

fn run_tool_process(tool: &LoadedExternalTool, arguments: &str) -> anyhow::Result<Output> {
    let mut command = Command::new(resolve_command_path(&tool.skill_dir, &tool.config.command));
    command.args(&tool.config.args);
    command.current_dir(&tool.skill_dir);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.env("RUSTPILOT_TOOL_NAME", &tool.config.name);
    command.env("RUSTPILOT_TOOL_ARGS", arguments);
    command.env("RUSTPILOT_TOOL_LANGUAGE", &tool.config.language);
    if let Some(runtime) = &tool.config.runtime {
        command.env("RUSTPILOT_TOOL_RUNTIME", runtime);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "spawn command '{}' for tool '{}'",
            tool.config.command, tool.config.name
        )
    })?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(arguments.as_bytes())?;
    }
    Ok(child.wait_with_output()?)
}

fn resolve_command_path(base_dir: &Path, command: &str) -> PathBuf {
    if !command.contains('/') && !command.contains('\\') {
        return PathBuf::from(command);
    }
    let path = PathBuf::from(command);
    if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    }
}
