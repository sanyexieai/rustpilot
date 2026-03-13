# Rustpilot

Rustpilot is a Rust-based coding agent CLI inspired by Claude Code. It combines a chat loop, project-local tools, task/worktree orchestration, session persistence, approval policies, skills, MCP tool loading, and an internal UI server.

## Current Status

The repository is in active development, but the main runtime is already wired together:

- CLI entrypoint and chat loop
- Persistent sessions
- Approval modes: `auto`, `read_only`, `manual`
- Team/task/worktree orchestration
- Resident agents and internal UI server
- Agent-generated chat-style management UI driven by project state, UI memory, backend protocols, and structured `chat_ui` data
- Skill loading from `skills/`
- Tool discovery/import via `tool.toml`
- MCP tool loading from `mcps/`

Local verification currently passes:

```bash
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Requirements

- Rust nightly toolchain
- Git
- A `.env` file with LLM credentials

The codebase currently assumes a compatible LLM endpoint via environment variables such as:

```bash
LLM_API_KEY=your-api-key
LLM_PROVIDER=kimi-coding
LLM_API_BASE_URL=https://api.kimi.com/coding/
LLM_MODEL=kimi-for-coding
LLM_TIMEOUT_SECS=300
LLM_USER_AGENT=openclaw
```

`rustpilot` will also create or extend `.env` guidance on startup when keys are missing.

## Run

```bash
cargo run
```

The main process starts a local UI server automatically. Natural-language requests such as `open dashboard` or `打开一个管理当前状态的页面` trigger the generated management page instead of requiring a dedicated command.

## Common CLI Commands

- `/tasks`
- `/tasks tree`
- `/agents`
- `/residents`
- `/sessions`
- `/session current`
- `/session new [label]`
- `/session new [label] [--focus <lead|shell|team|worker(...)>]`
- `/session use <session_id>`
- `/focus lead`
- `/focus shell`
- `/focus team`
- `/focus worker <task_id>`
- `/focus status`
- `/approval status`
- `/approval history [reason] [limit]`
- `/approval auto`
- `/approval read_only`
- `/approval manual`
- `/shell <command>`
- `/reply <task_id> <content>`
- `/task pause <task_id>`
- `/task resume <task_id>`
- `/task cancel <task_id>`
- `/task priority <task_id> <critical|high|medium|low>`
- `/team run <goal>`
- `/team start [max_parallel]`
- `/team stop`
- `/team status`
- `/skills`
- `/skill <name>`
- `/skill-tool-init <name>`
- `/skill-tool-init <feature|project|generic|kernel> <name>`
- `/tool-import <source_dir>`
- `/mcp-tool-init <name>`

## Hierarchical Task Control

Task delegation now follows a hierarchical expansion protocol:

- Try to complete a task directly when it can be done with no more than 2 tool types and usually under 10 steps per tool
- Otherwise decompose it into sub-tasks with explicit deliverables
- Keep direct child count per parent at 10 or below
- Escalate back up the chain when depth, child count, or execution steps exceed the threshold

Useful commands:

- `/tasks tree` shows parent-child structure and threshold alerts
- `/task pause <task_id>` pauses delegated work for replanning
- `/task resume <task_id>` re-queues a paused task
- `/task cancel <task_id>` cancels delegated work
- `/task priority <task_id> <critical|high|medium|low>` changes a task priority

## Project Layout

```text
src/
  main.rs                 CLI entrypoint
  app.rs                  main application loop
  cli.rs                  CLI command parsing
  agent.rs                model/tool execution loop
  app_commands.rs         command dispatch and session actions
  wire.rs                 wire protocol types
  wire_exec.rs            wire request execution
  terminal_session.rs     persistent terminal sessions
  ui_server.rs            local HTTP/WebSocket UI server
  runtime/                focused runtime helpers
  project_tools/          tasks, worktrees, approvals, mailbox, sessions
```

The UI page is no longer maintained as a static `src/ui/index.html` template. The server generates and caches project-local artifacts under `.team/`, including:

```text
.team/
  ui_surface.json
  ui_schema.json
  ui_rules.json
  ui_page_context.json
  ui_page.html
  ui_page_request.json
```

These files represent the UI planning, schema, structured UI design rules, final HTML cache, and structured user-intent memory used by the UI agent.

By default, the intended generated UI is a chat-style control surface:

- one fixed `Main` conversation
- one `Agent Team` group thread
- a detail panel for the selected agent's transcript and runtime state

The Rust server should provide data and rules; the final UI code should come from the UI agent. The built-in fallback page is only a minimal bootstrap shell while generation is pending or retrying.

## Skills

Prompt/reference skills are loaded from `skills/` or a custom `SKILLS_DIR`.

Each skill directory must contain:

```text
skills/
  example-tool/
    SKILL.md
    tests/
      smoke.json
```

The loader validates and runs the tool tests before exposing the tool to the agent.

## Tools

Executable project tools are separate from prompt skills. They must be installed under:

```text
tools/
  feature/
    my-tool/
      tool.toml
      README.md
      tool.py
      tests/
        smoke.json
  project/
  generic/
  kernel/
```

Rules:

- Only `tools/<level>/<tool-name>/` is treated as an installed tool location
- Each tool must live in its own directory
- Each tool must include `tool.toml`
- The directory level and `tool.toml` `level` must match
- `kernel` tools must resolve to a compiled Rust binary
- `generic` tools are intended for script runtimes such as Python or Node

Minimal `tool.toml`:

```toml
schema_version = 1

[tool]
name = "echo_external"
description = "echo input json"
level = "generic"
runtime_kind = "script"
language = "python"
runtime = "python 3"
command = "python"
args = ["./tool.py"]
```

You can scaffold a new tool with:

```text
/skill-tool-init generic echo-tool
```

You can import a discovered tool into the canonical project layout with:

```text
/tool-import path/to/unpacked/tool
```

Only directories containing a valid `tool.toml` are recognized as importable tools.

## MCP Tools

MCP tools are loaded from `mcps/` or a custom `MCPS_DIR`.

Each MCP tool directory must contain:

```text
mcps/
  filesystem-read/
    mcp.json
    tests/
      smoke.json
```

You can scaffold one with:

```text
/mcp-tool-init filesystem-read
```

## Development

Use these checks before shipping changes:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## CI

GitHub Actions now enforces:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --locked`

Release builds are produced for:

- Linux x86_64
- Linux ARM64
- macOS x86_64
- macOS ARM64
- Windows x86_64

Tagged pushes matching `v*` publish a GitHub Release with packaged artifacts.

## Notes

- Older repository notes referenced `DEVLOG.md` and `TODO.md`; those files are not present and are no longer referenced here.
- Some existing source files still contain legacy mojibake in user-facing strings. The README is now normalized to UTF-8, but runtime text cleanup is still worth doing separately.

## License

MIT
