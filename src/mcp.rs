use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use crate::openai_compat::{Tool, ToolCall, ToolFunction};

#[derive(Debug, Clone, Deserialize)]
struct McpToolConfig {
    name: String,
    description: String,
    #[serde(default)]
    parameters: Option<Value>,
    server: McpServerConfig,
    mcp_tool: String,
}

#[derive(Debug, Clone, Deserialize)]
struct McpServerConfig {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct McpTestCase {
    name: String,
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    expect_error: bool,
    #[serde(default)]
    expect_text_contains: Option<String>,
}

#[derive(Debug, Clone)]
struct LoadedMcpTool {
    config: McpToolConfig,
    dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct McpCache {
    fingerprint: u64,
    tools: Vec<LoadedMcpTool>,
}

fn cache() -> &'static Mutex<McpCache> {
    static CACHE: OnceLock<Mutex<McpCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(McpCache::default()))
}

struct JsonRpcStdio {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

pub fn mcp_tool_definitions() -> Vec<Tool> {
    match with_loaded_tools(|tools| {
        let defs = tools
            .iter()
            .map(|tool| Tool {
                r#type: "function".to_string(),
                function: ToolFunction {
                    name: tool.config.name.clone(),
                    description: format!(
                        "[mcp:{}] {}",
                        tool.config.server.name, tool.config.description
                    ),
                    parameters: tool
                        .config
                        .parameters
                        .clone()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                },
            })
            .collect();
        Ok(defs)
    }) {
        Ok(defs) => defs,
        Err(err) => {
            eprintln!("[mcp] load failed: {}", err);
            Vec::new()
        }
    }
}

pub fn handle_mcp_tool_call(call: &ToolCall) -> anyhow::Result<Option<String>> {
    with_loaded_tools(|tools| {
        let Some(tool) = tools.iter().find(|t| t.config.name == call.function.name) else {
            return Ok(None);
        };

        let args: Value = serde_json::from_str(&call.function.arguments)
            .with_context(|| format!("invalid mcp tool arguments: {}", call.function.arguments))?;
        let args_obj = ensure_object(args)?;

        let mut rpc = connect_server(&tool.config.server)?;
        let result = rpc.request(
            "tools/call",
            json!({
                "name": tool.config.mcp_tool,
                "arguments": args_obj
            }),
        )?;
        Ok(Some(serde_json::to_string_pretty(&result)?))
    })
}

fn with_loaded_tools<T>(
    f: impl FnOnce(&[LoadedMcpTool]) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut guard = cache().lock().unwrap_or_else(|err| err.into_inner());
    let Some(base) = resolve_mcps_dir()? else {
        guard.fingerprint = 0;
        guard.tools.clear();
        return f(&[]);
    };

    let fingerprint = compute_dir_fingerprint(&base)?;
    if fingerprint != guard.fingerprint {
        guard.tools = load_mcp_tools(&base)?;
        guard.fingerprint = fingerprint;
    }

    f(&guard.tools)
}

fn resolve_mcps_dir() -> anyhow::Result<Option<PathBuf>> {
    if let Ok(dir) = std::env::var("MCPS_DIR") {
        let path = PathBuf::from(dir);
        return Ok(path.is_dir().then_some(path));
    }

    let cwd = std::env::current_dir()?;
    let direct = cwd.join("mcps");
    Ok(direct.is_dir().then_some(direct))
}

pub fn init_mcp_tool(raw_name: &str) -> anyhow::Result<PathBuf> {
    let name = normalize_name(raw_name);
    if name.is_empty() {
        anyhow::bail!("invalid mcp tool name: {}", raw_name);
    }

    let base = resolve_or_create_mcps_dir()?;
    let dir = base.join(&name);
    if dir.exists() {
        anyhow::bail!("mcp tool already exists: {}", dir.display());
    }

    fs::create_dir_all(dir.join("tests"))?;
    fs::write(dir.join("mcp.json"), mcp_json_template(&name))?;
    fs::write(dir.join("tests").join("smoke.json"), mcp_test_template())?;
    Ok(dir)
}

fn resolve_or_create_mcps_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("MCPS_DIR") {
        let path = PathBuf::from(dir);
        fs::create_dir_all(&path)?;
        return Ok(path);
    }

    let cwd = std::env::current_dir()?;
    let path = cwd.join("mcps");
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn normalize_name(input: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for ch in input.trim().chars() {
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

fn mcp_json_template(name: &str) -> String {
    format!(
        "{{\n  \"name\": \"mcp_{}\",\n  \"description\": \"describe this MCP tool\",\n  \"parameters\": {{\n    \"type\": \"object\",\n    \"properties\": {{\n      \"path\": {{ \"type\": \"string\" }}\n    }},\n    \"required\": [\"path\"]\n  }},\n  \"mcp_tool\": \"read_file\",\n  \"server\": {{\n    \"name\": \"filesystem\",\n    \"command\": \"npx\",\n    \"args\": [\"-y\", \"@modelcontextprotocol/server-filesystem\", \".\"]\n  }}\n}}\n",
        name.replace('-', "_")
    )
}

fn mcp_test_template() -> &'static str {
    "{\n  \"name\": \"smoke\",\n  \"arguments\": { \"path\": \"README.md\" },\n  \"expect_error\": false\n}\n"
}

fn compute_dir_fingerprint(base: &Path) -> anyhow::Result<u64> {
    let mut files = Vec::new();
    collect_files(base, base, &mut files)?;
    files.sort();

    let mut hasher = DefaultHasher::new();
    for relative in files {
        relative.hash(&mut hasher);
        let meta = fs::metadata(base.join(&relative))?;
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

fn load_mcp_tools(base: &Path) -> anyhow::Result<Vec<LoadedMcpTool>> {
    let mut loaded = Vec::new();
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        let config_path = dir.join("mcp.json");
        let tests_dir = dir.join("tests");
        if !config_path.is_file() || !tests_dir.is_dir() {
            continue;
        }

        let content = fs::read_to_string(&config_path)?;
        let config: McpToolConfig = serde_json::from_str(&content)
            .with_context(|| format!("invalid mcp tool config: {}", config_path.display()))?;
        let tool = LoadedMcpTool {
            config,
            dir: dir.clone(),
        };

        if let Err(err) = validate_and_test_tool(&tool) {
            eprintln!(
                "[mcp] skip '{}' ({}): {}",
                tool.config.name,
                tool.dir.display(),
                err
            );
            continue;
        }

        loaded.push(tool);
    }
    Ok(loaded)
}

fn validate_and_test_tool(tool: &LoadedMcpTool) -> anyhow::Result<()> {
    if tool.config.name.trim().is_empty() {
        anyhow::bail!("name is required");
    }
    if tool.config.description.trim().is_empty() {
        anyhow::bail!("description is required");
    }
    if tool.config.mcp_tool.trim().is_empty() {
        anyhow::bail!("mcp_tool is required");
    }
    if tool.config.server.command.trim().is_empty() {
        anyhow::bail!("server.command is required");
    }

    let tests = load_tests(&tool.dir.join("tests"))?;
    if tests.is_empty() {
        anyhow::bail!("tests/ must contain at least one .json file");
    }

    for case in tests {
        run_test_case(tool, &case).with_context(|| format!("test '{}' failed", case.name))?;
    }
    Ok(())
}

fn load_tests(dir: &Path) -> anyhow::Result<Vec<McpTestCase>> {
    let mut tests = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        let test: McpTestCase = serde_json::from_str(&content)
            .with_context(|| format!("invalid test case: {}", path.display()))?;
        tests.push(test);
    }
    Ok(tests)
}

fn run_test_case(tool: &LoadedMcpTool, test: &McpTestCase) -> anyhow::Result<()> {
    let args_obj = ensure_object(test.arguments.clone())?;
    let mut rpc = connect_server(&tool.config.server)?;
    let call = rpc.request(
        "tools/call",
        json!({
            "name": tool.config.mcp_tool,
            "arguments": args_obj
        }),
    );

    match (call, test.expect_error) {
        (Ok(result), false) => {
            if let Some(expected) = &test.expect_text_contains {
                let text = serde_json::to_string(&result)?;
                if !text.contains(expected) {
                    anyhow::bail!("result missing expected fragment '{}'", expected);
                }
            }
            Ok(())
        }
        (Err(_), true) => Ok(()),
        (Ok(_), true) => anyhow::bail!("expected error but call succeeded"),
        (Err(err), false) => Err(err),
    }
}

fn ensure_object(value: Value) -> anyhow::Result<Value> {
    match value {
        Value::Object(map) => Ok(Value::Object(map)),
        Value::Null => Ok(json!({})),
        _ => anyhow::bail!("arguments must be a JSON object"),
    }
}

fn connect_server(server: &McpServerConfig) -> anyhow::Result<JsonRpcStdio> {
    let mut command = Command::new(resolve_command_path(&server.command));
    command.args(&server.args);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(cwd) = &server.cwd {
        command.current_dir(resolve_cwd(cwd)?);
    }
    for (k, v) in &server.env {
        command.env(k, v);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn mcp server '{}'", server.name))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("mcp server '{}' missing stdin", server.name))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("mcp server '{}' missing stdout", server.name))?;

    let mut rpc = JsonRpcStdio {
        stdin,
        stdout: BufReader::new(stdout),
        next_id: 1,
    };
    initialize_session(&mut rpc)?;
    Ok(rpc)
}

fn initialize_session(rpc: &mut JsonRpcStdio) -> anyhow::Result<()> {
    let _ = rpc.request(
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "rustpilot", "version": "0.1.0" }
        }),
    )?;
    rpc.notify("notifications/initialized", json!({}))?;
    Ok(())
}

impl JsonRpcStdio {
    fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;

        loop {
            let msg = self.read_message()?;
            let Some(msg_id) = msg.get("id") else {
                continue;
            };
            if msg_id.as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = msg.get("error") {
                anyhow::bail!("mcp error: {}", error);
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.send_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
    }

    fn send_message(&mut self, message: &Value) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(message)?;
        let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(&bytes)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read_message(&mut self) -> anyhow::Result<Value> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line)?;
            if read == 0 {
                anyhow::bail!("mcp server closed connection");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            let lower = trimmed.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                content_length = Some(value.trim().parse::<usize>()?);
            }
        }

        let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf)?;
        let value: Value = serde_json::from_slice(&buf)?;
        Ok(value)
    }
}

fn resolve_command_path(command: &str) -> PathBuf {
    PathBuf::from(command)
}

fn resolve_cwd(cwd: &str) -> anyhow::Result<PathBuf> {
    let path = PathBuf::from(cwd);
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()?.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn ensure_object_accepts_object_and_null() {
        let obj = ensure_object(json!({"a":1})).expect("obj");
        assert!(obj.is_object());

        let null_obj = ensure_object(Value::Null).expect("null");
        assert_eq!(null_obj, json!({}));
    }

    #[test]
    fn init_mcp_tool_creates_template_files() {
        let temp = std::env::temp_dir().join(format!(
            "mcp_init_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&temp).expect("create temp");
        unsafe {
            std::env::set_var("MCPS_DIR", &temp);
        }

        let created = init_mcp_tool("File Read").expect("init mcp tool");
        assert!(created.join("mcp.json").is_file());
        assert!(created.join("tests").join("smoke.json").is_file());

        unsafe {
            std::env::remove_var("MCPS_DIR");
        }
        let _ = fs::remove_dir_all(&temp);
    }
}
