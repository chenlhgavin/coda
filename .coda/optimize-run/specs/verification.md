

# Verification Plan: optimize-run

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] All new tests pass
- [ ] No regression in existing tests (`cargo test`)
- [ ] Code coverage is maintained or improved

## Functional Verification

### Data Flow — Runner Streaming Events

- [ ] Scenario 1: Text deltas are emitted from `StreamEvent` messages
  - Input: Run a feature with a planned dev phase so the agent generates text
  - Expected: `RunEvent::AgentTextDelta` events appear in the channel with non-empty text fragments that, when concatenated, match the full assistant text collected in the response

- [ ] Scenario 2: Tool activity events are emitted for known tools
  - Input: Run a feature where the agent invokes Bash, Write, Read, Grep, and Glob tools
  - Expected: Each tool invocation emits a `RunEvent::ToolActivity` with the correct `tool_name` and a meaningful `summary` (command string for Bash, file path for Write/Read/Edit, pattern for Grep/Glob)

- [ ] Scenario 3: Unknown tools produce empty summary without error
  - Input: Agent invokes an unrecognized tool (e.g., a custom MCP tool)
  - Expected: `RunEvent::ToolActivity` is emitted with `tool_name` set correctly and `summary` as an empty string; no panic or error

- [ ] Scenario 4: Existing response collection is unaffected
  - Input: Run a complete feature with multiple phases
  - Expected: `AgentResponse.text`, `AgentResponse.tool_output`, and `AgentResponse.result` contain the same data as before the change. Run logger output matches previous format. Phase metrics (turns, cost, tokens) are identical.

### UI Layout — Split Panel

- [ ] Scenario 5: Split panel renders correctly at standard terminal sizes
  - Input: Run with terminal width ≥ 80 columns, height ≥ 24 rows
  - Expected: Left sidebar appears at ~22 columns showing all phases with status icons. Right content area fills remaining width with a bordered block titled with the current phase name.

- [ ] Scenario 6: Sidebar shows all phase statuses correctly
  - Input: Run through multiple phases (some completed, one running, some pending)
  - Expected: Completed phases show `●` icon with elapsed time. Running phase shows spinner animation with live elapsed timer. Pending phases show `○` in dimmed style. PR status appears as the last sidebar entry.

- [ ] Scenario 7: Content area title updates on phase transition
  - Input: Observe the content area as phases transition from one to the next
  - Expected: The block title changes to reflect the currently active phase name

### Streaming Buffer and Scroll

- [ ] Scenario 8: Streaming text appears in real-time
  - Input: Start a run and observe the content area during a dev phase
  - Expected: Text appears incrementally as the agent generates it, not in large chunks at turn boundaries. Tool activity lines (prefixed with `┄`) are interspersed with assistant text.

- [ ] Scenario 9: Auto-scroll follows new content
  - Input: Let streaming text exceed the visible content area height without pressing any keys
  - Expected: The view automatically scrolls to show the latest text at the bottom

- [ ] Scenario 10: Manual scroll-up disables auto-scroll
  - Input: Press `Up` or `PageUp` while text is streaming
  - Expected: The view scrolls up and stays at the user-selected position even as new text arrives. New content continues to be buffered but the viewport does not jump.

- [ ] Scenario 11: Scrolling to bottom re-enables auto-scroll
  - Input: After scrolling up, press `Down` repeatedly until reaching the bottom (or press `End`)
  - Expected: Auto-scroll resumes and the view follows new content again

- [ ] Scenario 12: Phase transition clears the buffer
  - Input: Observe the content area as one phase completes and the next begins
  - Expected: The content area clears completely. Scroll offset resets to 0. Auto-scroll is re-enabled. New phase's streaming text begins appearing from the top.

- [ ] Scenario 13: `PageUp` / `PageDown` scroll by visible page height
  - Input: Accumulate enough content to exceed visible height, then press `PageUp` followed by `PageDown`
  - Expected: Each press scrolls by approximately one page (the content area's visible height). Scroll offset is clamped within valid bounds.

- [ ] Scenario 14: Help bar shows scroll hints during active phase
  - Input: Observe the help bar while a phase is running
  - Expected: Help bar shows `[Ctrl+C] Cancel  [↑↓] Scroll  [End] Auto-scroll` instead of just `[Ctrl+C] Cancel`

## Edge Cases

- [ ] Empty agent response: If the agent produces no text in a phase (e.g., review disabled), the content area remains empty without errors
- [ ] Very long single line: A line exceeding the content area width wraps correctly (via `Wrap { trim: false }`) rather than being truncated or causing layout overflow
- [ ] Rapid text deltas: Hundreds of `AgentTextDelta` events arriving in a single 100ms tick are coalesced and appended without noticeable lag or dropped events
- [ ] Newline-only deltas: A delta containing only `\n` characters correctly advances lines without producing spurious empty styled spans
- [ ] Partial line followed by newline: Delta `"hello"` followed by delta `" world\n"` produces a single complete line `"hello world"` followed by a new empty line
- [ ] Malformed `StreamEvent.event` JSON: Missing `type`, `delta`, or `text` fields return `None` from `extract_text_delta` without panic or error log spam
- [ ] Unknown `StreamEvent` event type: Events like `content_block_start`, `content_block_stop`, `message_start`, etc. are silently ignored
- [ ] Phase name longer than sidebar width: Name is truncated with `…` suffix in the sidebar; full name appears in the content area title
- [ ] Zero phases in initial state: Sidebar renders empty without panic (degenerate but possible during pre-loading)
- [ ] `Ctrl+C` still works during scroll: Pressing `Ctrl+C` while scrolled up still cancels the run immediately

## Integration Points

- [ ] **`RunEvent` channel compatibility**: Existing `RunEvent` consumers (the `handle_event` match in `RunUi`) handle new variants without falling through to a catch-all that drops them
- [ ] **`include_partial_messages` flag propagation**: The flag is correctly passed through `AgentProfile::Coder.to_options()` → `ClaudeAgentOptions` → `SubprocessTransport` CLI args (verify `--include-partial-messages` appears in the spawned process args)
- [ ] **Run logger not affected**: The structured run log (`.coda/<slug>/logs/run-*.log`) still records complete prompt/response pairs with correct metrics; streaming events do not pollute the log
- [ ] **State persistence unaffected**: `state.yml` is written correctly at phase transitions with proper status, timing, cost, and token fields — identical behavior to pre-change
- [ ] **PR creation still works**: The `create_pr` phase still extracts PR URLs from tool output and/or `gh pr list` fallback; streaming events don't interfere
- [ ] **Review/Verify detail text still shows**: `RunEvent::ReviewRound` and `RunEvent::VerifyAttempt` detail strings still appear in the sidebar (previously shown inline with the phase line)
- [ ] **Resume from crash**: Resuming an interrupted run (phase status = `Running` in `state.yml`) still works correctly; streaming UI initializes cleanly on resume

## Performance

- [ ] UI tick rate remains smooth (100ms interval) even under heavy streaming — no visible stutter or frame drops
- [ ] Memory usage during a long phase (>5 minutes of continuous streaming) stays bounded and reasonable (content buffer grows linearly, not exponentially)
- [ ] Channel drain loop processes all queued events within a single tick without blocking the draw cycle

## Security

- [ ] `summarize_tool_input` does not expose sensitive data: Bash command summaries are truncated to 60 chars; no secrets or tokens are displayed if the agent invokes tools with sensitive arguments
- [ ] `extract_text_delta` does not execute or eval any content from the `StreamEvent.event` JSON — it only reads string values