# Rustpilot

Rustpilot is a Rust-based coding agent CLI inspired by Claude Code. It combines a chat loop, project-local tools, task/worktree orchestration, session persistence, approval policies, skills, MCP tool loading, and an internal UI server.

## Current Status

The repository is in active development, but the main runtime is already wired together:

- CLI entrypoint and chat loop
- Persistent sessions
- Approval modes: `auto`, `read_only`, `manual`
- Team/task/worktree orchestration
- Resident agents and internal UI server
- Skill loading from `skills/`
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

## Common CLI Commands

- `/tasks`
- `/agents`
- `/residents`
- `/sessions`
- `/session current`
- `/session new [label]`
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
- `/team run <goal>`
- `/team start [max_parallel]`
- `/team stop`
- `/team status`
- `/skills`
- `/skill <name>`
- `/skill-tool-init <name>`
- `/mcp-tool-init <name>`

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

## Skills

External tools can be loaded from `skills/` or a custom `SKILLS_DIR`.

Each skill directory must contain:

```text
skills/
  example-tool/
    SKILL.md
    tests/
      smoke.json
```

The loader validates and runs the tool tests before exposing the tool to the agent.

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
