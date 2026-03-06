# Development Log

## Final Goal

Build a cross-platform terminal session layer for `rustpilot` that can:

- create long-lived terminal sessions
- write input into a session
- read incremental output from a session
- track session status and logs
- later support optional external window presentation without making window APIs the core dependency

## Working Rule

Before each development session:

1. Review this file and the current implementation status.
2. Re-state the final goal against the current codebase.
3. Adjust the next step based on what is already working, what is blocked, and what now seems unnecessary.
4. Record what changed after the work is complete.

This keeps the plan dynamic instead of locking the project into an outdated task list.

## Status Snapshot

### Done

- Added a cross-platform shell execution wrapper for one-shot commands.
- Added `terminal_session` module with a first-pass session manager.
- Implemented session create/write/read/list/status/kill at the module level.
- Exposed terminal session operations as tools in the main agent loop.
- Added per-session log persistence for terminal output.
- Added terminal session metadata persistence so historical sessions can be listed and inspected after restart.
- Added explicit live vs restored session semantics and read-only signaling.
- Moved builtin tool wiring out of `main.rs` into a dedicated tool module.
- Moved task/worktree tool wiring and manager implementations out of `main.rs` into a dedicated project tool module.
- Replaced per-call project manager construction with a shared `ProjectContext` created once at startup.
- Moved activity/progress rendering and heartbeat logic out of `main.rs` into a dedicated runtime module.
- Activated the `skills` module with minimal CLI entrypoints and test coverage.

### Current Limitations

- The terminal session backend uses child process pipes, not PTY/ConPTY yet.
- Interactive programs that require a real terminal may not behave correctly.
- No built-in external terminal window launch flow yet.
- Live session processes are still process-local and in-memory; restart only restores metadata and log access, not process control.

### Next Likely Steps

- Add better session lifecycle tests through `handle_tool_call`.
- Decide when to upgrade the backend from pipes to PTY/ConPTY.
- Add session log persistence.
- Add optional session window presentation as a separate layer.

## Session Notes

### 2026-03-06

Review:

- The project goal shifted from generic tool compatibility to a stronger command-line interaction model.
- The correct abstraction is a terminal session layer, not a platform-specific terminal window wrapper.

Changes made:

- Implemented the first terminal session manager in [src/terminal_session.rs](/d:/code/rustpilot/rustpilot/src/terminal_session.rs).
- Exported the module from [src/lib.rs](/d:/code/rustpilot/rustpilot/src/lib.rs).
- Added `terminal_create`, `terminal_write`, `terminal_read`, `terminal_list`, `terminal_status`, and `terminal_kill` to [src/main.rs](/d:/code/rustpilot/rustpilot/src/main.rs).
- Added lifecycle coverage for the `terminal_*` tool path through `handle_tool_call`.
- Added a manager reset path so targeted tests can run without leaking session state across cases.
- Added per-session log files and verified both module-level and tool-level output persistence.
- Added disk-backed session index loading so restarted managers can list historical sessions, read old logs, and continue session IDs safely.
- Added error-path coverage for exited, restored, and unknown session writes.
- Added explicit `source` and `read_only` fields so callers can distinguish controllable sessions from historical records.
- Extracted `bash`, file tools, and `terminal_*` tool definitions/dispatch into [src/agent_tools.rs](/d:/code/rustpilot/rustpilot/src/agent_tools.rs), reducing `main.rs` coupling.
- Extracted `task_*` / `worktree_*` tool definitions, argument parsing, and manager implementations into [src/project_tools.rs](/d:/code/rustpilot/rustpilot/src/project_tools.rs).
- Added shared project state in [src/project_tools.rs](/d:/code/rustpilot/rustpilot/src/project_tools.rs) so CLI commands and tool dispatch reuse the same managers instead of recreating them per call.
- Extracted activity state, rendering, and wait heartbeat into [src/activity.rs](/d:/code/rustpilot/rustpilot/src/activity.rs).
- Added `/skills` and `/skill <name>` commands in [src/main.rs](/d:/code/rustpilot/rustpilot/src/main.rs) and covered `SkillRegistry` loading in [src/skills.rs](/d:/code/rustpilot/rustpilot/src/skills.rs).

Adjustment:

- Keep the near-term focus on stable session management and tool integration.
- Defer real terminal emulation and new-window presentation until the current interaction model is proven useful.
- Prefer focused tests during development; temporary tests are acceptable, but stable regression coverage should be kept when it protects useful behavior.
- The next useful step is likely session history filtering or better output/event modeling, not more raw lifecycle plumbing.
- `main.rs` is now much closer to an orchestration entrypoint; the next coupling boundary is likely activity/progress rendering rather than tool plumbing.
- The next worthwhile runtime improvement is deciding whether project state should stay file-backed-only or gain a bounded cache for hot paths.
- The lingering `skills` warning is resolved; the next cleanup target is now functional rather than structural.
- With runtime progress extracted, the next practical cleanup target is either the lingering `skills` warning or a bounded cache for task/worktree hot paths.
