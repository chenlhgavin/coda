

# Task: refactor

## Overview

The CODA codebase has accumulated significant complexity in two god objects — `Runner` (~2900 lines, 7+ concerns) and `Engine` (~1100 lines) — that violate the Single Responsibility Principle and make isolated testing difficult. Duplicated streaming/timeout/reconnect logic exists between `PlanSession` and `Runner`. Several correctness bugs affect budget tracking across resumed sessions, verify loop bounds, and state migration ordering. Blocking git/gh CLI operations are called directly from async contexts without `spawn_blocking`. Multiple stringly-typed fields lack type safety where enums would prevent invalid states.

This refactoring decomposes the god objects into focused components, extracts shared abstractions, fixes correctness bugs, and introduces proper type safety — all while preserving the existing Slack and TUI user experience unchanged.

## Root Cause Analysis

`Runner` grew organically to own: agent session lifecycle (connect/disconnect/reconnect with idle timeout), phase execution (5 distinct phase types), state persistence, metrics tracking, review orchestration (3 engine modes), verification loops, git operations, and PR creation. When a new concern was added, it was bolted onto Runner rather than extracted.

Similarly, `Engine::init` contains inline streaming loops (`run_analyze_phase`, `run_setup_phase`) that duplicate the timeout/accumulation pattern from Runner but with slight differences, because no shared abstraction existed.

The `PlanSession::send_inner` method independently re-implemented the same idle timeout + exponential backoff reconnection pattern as `Runner::send_and_collect`, including its own copy of the tool summary function.

Correctness bugs stem from implicit assumptions: budget tracking assumes a single continuous session (breaks on resume), the verify loop uses `max_retries` in a `0..=max_retries` range (off-by-one), and state migration appends phases without validating canonical ordering.

## Proposed Solution

Decompose into 7 phases following a bottom-up dependency order:

1. Extract shared agent session abstraction (eliminates DRY violations)
2. Decompose Runner into trait-based phase executors (fixes SRP/OCP)
3. Fix correctness bugs (budget, verify bounds, migration, PR title)
4. Wrap blocking operations in `spawn_blocking` (CLAUDE.md compliance)
5. Introduce type-safe enums (replace stringly-typed fields)
6. Extract init flow and add cancellation support
7. Extract state persistence layer

## Affected Areas

| Module | Nature of Change |
|--------|-----------------|
| `crates/coda-core/src/runner.rs` | Major decomposition — reduced to thin orchestrator |
| `crates/coda-core/src/planner.rs` | Refactor `send_inner` to delegate to `AgentSession` |
| `crates/coda-core/src/engine.rs` | Init flow refactor, cancellation wiring |
| `crates/coda-core/src/session.rs` | **New** — shared agent session abstraction |
| `crates/coda-core/src/phases/mod.rs` | **New** — `PhaseExecutor` trait and `PhaseContext` |
| `crates/coda-core/src/phases/dev.rs` | **New** — dev phase executor |
| `crates/coda-core/src/phases/review.rs` | **New** — review phase executor (all 3 engines) |
| `crates/coda-core/src/phases/verify.rs` | **New** — verify phase executor |
| `crates/coda-core/src/phases/docs.rs` | **New** — update-docs phase executor |
| `crates/coda-core/src/phases/pr.rs` | **New** — PR creation phase executor |
| `crates/coda-core/src/state.rs` | `StateManager` extraction, migration fix, `spent_budget()` |
| `crates/coda-core/src/git.rs` | `AsyncGitOps` wrapper |
| `crates/coda-core/src/gh.rs` | `AsyncGhOps` wrapper, `PrState` enum |
| `crates/coda-core/src/reviewer.rs` | `ReviewSeverity` enum |
| `crates/coda-core/src/config.rs` | `ReasoningEffort` enum, verify field rename |
| `crates/coda-core/src/error.rs` | `Cancelled` variant addition |
| `crates/coda-core/src/lib.rs` | Re-exports for new public types |
| `apps/coda-cli/src/app.rs` | Internal API adjustments only (no behavioral change) |
| `apps/coda-server/` | Internal API adjustments only (no behavioral change) |

## Development Phases

### Phase 1: Extract Agent Session Abstraction

- **Goal**: Eliminate the largest DRY violation by creating a shared `AgentSession` that encapsulates the streaming/timeout/reconnect pattern used by both `PlanSession` and `Runner`.
- **Tasks**:
  - Create `crates/coda-core/src/session.rs` with `AgentSession` struct wrapping `ClaudeClient`
  - Move and unify `send_and_collect` / `send_inner` logic into `AgentSession::send()` with idle timeout, exponential backoff reconnection, response accumulation, and API error detection
  - Define a `SessionEvent` enum for streaming callbacks (text delta, tool activity, turn completed, idle warning, reconnecting) so callers receive events without the session knowing about `RunEvent` or `PlanStreamUpdate`
  - Accept an `Option<UnboundedSender<SessionEvent>>` for event emission
  - Move shared free functions into the session module: `collect_tool_result_text`, `extract_text_delta`, `contains_api_error_pattern`, `summarize_tool_input` (unified from the two duplicate implementations), `TOOL_SUMMARY_MAX_LEN` constant, `truncate_str`
  - Define `AgentResponse` struct in session module (currently in `runner.rs`)
  - Refactor `PlanSession::send_inner` to create/delegate to `AgentSession::send()`
  - Refactor `Runner::send_and_collect` to delegate to `AgentSession::send()`
  - Remove duplicate `summarize_plan_tool` from `planner.rs` and `TOOL_SUMMARY_MAX_LEN` from both modules
  - Migrate all existing tests for `summarize_tool_input`, `truncate_str`, `contains_api_error_pattern` from `runner.rs::tests` to `session.rs::tests`
  - Write new tests for `AgentSession::send()` covering: normal completion, idle timeout with successful reconnect, idle timeout with all retries exhausted, empty response detection, budget exhaustion detection, API error pattern detection
- **Commit message**: "refactor(core): extract AgentSession abstraction from Runner and PlanSession"

### Phase 2: Decompose Runner into Phase Executors

- **Goal**: Break the `Runner` god object into focused phase executor components using a trait-based dispatch, fixing both SRP and OCP violations.
- **Tasks**:
  - Create `crates/coda-core/src/phases/mod.rs` with `PhaseExecutor` trait and `PhaseContext` struct:
    ```rust
    #[async_trait]
    pub trait PhaseExecutor: Send {
        async fn execute(&mut self, ctx: &mut PhaseContext) -> Result<TaskResult, CoreError>;
    }

    pub struct PhaseContext {
        pub session: AgentSession,
        pub state: FeatureState,
        pub state_path: PathBuf,
        pub worktree_path: PathBuf,
        pub pm: PromptManager,
        pub config: CodaConfig,
        pub git: Arc<dyn GitOps>,
        pub gh: Arc<dyn GhOps>,
        pub progress_tx: Option<UnboundedSender<RunEvent>>,
        pub run_logger: Option<RunLogger>,
        pub metrics: MetricsTracker,
        pub review_summary: ReviewSummary,
        pub verification_summary: VerificationSummary,
    }
    ```
  - Extract `crates/coda-core/src/phases/dev.rs` — `DevPhaseExecutor` from `Runner::run_dev_phase`, parameterized by phase index
  - Extract `crates/coda-core/src/phases/review.rs` — `ReviewPhaseExecutor` from `Runner::run_review`, `run_review_claude`, `run_review_codex`, `ask_claude_to_fix`, `finalize_review_phase`, and `resolve_review_engine`
  - Extract `crates/coda-core/src/phases/verify.rs` — `VerifyPhaseExecutor` from `Runner::run_verify`
  - Extract `crates/coda-core/src/phases/docs.rs` — `DocsPhaseExecutor` from `Runner::run_update_docs`, `validate_doc_files`, `build_doc_fix_prompt`
  - Extract `crates/coda-core/src/phases/pr.rs` — `PrPhaseExecutor` from `Runner::create_pr`, `prepare_squash`, `extract_pr_url`, `extract_pr_number`
  - Refactor `Runner::execute()` to build executor instances by `PhaseKind` and dispatch via the trait — replacing the current `match phase_name.as_str()` string matching at `runner.rs:878-895`
  - Add `PhaseContext` helper methods: `emit_event()`, `save_state()`, `load_spec()`, `get_diff()`, `commit_coda_state()`
  - Write unit tests for each executor in isolation using mock `AgentSession` (test prompt rendering, state transitions, and error handling)
  - Ensure the existing `runner.rs::tests` continue to pass (they test free functions that are now in phases or session modules)
- **Commit message**: "refactor(core): decompose Runner into trait-based phase executors"

### Phase 3: Fix Correctness Bugs

- **Goal**: Fix four identified correctness issues: budget tracking across sessions, verify loop off-by-one, state migration ordering, and PR title extraction.
- **Tasks**:
  - **Budget tracking**: Add `fn spent_budget(&self) -> f64` method to `FeatureState` that sums `cost_usd` across all completed phases. In `Runner::execute()` (or `PhaseContext` construction), compute `remaining_budget = config.agent.max_budget_usd - state.spent_budget()` and pass it to `AgentSession` / SDK options. Add test: create a state with 3 completed phases totaling $2.50, verify `spent_budget()` returns 2.50 and remaining budget is correctly reduced.
  - **Verify off-by-one**: The current loop `for attempt in 0..=max_retries` runs `max_retries + 1` iterations. Restructure to: run initial verification attempt, then `for retry in 0..max_retries` for retry attempts. Rename config field to `max_verify_retries` with `#[serde(alias = "max_retries")]` for backward compatibility. Fix `VerifyAttempt` event to report consistent `attempt` (1-based) and `max_attempts` (= 1 + max_retries). Add test asserting exactly `max_retries` retry attempts after initial failure.
  - **Migration ordering**: In `state.rs::FeatureState::migrate()`, after appending missing quality phases, sort all quality phases (those with `kind == PhaseKind::Quality`) according to the canonical order in `QUALITY_PHASES` constant. Add a validation check that no `PhaseKind::Dev` phase appears after the first `PhaseKind::Quality` phase. Add test: legacy state `[dev1, verify, dev2, review]` → migrated to `[dev1, dev2, review, verify, update-docs]`.
  - **PR title**: In `PrPhaseExecutor`, after `gh pr create` succeeds, extract the PR title from the agent response text or from `gh pr view --json title` output. Fall back to the generated `feat(<slug>): ...` title only if extraction fails. Add test for title extraction from sample `gh pr create` output.
- **Commit message**: "fix(core): fix budget tracking, verify bounds, migration order, and PR title extraction"

### Phase 4: Wrap Blocking Ops in `spawn_blocking`

- **Goal**: Comply with the CLAUDE.md rule: "Avoid blocking operations in async contexts. Use `tokio::task::spawn_blocking` for CPU-intensive or blocking operations."
- **Tasks**:
  - Create `crates/coda-core/src/async_ops.rs` with `AsyncGitOps` struct wrapping `Arc<dyn GitOps>` and providing async versions of all methods via `tokio::task::spawn_blocking`
  - Create `AsyncGhOps` struct wrapping `Arc<dyn GhOps>` similarly
  - Both wrappers implement a new async trait (e.g., `AsyncGitOperations`, `AsyncGhOperations`) or simply provide inherent async methods
  - Audit all callsites in `runner.rs` (now phases/*.rs), `planner.rs`, and `engine.rs` where git/gh sync methods are called from async functions; replace with async wrapper calls
  - Callsites in sync contexts (e.g., `Engine::plan()`, `Engine::scan_cleanable_worktrees()`) continue using the sync traits directly — no change needed
  - Write tests verifying the async wrappers correctly delegate to the underlying sync trait (using mock implementations)
- **Commit message**: "refactor(core): wrap blocking git/gh operations in spawn_blocking for async safety"

### Phase 5: Introduce Type-Safe Enums

- **Goal**: Replace stringly-typed fields with proper enums to leverage Rust's type system for correctness.
- **Tasks**:
  - Define `ReviewSeverity` enum in `crates/coda-core/src/reviewer.rs`:
    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum ReviewSeverity { Critical, Major, Minor, Info }
    ```
    Replace `ReviewIssue.severity: String` field. Update `parse_review_issues` and `parse_review_issues_structured` to produce `ReviewSeverity`. Update filtering logic that checks `severity == "critical"` etc.
  - Define `PrState` enum in `crates/coda-core/src/gh.rs`:
    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "UPPERCASE")]
    pub enum PrState { Open, Merged, Closed, Draft }
    ```
    Replace `PrStatus.state: String` and `CleanedWorktree.pr_state: String`. Update `engine.rs:922-923` comparison logic from `state.to_uppercase() != "MERGED"` to pattern matching. Implement `Display` for user-facing output.
  - Define `ReasoningEffort` enum in `crates/coda-core/src/config.rs`:
    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum ReasoningEffort { Low, Medium, High }
    ```
    Replace `ReviewConfig.codex_reasoning_effort: String`. Implement `Display` for CLI arg passthrough to codex.
  - Add `FromStr` implementations for all three enums for CLI argument parsing
  - Write exhaustive tests for serialization roundtrip, `Display`/`FromStr` consistency, and pattern matching completeness
- **Commit message**: "refactor(core): replace stringly-typed fields with type-safe enums"

### Phase 6: Extract Init Flow and Add Cancellation

- **Goal**: Clean up Engine's init flow to use the shared `AgentSession` abstraction, and add graceful cancellation support throughout the execution pipeline.
- **Tasks**:
  - **Init refactor**: Refactor `Engine::run_analyze_phase` and `Engine::run_setup_phase` to use `AgentSession::send()` instead of inline streaming loops. Map `SessionEvent` to `InitEvent` in the calling code. This eliminates the duplicated message processing logic in `engine.rs:500-670`.
  - **Cancellation**: Add `tokio_util::sync::CancellationToken` parameter to `Engine::run()`, `Runner::execute()`, and `AgentSession::send()`. Check `token.is_cancelled()` between phases in the orchestration loop. In the streaming loop, use `tokio::select!` to race the stream against the cancellation token. Add `CoreError::Cancelled` variant. When cancelled: save current state, disconnect agent, return `Err(CoreError::Cancelled)`.
  - Wire cancellation from CLI: create the token in `App::run_tui()`, clone it into the Ctrl+C handler (replacing the current `process::exit` behavior), pass it to `engine.run()`.
  - Wire cancellation from server: create the token per-session, cancel on Slack thread timeout or explicit user cancel command.
  - Write tests: verify that cancellation between phases saves state and returns `CoreError::Cancelled`. Verify that cancellation during streaming causes graceful teardown.
- **Commit message**: "refactor(core): extract init flow into AgentSession and add graceful cancellation"

### Phase 7: Extract State Persistence Layer

- **Goal**: Centralize state mutation and persistence into a dedicated `StateManager`, making state transitions testable independently of the agent session.
- **Tasks**:
  - Create `StateManager` struct in `crates/coda-core/src/state.rs` (or a new `state_manager.rs` file):
    ```rust
    pub struct StateManager {
        state: FeatureState,
        state_path: PathBuf,
    }
    ```
  - Move methods from `Runner` / `PhaseContext` into `StateManager`: `save_state()` → `save()`, `mark_phase_running()`, `complete_phase()`, `update_totals()`, `restore_summaries_from_state()`, `spent_budget()`
  - `StateManager` enforces invariants:
    - Phases can only transition `Pending → Running → Completed` or `Pending → Running → Failed`
    - `current_phase` is derived from phase statuses (not stored as an independent counter), eliminating the stale-counter concern noted in the existing code comment at `runner.rs:816-821`
    - Feature status transitions are validated (e.g., cannot go from `Completed` back to `InProgress`)
  - Update `PhaseContext` to hold `StateManager` instead of raw `FeatureState` + `state_path`
  - Write comprehensive tests for state transitions: valid transitions succeed, invalid transitions return errors, `save()` persists to disk, `spent_budget()` sums correctly, `restore_summaries_from_state()` reconstructs review and verification summaries accurately
- **Commit message**: "refactor(core): extract StateManager for centralized state persistence and validation"

## Dependencies

| Crate | Version | Purpose | Phase |
|-------|---------|---------|-------|
| `tokio-util` | `0.7` | `CancellationToken` for graceful shutdown | Phase 6 |

No other new dependencies. `tokio-util` is already a transitive dependency of `tokio-tungstenite`.

## Risk & Trade-offs

1. **Phase 2 carries the most risk** — decomposing Runner touches the critical execution path. Mitigation: TDD approach writes tests for each executor before extraction. The existing `runner.rs::tests` module provides regression coverage. Each executor can be extracted and tested independently.

2. **Phase ordering is critical** — Phase 1 must complete before Phase 2 (executors depend on `AgentSession`). Phase 3 can run in parallel with Phase 2. Phases 4 and 5 are independent of each other and can follow in any order. Phase 6 depends on Phase 1. Phase 7 depends on Phase 2.

3. **Config field rename in Phase 3** — Renaming `max_retries` to `max_verify_retries` is a breaking change to existing `config.yml` files. Mitigated by `#[serde(alias = "max_retries")]` for backward-compatible deserialization. Existing configs continue to work.

4. **`AsyncGitOps` indirection in Phase 4** — Adds a wrapper layer. The alternative of making `GitOps` natively async was rejected because the trait is also used in synchronous contexts (`Engine::plan`, `Engine::scan_cleanable_worktrees`). The wrapper approach keeps the sync trait for sync callers and provides async versions only where needed.

5. **`PhaseContext` shared mutable state** — `PhaseContext` bundles several mutable concerns (session, state, metrics, summaries). An alternative is full Actor isolation per-executor with channel-based communication, but that adds complexity disproportionate to the current codebase size. The `PhaseContext` approach is pragmatic and testable.

6. **Scope is deliberately conservative** — This plan excludes cosmetic issues, event-sourced state persistence, a plugin system for custom phase types, and full Actor-model isolation. These are deferred as potential future improvements once the foundational decomposition is stable.