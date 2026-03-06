# Terminal Backend Evaluation

## Review

Current goal remains unchanged: keep `TerminalManager` as the stable session API while upgrading the live backend from plain child-process pipes to a real terminal backend.

The current implementation in `src/terminal_session.rs` is explicitly pipe-based:
- `SessionEntry` stores `Child` and `ChildStdin` directly.
- `create()` spawns a shell with `stdin/stdout/stderr` piped.
- `spawn_reader()` starts one reader thread per stream and merges stdout/stderr into a shared byte buffer.
- `write()` writes bytes directly into `ChildStdin`.
- `refresh_state()` uses `Child::try_wait()`.

This is good enough for line-oriented shells, but it is not a real terminal. Programs that check `isatty` / terminal capabilities, use cursor movement, or require screen size negotiation will continue to behave incorrectly until the backend changes.

## What PTY/ConPTY Changes

A PTY backend changes the I/O model in three important ways:
- Output becomes a single terminal byte stream from the PTY master side, not separate stdout/stderr pipes.
- Input is written to the PTY master writer, not to `ChildStdin`.
- Terminal size becomes a real concern, so resize support should exist even if the first caller does not use it yet.

On Windows, the equivalent is ConPTY. On Unix, it is a PTY. The cleanest migration path is to use one cross-platform crate for the live backend rather than keep separate native integrations inside `rustpilot`.

## Current Coupling Points

These parts of `src/terminal_session.rs` are the main migration points:
- `SessionEntry`: currently hard-codes `Child` and `ChildStdin`.
- `TerminalManager::create()`: currently assumes `Command` + `Stdio::piped()`.
- `TerminalManager::write()`: currently requires a concrete `ChildStdin`.
- `refresh_state()`: currently requires `Child::try_wait()`.
- `spawn_reader()`: currently expects `Read` streams for stdout/stderr separately.
- `default_shell()` / `shell_command()`: currently build process startup directly in this module.

The good news is that the public manager API is already narrow. `create/write/read/list/status/kill/reset/clear_live_sessions` can stay stable while the live backend is swapped underneath.

## Recommended Refactor Shape

Do not replace the whole module in one step. First introduce an internal backend abstraction.

Suggested internal shape:

```rust
trait LiveTerminalBackend: Send {
    fn write(&mut self, input: &[u8]) -> anyhow::Result<()>;
    fn try_wait(&mut self) -> anyhow::Result<Option<i32>>;
    fn kill(&mut self) -> anyhow::Result<()>;
    fn resize(&mut self, cols: u16, rows: u16) -> anyhow::Result<()>;
}
```

Then change `SessionEntry` from concrete process fields to something like:

```rust
struct SessionEntry {
    id: String,
    shell: String,
    cwd: PathBuf,
    log_path: PathBuf,
    created_at: u64,
    backend: Box<dyn LiveTerminalBackend>,
    output: Arc<Mutex<Vec<u8>>>,
    state: SessionState,
}
```

That keeps all existing persistence and session metadata logic intact.

## Migration Stages

### Stage 1: Internal Backend Extraction

Goal: no behavior change yet.

- Keep the current pipe implementation.
- Wrap it in a `PipeBackend` that implements the new backend trait.
- Move process startup and lifecycle handling behind backend constructors.
- Keep `TerminalManager` public API unchanged.

This is the safest first change because it isolates the swap point before introducing PTY complexity.

### Stage 2: PTY Backend Introduction

Goal: add a real terminal backend without changing tool APIs yet.

- Add a PTY/ConPTY-backed `PtyBackend`.
- Use one reader thread on the PTY master stream instead of separate stdout/stderr threads.
- Continue writing raw bytes into the shared output buffer and log file.
- Keep restored-session behavior exactly as it is now.

At this point, `terminal_create` can either:
- always choose PTY when available, or
- choose PTY behind an opt-in flag during rollout.

### Stage 3: Capability Expansion

Goal: expose the parts that matter only once PTY exists.

- Add `terminal_resize`.
- Optionally record initial `cols` / `rows` in session metadata.
- Consider adding a capability field to `TerminalSessionInfo`, such as `backend = "pipe" | "pty"`.

### Stage 4: Behavior Cleanup

Once PTY is stable:
- Revisit whether `stdout/stderr` distinction is still relevant. With PTY it usually is not.
- Re-evaluate output decoding. Full-screen apps will emit ANSI control sequences, so plain text reads may still be noisy even though the app works.

## Key Compatibility Notes

### Logging and Persistence

Current log persistence should survive the migration unchanged. The output source changes, but the sink does not. `append_log_chunk()` and the `output: Arc<Mutex<Vec<u8>>>` design can stay.

### Read Semantics

`read(session_id, from)` can remain byte-offset based. That is good because PTY output is naturally a terminal byte stream. The existing API is already compatible with this.

### State Semantics

`SessionState`, `SessionSource`, restored-session indexing, and `read_only` semantics do not need redesign. They are independent from whether the live backend is pipe or PTY.

### Tests

The current tests in `tests/app_integration.rs` remain useful, but PTY rollout should add specific coverage for:
- interactive prompt presence
- multi-command shell continuity
- resize no-op / resize success
- programs that fail under pipe but succeed under PTY

## Risks

Main engineering risks:
- PTY output will include more control sequences than the current pipe output.
- Windows ConPTY process shutdown behavior can differ from `Child::kill()` assumptions.
- Some PTY crates expose lifecycle handles differently from `std::process::Child`, so the backend abstraction must own that difference.
- If PTY rollout happens before backend extraction, the diff will be larger than necessary and harder to test.

## Recommendation

Recommended next implementation order:
1. Extract a backend trait and `PipeBackend` first.
2. Keep all current tool and persistence APIs unchanged.
3. Add PTY/ConPTY as a second backend behind that trait.
4. Only after that, add `terminal_resize` and optional backend metadata.

This keeps the migration incremental and makes it much easier to compare PTY behavior against the current baseline.
