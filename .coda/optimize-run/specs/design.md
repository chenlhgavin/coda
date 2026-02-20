

# Feature: optimize-run

## Overview

Transform the `coda run` TUI from a static single-line-per-phase status view into a split-panel layout with real-time streaming AI output. The left sidebar shows a compact phase list with status icons and elapsed timers. The right main area displays a scrollable, auto-following stream of the agent's text and tool activity (file writes, bash commands, reads) as they happen during each phase.

This gives developers immediate visibility into what the AI agent is doing — replacing the opaque "Running..." indicator with a live feed of reasoning, code generation, and tool invocations.

## High-Level Design

**Data flow:** Enable `include_partial_messages` on `ClaudeAgentOptions` so the SDK emits `StreamEvent` messages containing token-level text deltas alongside the existing complete `Message::Assistant` / `Message::User` messages. In `Runner::send_and_collect`, parse these events and forward them to the UI via two new `RunEvent` variants (`AgentTextDelta`, `ToolActivity`) through the existing `UnboundedSender<RunEvent>` channel. The full response collection, logging, and metrics remain unchanged.

**UI layout:** Replace the full-width "Phases" block with a horizontal split. The left sidebar (~20 columns) renders a compact phase list. The right panel renders a scrollable `Paragraph` populated by streaming events. The buffer clears on each phase transition.

```
┌─── CODA Run: feature-name ─── [Running] ────────────────────────┐
│ ○ setup → ⠋ implement → ○ review → ○ verify → ○ PR             │
├───────────────┬──────────────────────────────────────────────────┤
│ Phases        │ implement                                        │
│               │                                                  │
│ [●] setup     │ Analyzing the design spec...                     │
│     45s       │ I'll start by creating the data types.           │
│ [⠋] implement │ ┄ Write src/types.rs                             │
│     2m 10s    │ ┄ Bash cargo build                               │
│ [○] review    │ Build successful. Now implementing the handler.. │
│ [○] verify    │ ┄ Write src/handler.rs                           │
│               │ Now I'll add the HTTP endpoint...                │
│               │ █                                                │
├───────────────┴──────────────────────────────────────────────────┤
│ Elapsed: 2m 55s   Turns: 8   Cost: $0.4521 USD                  │
├──────────────────────────────────────────────────────────────────┤
│ [Ctrl+C] Cancel  [↑↓] Scroll  [End] Auto-scroll                 │
└──────────────────────────────────────────────────────────────────┘
```

**Scroll behavior:**
- New content auto-scrolls to the bottom by default (`auto_scroll = true`).
- `Up` / `PageUp` scrolls up and disables auto-scroll.
- `Down` / `PageDown` scrolls down; reaching the bottom re-enables auto-scroll.
- `End` jumps to the bottom and re-enables auto-scroll.
- Phase transitions clear the buffer, reset scroll offset, and re-enable auto-scroll.

## Interface Design

### New `RunEvent` variants (`crates/coda-core/src/runner.rs`)

```rust
pub enum RunEvent {
    // ... existing variants unchanged ...

    /// Incremental text delta from the assistant (token-level streaming).
    AgentTextDelta {
        /// The text fragment to append to the streaming buffer.
        text: String,
    },

    /// A tool invocation observed during agent execution.
    ToolActivity {
        /// Tool name (e.g., "Bash", "Write", "Read", "Glob", "Grep").
        tool_name: String,
        /// Brief summary of the tool input (file path, command, pattern, etc.).
        summary: String,
    },
}
```

### `ClaudeAgentOptions` change (`crates/coda-core/src/runner.rs`)

In `Runner::new`, when constructing `ClaudeAgentOptions` via `AgentProfile::Coder.to_options(...)`, set `include_partial_messages: true` on the resulting options struct.

### `StreamEvent` text delta parser (inline in `runner.rs`)

```rust
/// Extracts text delta content from a raw `StreamEvent.event` JSON value.
///
/// Parses `content_block_delta` events with a `text_delta` delta type.
/// Returns `None` for all other event types.
fn extract_text_delta(event: &serde_json::Value) -> Option<&str> {
    if event.get("type")?.as_str()? == "content_block_delta" {
        let delta = event.get("delta")?;
        if delta.get("type")?.as_str()? == "text_delta" {
            return delta.get("text")?.as_str();
        }
    }
    None
}
```

### Tool input summarizer (inline in `runner.rs`)

```rust
/// Extracts a brief summary from a tool use input for UI display.
///
/// Returns a human-readable string like the file path for Write/Read,
/// the command for Bash, or the pattern for Grep/Glob.
fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => input.get("command")
            .and_then(|v| v.as_str())
            .map(|s| truncate_str(s, 60))
            .unwrap_or_default(),
        "Write" | "Read" | "Edit" => input.get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Grep" => input.get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| truncate_str(s, 60))
            .unwrap_or_default(),
        "Glob" => input.get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}
```

## Directory Structure

| Path | Change |
|------|--------|
| `crates/coda-core/src/runner.rs` | Add `AgentTextDelta` and `ToolActivity` to `RunEvent`; enable `include_partial_messages`; emit streaming events from `send_and_collect`; add `extract_text_delta` and `summarize_tool_input` helpers |
| `apps/coda-cli/src/run_ui.rs` | Rework layout to split panel; add content buffer + scroll state; handle new events; add keyboard scroll handling; update help bar |

No new files or crates.

## Core Data Structures

### Extended `RunUi` state (`run_ui.rs`)

```rust
pub struct RunUi {
    // ... existing fields unchanged ...

    /// Accumulated styled lines for the streaming content area.
    content_lines: Vec<Line<'static>>,
    /// Vertical scroll offset (number of lines scrolled from top).
    scroll_offset: u16,
    /// Whether auto-scroll is active (follows new content).
    auto_scroll: bool,
    /// Height of the content area (updated each draw for scroll math).
    content_visible_height: u16,
}
```

### Layout constants (`run_ui.rs`)

```rust
/// Width of the left sidebar showing phase status.
const SIDEBAR_WIDTH: u16 = 22;
```

## Development Phases

### Phase 1: Extend `RunEvent` and emit streaming events from Runner

- **Goal**: Forward real-time text deltas and tool activity from the agent stream to the UI channel without changing existing behavior.
- **Tasks**:
  - Add `AgentTextDelta { text: String }` and `ToolActivity { tool_name: String, summary: String }` variants to the `RunEvent` enum.
  - Add `extract_text_delta` and `summarize_tool_input` helper functions in `runner.rs`.
  - In `Runner::new`, set `include_partial_messages: true` on `ClaudeAgentOptions`.
  - In `send_and_collect`, extend the stream processing loop:
    - On `Message::StreamEvent(event)`: call `extract_text_delta(&event.event)` and if `Some(text)`, emit `RunEvent::AgentTextDelta { text }`.
    - On `Message::Assistant` with `ContentBlock::ToolUse(tu)`: emit `RunEvent::ToolActivity { tool_name: tu.name, summary: summarize_tool_input(&tu.name, &tu.input) }`.
  - Verify existing response collection (`resp.text`, `resp.tool_output`, `resp.result`), logging, and metrics are unaffected.
  - Update the catch-all `_ => {}` in `RunUi::handle_event` so it doesn't silently ignore the new variants (leave them as no-ops for now — Phase 3 will handle them).
- **Commit message**: `feat(run): emit streaming text deltas and tool activity events from runner`

### Phase 2: Rework `RunUi` layout to split panel

- **Goal**: Replace the full-width phases block with a sidebar + content area layout.
- **Tasks**:
  - Add `content_lines`, `scroll_offset`, `auto_scroll`, `content_visible_height` fields to `RunUi`, initialized to empty/default/true/0 in `RunUi::new`.
  - Add `SIDEBAR_WIDTH` constant.
  - In `draw`, change the middle layout constraint from a single `Constraint::Length(phase_list_height)` to a `Constraint::Min(8)` (flexible height), then split it horizontally with `Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])`.
  - Create `render_sidebar` function: compact phase list in the left area — one line per phase with icon, truncated name, and elapsed time. Include PR status as a final entry.
  - Create `render_content` function: render `content_lines` as a `Paragraph` with `Wrap { trim: false }` and `.scroll((scroll_offset, 0))`. Use the current phase name as the block title. Record visible height in `content_visible_height`.
  - Replace the `render_phase_list` call with `render_sidebar` + `render_content`.
  - Keep `render_pipeline`, `render_header`, `render_summary`, and `render_help_bar` unchanged (pipeline bar still shows the horizontal compact view).
- **Commit message**: `feat(run-ui): split layout into phases sidebar and content area`

### Phase 3: Implement streaming buffer and scroll behavior

- **Goal**: Populate the content area with real-time streaming text and enable scrolling.
- **Tasks**:
  - In `handle_event`, handle new variants:
    - `RunEvent::AgentTextDelta { text }`: Split by `\n`. Append text fragments to `content_lines` — if the last line is an incomplete text line, append to it; on `\n`, start a new `Line`. Style as `Color::White`. If `auto_scroll`, update `scroll_offset` to pin to bottom.
    - `RunEvent::ToolActivity { tool_name, summary }`: Append a new `Line` styled with `Color::DarkGray` in the format `┄ {tool_name} {summary}`. If `auto_scroll`, update `scroll_offset`.
    - `RunEvent::PhaseStarting { .. }`: After existing logic, clear `content_lines`, reset `scroll_offset = 0`, set `auto_scroll = true`.
  - Compute max scroll offset as `content_lines.len().saturating_sub(content_visible_height as usize) as u16`.
  - In the keyboard polling loop, add handlers:
    - `Up`: `scroll_offset = scroll_offset.saturating_sub(1); auto_scroll = false;`
    - `Down`: increment `scroll_offset` (clamped to max); if at max, set `auto_scroll = true`.
    - `PageUp`: subtract `content_visible_height` from `scroll_offset`; `auto_scroll = false`.
    - `PageDown`: add `content_visible_height` to `scroll_offset` (clamped); if at max, `auto_scroll = true`.
    - `End`: set `scroll_offset` to max; `auto_scroll = true`.
  - Update `render_help_bar`: when a phase is running, show `[Ctrl+C] Cancel  [↑↓] Scroll  [End] Auto-scroll`.
- **Commit message**: `feat(run-ui): streaming text buffer with auto-scroll and keyboard navigation`

### Phase 4: Polish and edge cases

- **Goal**: Handle edge cases, visual polish, and defensive parsing.
- **Tasks**:
  - In `extract_text_delta`, handle unknown/unexpected event types gracefully (return `None`).
  - Ensure `StreamEvent` parsing does not panic on malformed JSON — rely on the chained `Option` approach.
  - Truncate phase names in the sidebar to `SIDEBAR_WIDTH - 8` chars with `…` suffix.
  - When terminal width is very small (< `SIDEBAR_WIDTH + 20`), fall back to content-only view (hide sidebar) to prevent layout breakage.
  - Coalesce consecutive `AgentTextDelta` events within a single `try_recv` drain loop into a single buffer append to reduce per-tick processing.
  - Run `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`.
- **Commit message**: `fix(run-ui): polish streaming display edge cases and defensive parsing`

## Dependencies

None. All required functionality (`Paragraph::scroll`, `Wrap`, `Layout::horizontal`, `serde_json` value access) is available in existing dependencies.

## Risk & Trade-offs

| Risk | Severity | Mitigation |
|------|----------|------------|
| **`StreamEvent.event` JSON format is undocumented in the Rust SDK** | Medium | Parse defensively with chained `Option` — unknown event types return `None` and are silently skipped. If `include_partial_messages` yields no useful deltas, the UI still works (content area stays empty until complete `Message::Assistant` text arrives at turn boundaries, which we can fall back to emitting). |
| **High-frequency delta events may increase channel throughput** | Low | The `UnboundedReceiver` + `try_recv` drain loop already handles bursts. Coalescing consecutive text deltas in Phase 4 reduces per-tick overhead. The 100ms tick interval provides natural batching. |
| **Sidebar too narrow for long phase names** | Low | Truncate with `…` at `SIDEBAR_WIDTH - 8` characters. Full phase name shown in the content area title block. |
| **Small terminal sizes** | Low | Fall back to content-only view when terminal width < `SIDEBAR_WIDTH + 20`. The sidebar minimum height is handled by `Constraint::Min(8)`. |
| **Content buffer memory for extremely long phases** | Low | A typical phase produces hundreds to low thousands of lines. No cap needed initially; can add a 10K-line ring buffer if profiling shows concern. |
| **`include_partial_messages` may affect runner behavior** | Low | This flag only adds extra `StreamEvent` messages to the stream — it does not change the semantics of `Message::Assistant`, `Message::User`, or `Message::Result`. Existing collection logic is untouched. |