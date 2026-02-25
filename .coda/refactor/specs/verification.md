

# Verification Plan: refactor

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] All new tests pass: `cargo nextest run --all-features`
- [ ] No regression in existing tests — every test that existed before the refactoring continues to pass (migrated tests count as passing if they pass in their new module location)
- [ ] Code coverage is maintained or improved — specifically, the extracted modules (`session.rs`, `phases/*.rs`, `state_manager.rs`, `async_ops.rs`) have test coverage for all public methods

## Functional Verification

### Phase 1: AgentSession Abstraction

- [ ] Scenario: PlanSession streaming conversation
  - Input: `coda plan test-feature`, type a message, receive streaming response
  - Expected: Streaming text appears incrementally in TUI, tool activity events display correctly, response completes without timeout. Behavior is identical to pre-refactor.

- [ ] Scenario: Runner dev phase execution through AgentSession
  - Input: `coda run test-feature` on a planned feature with one dev phase
  - Expected: Dev phase executes, streams text/tool events, commits result, updates state. Behavior is identical to pre-refactor.

- [ ] Scenario: Idle timeout triggers reconnection
  - Input: Agent becomes unresponsive during a phase (simulate by setting `idle_timeout_secs: 5` and introducing network delay)
  - Expected: `SessionEvent::IdleWarning` emitted, subprocess reconnected with exponential backoff, prompt re-sent, phase completes successfully. Partial data from stalled session is discarded.

- [ ] Scenario: All idle retries exhausted
  - Input: Agent remains unresponsive beyond all configured retries
  - Expected: `CoreError::IdleTimeout` returned with correct `total_idle_secs` and `retries_exhausted` values

### Phase 2: Phase Executors

- [ ] Scenario: Full pipeline execution (dev → review → verify → docs → PR)
  - Input: `coda run test-feature` on a multi-phase feature
  - Expected: Each phase dispatched to the correct executor, events emitted in the correct order (`PhaseStarting` → `PhaseCompleted` for each), state.yml updated after each phase, PR created at the end

- [ ] Scenario: Resume from interrupted run
  - Input: A feature with `state.yml` showing 2 of 4 phases completed, then `coda run test-feature`
  - Expected: Execution resumes from phase 3, completed phases are skipped, review/verification summaries are correctly restored from state

- [ ] Scenario: Phase failure handling
  - Input: A dev phase where the agent returns an error
  - Expected: `PhaseFailed` event emitted, state.yml records the phase as `Failed`, feature status set to `Failed`, run terminates with error

### Phase 3: Correctness Fixes

- [ ] Scenario: Budget tracking across resumed sessions
  - Input: Feature with 3 completed phases costing $1.00 each, `max_budget_usd: 5.00`, resume run
  - Expected: New session starts with `remaining_budget = 2.00`, not `5.00`. If the next phase costs $2.50, budget exhaustion is detected.

- [ ] Scenario: Verify loop respects exact retry count
  - Input: `max_verify_retries: 2` in config, verification always fails
  - Expected: Exactly 3 total attempts (1 initial + 2 retries). `VerifyAttempt` events report `attempt: 1/3`, `attempt: 2/3`, `attempt: 3/3`.

- [ ] Scenario: Backward-compatible config deserialization
  - Input: Existing `config.yml` with `max_retries: 3` (old field name)
  - Expected: Config loads successfully, `max_verify_retries` is set to 3

- [ ] Scenario: State migration with out-of-order quality phases
  - Input: Legacy `state.yml` with phases `[dev1, verify, dev2, review]`
  - Expected: After migration, phases are `[dev1, dev2, review, verify, update-docs]` — dev phases first in original order, quality phases in canonical order, missing `update-docs` appended

- [ ] Scenario: PR title extracted from actual PR
  - Input: `gh pr create` succeeds and outputs a URL and title
  - Expected: `state.pr.title` reflects the actual PR title, not the hardcoded `feat(<slug>): feature implementation`

### Phase 4: Async Safety

- [ ] Scenario: Git push from async context
  - Input: `coda run test-feature` reaching the push phase
  - Expected: `git push` executes on a blocking thread (via `spawn_blocking`), does not block the tokio runtime. Other async tasks (event emission, timeout checking) continue to operate.

- [ ] Scenario: Sync callers unaffected
  - Input: `coda plan test-feature` (uses `Engine::plan()` which is sync)
  - Expected: `GitOps` trait methods called directly without async wrappers, no behavioral change

### Phase 5: Type-Safe Enums

- [ ] Scenario: Review severity filtering
  - Input: Review output containing critical, major, and minor issues
  - Expected: Only `Critical` and `Major` severity issues trigger fix attempts. `Minor` and `Info` are logged but not acted upon. No string comparison anywhere in the review path.

- [ ] Scenario: PR state matching in clean command
  - Input: `coda clean` with features whose PRs are in various states
  - Expected: `PrState::Merged` and `PrState::Closed` features are offered for cleanup. `PrState::Open` and `PrState::Draft` are skipped. No `.to_uppercase()` string comparison.

- [ ] Scenario: Config with reasoning effort enum
  - Input: `config.yml` with `codex_reasoning_effort: high`
  - Expected: Deserializes to `ReasoningEffort::High`, passed correctly to codex CLI

### Phase 6: Init Flow & Cancellation

- [ ] Scenario: `coda init` uses AgentSession
  - Input: `coda init` on a fresh project
  - Expected: Analyze and setup phases stream correctly, `InitEvent` events emitted, `.coda/` and `.trees/` created. Identical user-visible behavior to pre-refactor.

- [ ] Scenario: Ctrl+C during run cancels gracefully
  - Input: `coda run test-feature`, press Ctrl+C during a dev phase
  - Expected: Current state saved to `state.yml`, agent session disconnected, terminal restored, message displayed: "Progress has been saved. Run `coda run test-feature` to resume." No `process::exit`.

- [ ] Scenario: Cancellation between phases
  - Input: Cancel token triggered between phase 2 completion and phase 3 start
  - Expected: Phase 2 marked as completed in state, `CoreError::Cancelled` returned, no partial phase 3 execution

### Phase 7: State Manager

- [ ] Scenario: State transitions enforced
  - Input: Attempt to mark a `Completed` phase as `Running`
  - Expected: Error returned, state unchanged

- [ ] Scenario: `current_phase` derived not stored
  - Input: Resume a run where `state.yml` has stale `current_phase` counter but accurate phase statuses
  - Expected: Execution resumes from the correct phase based on phase statuses, ignoring the stale counter

- [ ] Scenario: `spent_budget()` accuracy
  - Input: State with phases: dev1 ($1.20 completed), dev2 ($0.80 completed), review ($0 pending)
  - Expected: `spent_budget()` returns `2.00`

## Edge Cases

- [ ] Empty feature with zero dev phases — only quality phases in the pipeline. Executors dispatch correctly, no dev phase executor instantiated.
- [ ] All phases already completed on resume — `Runner::execute()` skips to PR creation immediately
- [ ] `max_verify_retries: 0` — exactly 1 verification attempt, no retries
- [ ] `max_budget_usd` set lower than already-spent budget on resume — immediately returns `CoreError::BudgetExhausted` before starting a new phase
- [ ] Agent returns empty response on first turn of a phase — detected and reported as `CoreError::AgentError`, not silently swallowed
- [ ] `config.yml` with unknown enum value (e.g., `codex_reasoning_effort: "turbo"`) — deserialization fails with a clear error message naming the valid variants
- [ ] `state.yml` migration from pre-quality-phase format — phases list containing only dev phases gets all three quality phases appended in canonical order
- [ ] Cancellation token triggered during `AgentSession` reconnection backoff sleep — cancellation takes effect immediately, does not wait for backoff to complete
- [ ] Concurrent `save_state()` calls from state manager — not possible by design (single-threaded ownership), but verify `StateManager` is `!Sync` to prevent accidental sharing
- [ ] `PrState` deserialization from GitHub API returning lowercase (`"merged"`) vs uppercase (`"MERGED"`) — `#[serde(rename_all = "UPPERCASE")]` handles the canonical form; add `#[serde(alias)]` or case-insensitive parsing for robustness
- [ ] `ReviewSeverity` parsing from agent text containing mixed case (`"CRITICAL"`, `"Critical"`, `"critical"`) — `FromStr` implementation handles all cases

## Integration Points

- [ ] `apps/coda-cli` compiles and runs without behavioral changes — `coda init`, `coda plan`, `coda run`, `coda list`, `coda status`, `coda clean` all produce identical user-visible output
- [ ] `apps/coda-server` compiles and runs without behavioral changes — Slack command dispatch, session management, and progress streaming to threads all function identically
- [ ] `coda-pm` (PromptManager) API unchanged — all template rendering calls from phase executors and engine use the same `pm.render()` interface
- [ ] `claude-agent-sdk-rs` integration unchanged — `AgentSession` wraps `ClaudeClient` with the same connect/query/receive_response pattern, no SDK API changes required
- [ ] `state.yml` format backward compatible — existing state files from previous versions load correctly after migration. New state files written by the refactored code can be read by the previous version's deserializer (no removed fields, only added fields with defaults).
- [ ] `config.yml` format backward compatible — existing configs with `max_retries`, string-valued `codex_reasoning_effort` load correctly via `#[serde(alias)]` and flexible deserialization
- [ ] Git worktree layout unchanged — `.trees/<slug>/` structure, `.coda/<slug>/state.yml` and `.coda/<slug>/specs/` paths identical
- [ ] RunEvent and InitEvent enum variants unchanged — TUI and plain-text renderers receive the same events in the same order (new internal `SessionEvent` is mapped, not exposed)

## Performance

- [ ] No measurable regression in phase execution time — the `PhaseExecutor` trait dispatch and `AgentSession` delegation add negligible overhead compared to agent API latency
- [ ] `spawn_blocking` for git operations does not introduce contention — verify via `tracing` that blocking tasks complete promptly and don't starve the tokio worker pool (git operations are sequential per-feature, not parallel)
- [ ] State serialization/deserialization performance unchanged — `StateManager::save()` writes the same YAML format with the same frequency as before

## Security

- [ ] No new `unsafe` blocks introduced anywhere in the refactored code
- [ ] `AgentSession` does not log or expose API keys, tokens, or sensitive configuration values — verify `Debug` implementation on `AgentSession` redacts the `ClaudeClient` internals
- [ ] `CancellationToken` does not introduce a denial-of-service vector — only the owning CLI/server layer can trigger cancellation, not external input
- [ ] `AsyncGitOps` / `AsyncGhOps` wrappers do not introduce command injection — they pass through to the same `GitOps`/`GhOps` implementations that already sanitize arguments