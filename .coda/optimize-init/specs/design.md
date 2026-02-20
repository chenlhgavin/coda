# Feature: optimize-init

## Overview

`coda init` currently blocks for 1-2 minutes with zero visual feedback after printing a single line. The underlying `engine.init()` makes two sequential blocking `query()` calls to the Claude Agent SDK — first to analyze the repository structure, then to set up the project files. During this time users have no way to know whether the process is working or hung.

This feature replaces the silent blocking flow with a Ratatui TUI that provides real-time feedback: a two-phase pipeline indicator, live elapsed timers, cost tracking, and — most importantly — a scrollable output panel that streams the AI's text as it's generated. The engine is refactored to use `query_stream()` instead of `query()`, enabling token-level streaming of AI output through an `InitEvent` channel.

## High-Level Design

```
┌─────────────────────────────────────────────────────────┐
│                    App::init()                          │
│  Creates channel, spawns InitUi + Engine concurrently   │
│  via tokio::select! (same pattern as App::run_tui)      │
└────────────┬───────────────────────┬────────────────────┘
             │                       │
   ┌─────────▼─────────┐   ┌────────▼────────────────────┐
   │     InitUi         │   │     Engine::init()           │
   │  (Ratatui TUI)     │   │  query_stream() × 2         │
   │                    │   │                              │
   │  Consumes          │◄──│  Sends InitEvent via         │
   │  InitEvent from    │   │  UnboundedSender             │
   │  mpsc channel      │   │                              │
   └────────────────────┘   └──────────────────────────────┘
```

**Data flow:**

1. `App::init()` creates an `UnboundedSender<InitEvent>` / `UnboundedReceiver<InitEvent>` pair
2. `Engine::init(Some(tx))` drives two streaming agent calls sequentially. For each:
   - Sends `PhaseStarting` before the call
   - Iterates over the `query_stream()` stream, sending `StreamText` for each assistant text chunk
   - Sends `PhaseCompleted` or `PhaseFailed` after the call
3. `InitUi::run(rx)` consumes events in a non-blocking loop (100ms tick), updating the TUI each frame
4. `App::init()` uses `tokio::select!` with biased engine branch (identical to existing `run_tui` pattern)

**TUI layout:**

```
┌──────────────────────────────────────────────────┐
│ CODA Init: /path/to/project          [Running]   │  Header
├──────────────────────────────────────────────────┤
│ Pipeline: ● analyze-repo → ⠋ setup-project       │  Pipeline bar
├──────────────────────────────────────────────────┤
│  [●] analyze-repo        32s   $0.0123           │  Phase list
│  [⠋] setup-project       12s   Running...        │
├──────────────────────────────────────────────────┤
│ ┌ Output ───────────────────────────────────────┐│
│ │ project_type: "cli-tool"                      ││  AI streaming
│ │ tech_stack:                                   ││  output panel
│ │   languages:                                  ││  (auto-scroll)
│ │     - Rust                                    ││
│ │   frameworks:                                 ││
│ │     - Tokio                                   ││
│ └───────────────────────────────────────────────┘│
├──────────────────────────────────────────────────┤
│  Elapsed: 44s   Cost: $0.0123 USD                │  Summary bar
├──────────────────────────────────────────────────┤
│ [Ctrl+C] Cancel                                  │  Help bar
└──────────────────────────────────────────────────┘
```

The output panel is the key differentiator from `RunUi`. It maintains a rolling buffer of the last 200 lines of AI-generated text, automatically scrolling to the bottom as new text arrives. This gives users concrete evidence that the AI is actively working.

## Interface Design

**New event type in `coda-core`:**

```rust
/// Progress events emitted during `coda init` for real-time UI updates.
#[derive(Debug, Clone)]
pub enum InitEvent {
    /// Init pipeline starting with the ordered phase names.
    InitStarting { phases: Vec<String> },

    /// A phase is about to start executing.
    PhaseStarting {
        name: String,
        index: usize,
        total: usize,
    },

    /// A chunk of streaming text from the AI agent.
    StreamText { text: String },

    /// A phase completed successfully.
    PhaseCompleted {
        name: String,
        index: usize,
        duration: Duration,
        cost_usd: f64,
    },

    /// A phase failed.
    PhaseFailed {
        name: String,
        index: usize,
        error: String,
    },

    /// The entire init flow has finished.
    InitFinished { success: bool },
}
```

**Modified `Engine::init` signature:**

```rust
impl Engine {
    pub async fn init(
        &self,
        progress_tx: Option<UnboundedSender<InitEvent>>,
    ) -> Result<(), CoreError>;
}
```

When `progress_tx` is `None`, the function behaves identically to the current implementation (useful for testing or future CI use). When `Some`, it sends events as work progresses.

**New `InitUi` struct in `coda-cli`:**

```rust
pub struct InitUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    project_root: String,
    phases: Vec<PhaseDisplay>,      // reuse PhaseDisplay from run_ui
    active_phase: Option<usize>,
    start_time: Instant,
    total_cost: f64,
    output_lines: Vec<String>,      // rolling buffer, max 200 lines
    finished: bool,
    success: bool,
    spinner_tick: usize,
}

impl InitUi {
    pub fn new(project_root: &str) -> Result<Self>;
    pub async fn run(&mut self, rx: UnboundedReceiver<InitEvent>) -> Result<InitSummary>;
}

pub struct InitSummary {
    pub elapsed: Duration,
    pub total_cost: f64,
    pub success: bool,
}
```

## Directory Structure

| Action | Path | Purpose |
|--------|------|---------|
| **New** | `apps/coda-cli/src/init_ui.rs` | Ratatui TUI for init progress display |
| Modify | `crates/coda-core/src/engine.rs` | Refactor `init()` to streaming + `InitEvent` channel |
| Modify | `crates/coda-core/src/lib.rs` | Export `InitEvent` |
| Modify | `apps/coda-cli/src/app.rs` | Wire `InitUi` into `App::init()` with `tokio::select!` |
| Modify | `apps/coda-cli/src/main.rs` | Register `init_ui` module |

## Core Data Structures

**`InitEvent`** (described in Interface Design above): The event enum carrying all progress signals from engine to UI.

**`PhaseDisplay`**: Reuse the existing struct pattern from `run_ui.rs` — `name`, `status` (enum with Pending/Running/Completed/Failed variants), `started_at`, `detail`.

**Output buffer**: A `Vec<String>` capped at 200 lines. When `StreamText` arrives, the text is split by newlines and appended. If the buffer exceeds 200 lines, the oldest lines are dropped (`drain(0..overflow)`). The TUI renders the tail of the buffer that fits in the available terminal area.

## Development Phases

### Phase 1: InitEvent + Streaming Engine

- **Goal**: Refactor `Engine::init()` to use `query_stream()` and emit `InitEvent`s through an optional channel
- **Tasks**:
  - Define `InitEvent` enum in `crates/coda-core/src/engine.rs` (co-located with `Engine`, as it's tightly coupled to the init flow — unlike `RunEvent` which is in `runner.rs` because it's emitted by `Runner`)
  - Modify `Engine::init()` signature to accept `Option<UnboundedSender<InitEvent>>`
  - Add a small helper `fn emit(tx: &Option<UnboundedSender<InitEvent>>, event: InitEvent)` to avoid verbose `if let Some(tx) = &progress_tx { let _ = tx.send(...); }` at every callsite
  - Replace the first `query()` call (analyze_repo) with `query_stream()`: iterate the stream, collect `Message::Assistant` text blocks into `analysis_result` while simultaneously sending `StreamText` events, extract the final `ResultMessage` for cost info
  - Replace the second `query()` call (setup_project) with the same streaming pattern
  - Send `InitStarting`, `PhaseStarting`, `PhaseCompleted`/`PhaseFailed`, and `InitFinished` events at the appropriate points
  - Update `App::init()` to pass `None` temporarily (preserves current behavior, ensures compilation)
  - Ensure `cargo build`, `cargo test`, `cargo clippy -- -D warnings` all pass
- **Commit message**: `"feat(init): stream init agent calls and emit InitEvent progress"`

### Phase 2: InitUi TUI

- **Goal**: Implement the Ratatui TUI that consumes `InitEvent`s and renders the progress display
- **Tasks**:
  - Create `apps/coda-cli/src/init_ui.rs` with `InitUi` struct
  - Implement `InitUi::new()` — enter alternate screen, raw mode (mirror `RunUi::new` pattern)
  - Implement `InitUi::run()` — event loop with 100ms tick, non-blocking `try_recv()`, keyboard polling, same async-friendly pattern as `RunUi::run`
  - Implement `handle_event()` — map `InitEvent` variants to internal state updates; `StreamText` appends to output buffer with line splitting and 200-line cap
  - Implement `draw()` with layout: Header (1 line) → Pipeline bar (3 lines) → Phase list (5 lines, 2 phases + borders) → Output panel (flexible, fills remaining space) → Summary bar (3 lines) → Help bar (1 line)
  - Render functions: `render_header`, `render_pipeline`, `render_phase_list` (reuse patterns from `run_ui.rs`), `render_output_panel` (new — wraps text in a bordered block, shows tail of output buffer), `render_summary`, `render_help_bar`
  - Implement `Drop` for terminal cleanup (disable raw mode, leave alternate screen)
  - Register module in `apps/coda-cli/src/main.rs`
  - Ensure `cargo build`, `cargo clippy -- -D warnings` pass
- **Commit message**: `"feat(init): add Ratatui TUI for init progress display"`

### Phase 3: Wire App Layer

- **Goal**: Connect `InitUi` and streaming `Engine::init()` in `App::init()`
- **Tasks**:
  - Modify `App::init()` to create `UnboundedSender`/`UnboundedReceiver` pair
  - Construct `InitUi::new()` with project root display string
  - Use `tokio::select!` with biased engine branch to run `engine.init(Some(tx))` and `ui.run(rx)` concurrently (copy the exact pattern from `App::run_tui`)
  - After both complete, drop UI to restore terminal, then print the final summary to stdout (same pattern as `run_tui` post-processing)
  - Handle error cases: engine failure, UI cancellation (Ctrl+C)
  - Run full test suite: `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`
- **Commit message**: `"feat(init): wire InitUi with streaming engine for interactive progress"`

## Dependencies

No new external dependencies required. All necessary crates are already in the workspace:

- `ratatui`, `crossterm` — TUI rendering (used by `run_ui.rs`)
- `tokio::sync::mpsc` — channel for `InitEvent` (used by `runner.rs`)
- `futures::StreamExt` — stream iteration for `query_stream()` (used by `planner.rs`)
- `claude_agent_sdk_rs::query_stream` — already exported by the vendored SDK

## Risk & Trade-offs

| Risk | Impact | Mitigation |
|------|--------|------------|
| `query_stream()` behavioral differences from `query()` | Medium — different code path in SDK | `query_stream()` uses the same `SubprocessTransport` underneath; the only difference is it doesn't buffer all messages in memory. Message types (`Message::Assistant`, `Message::Result`) are identical. Verify by running init against a real repo. |
| AI output panel text volume | Low — setup phase generates tool-use output which can be verbose | Cap output buffer at 200 lines, drop oldest lines. Only extract text from `Message::Assistant` content blocks (skip tool-use details). |
| `Engine::init()` signature is a breaking change | Low — only called from `App::init()` | `Option<>` wrapper means passing `None` preserves existing behavior. No external consumers of this API. |
| TUI may feel heavy for a 2-phase pipeline | Low — cosmetic concern | The output panel is the primary value — users see AI text streaming in real-time, giving confidence the system is working. The pipeline bar is secondary but provides at-a-glance status. |
| Terminal cleanup on panic/crash | Medium — raw mode left on could corrupt terminal | `Drop` impl on `InitUi` handles cleanup. Same approach used by `RunUi` which has been stable in production. |