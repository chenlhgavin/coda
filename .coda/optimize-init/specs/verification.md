

# Verification Plan: optimize-init

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] All new tests pass
- [ ] No regression in existing tests (`cargo test` full suite)
- [ ] No new warnings introduced

## Functional Verification

### Core Streaming Flow

- [ ] Scenario 1: Fresh init on an uninitialized repository
  - Input: Run `coda init` in a repository that has no `.coda/` directory
  - Expected: TUI launches in alternate screen showing two-phase pipeline (analyze-repo → setup-project). AI text streams into the output panel in real-time. Both phases complete. TUI exits cleanly. Final summary printed to stdout. `.coda/`, `.trees/`, `.coda/config.yml`, `.coda.md` are all created correctly.

- [ ] Scenario 2: Init on an already-initialized repository
  - Input: Run `coda init` in a repository that already has `.coda/` directory
  - Expected: Fails with the existing "Project already initialized" error. TUI should either not launch (error caught before TUI setup) or display the error clearly and exit cleanly.

- [ ] Scenario 3: Streaming text appears in output panel during analyze-repo phase
  - Input: Run `coda init` and observe the output panel while the first phase runs
  - Expected: Text chunks appear incrementally (not all at once after completion). The panel auto-scrolls as new lines arrive. The text corresponds to the YAML analysis output (project_type, tech_stack, etc.).

- [ ] Scenario 4: Streaming text appears during setup-project phase
  - Input: Continue observing after analyze-repo completes
  - Expected: Output panel clears or continues (implementation detail) and shows AI text from the setup phase. Tool-use activity (file creation, directory creation) is visible through the streamed text.

- [ ] Scenario 5: Phase transitions display correctly
  - Input: Watch the pipeline bar and phase list during a full init run
  - Expected: analyze-repo shows spinner while running, then shows ● with duration and cost when complete. setup-project transitions from ○ (pending) to spinner (running) to ● (completed). Pipeline bar reflects the same transitions.

### TUI Display

- [ ] Scenario 6: Summary bar shows live elapsed time
  - Input: Observe the summary bar during init
  - Expected: Elapsed time updates every ~100ms (visible second counter ticking). Cost updates when each phase completes.

- [ ] Scenario 7: Spinner animation is visible
  - Input: Observe the active phase indicator during a running phase
  - Expected: Spinner character cycles through braille animation frames (⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏) smoothly.

- [ ] Scenario 8: Terminal is restored after successful completion
  - Input: Let init complete successfully
  - Expected: Alternate screen is exited, raw mode is disabled, cursor is visible, normal terminal behavior resumes. Final summary is printed to stdout with success message.

## Edge Cases

- [ ] User presses Ctrl+C during analyze-repo phase: TUI exits cleanly, terminal is restored, no partial `.coda/` directory left behind (init hadn't reached file creation yet)
- [ ] User presses Ctrl+C during setup-project phase: TUI exits cleanly, terminal is restored. Partial files may exist (this is expected — same behavior as current implementation if process is killed)
- [ ] Agent SDK connection failure (e.g., no API key, network error): Error is surfaced clearly. TUI shows the failed phase with error message, then exits. Terminal is restored.
- [ ] Very small terminal window (e.g., 40x10): TUI does not panic. Layout may be cramped but should not crash. Output panel may show only a few lines.
- [ ] Output buffer exceeds 200 lines: Oldest lines are dropped. No memory growth. Most recent text is always visible at the bottom of the output panel.
- [ ] `StreamText` event contains multi-line text (single chunk with embedded newlines): Lines are split correctly and each line appears as a separate entry in the output panel.
- [ ] `StreamText` event contains empty string: No crash, no empty line added to output buffer.
- [ ] Engine completes but UI hasn't drained all events yet: All events are processed before UI exits (channel fully drained on disconnect).
- [ ] `progress_tx` is `None`: `Engine::init(None)` behaves identically to the old blocking implementation — no panics, no channel-related errors, same functional outcome.

## Integration Points

- [ ] `Engine::init(None)` produces the exact same side effects as the previous `query()`-based implementation: same files created, same config.yml content, same `.coda.md` content
- [ ] `query_stream()` returns the same `Message` types (`Message::Assistant`, `Message::Result`) as `query()` — the `analysis_result` text collected from streaming is equivalent to what `extract_text_from_messages()` previously returned
- [ ] `App::init()` uses the same `tokio::select!` biased pattern as `App::run_tui()` — engine result is always captured before UI branch can break the loop
- [ ] `InitUi` follows the same lifecycle as `RunUi`: `new()` → alternate screen + raw mode → `run()` → `Drop` restores terminal
- [ ] `InitEvent` is properly exported from `coda-core` via `lib.rs` and importable from `coda-cli`
- [ ] Existing `coda run`, `coda plan`, `coda list`, `coda status`, `coda clean` commands are completely unaffected

## Performance

- [ ] TUI tick rate (~100ms) does not cause noticeable CPU usage when idle (no busy-spinning)
- [ ] Output buffer cap at 200 lines prevents unbounded memory growth during verbose AI output
- [ ] Streaming does not add measurable latency compared to the previous blocking `query()` — total init wall-clock time should be comparable or slightly faster (first text appears sooner)
- [ ] Channel send operations (`tx.send()`) are non-blocking and do not slow down the engine's stream consumption

## Security

- [ ] No sensitive data (API keys, tokens) is displayed in the TUI output panel — only AI-generated text content
- [ ] Terminal raw mode is always cleaned up, even on panic (verified by `Drop` impl) — a stuck raw mode terminal is a usability/security concern as it can mask input