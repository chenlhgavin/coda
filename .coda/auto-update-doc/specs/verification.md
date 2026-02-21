

# Verification Plan: auto-update-doc

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
  - [ ] `cargo test`
  - [ ] `cargo deny check`
  - [ ] `typos`
- [ ] All new tests pass
- [ ] No regression in existing tests

## Functional Verification

### Pipeline Structure

- [ ] `QUALITY_PHASES` includes `"update-docs"` as the third entry after `"review"` and `"verify"`
  - Input: Inspect `crates/coda-core/src/planner.rs` constant
  - Expected: `&["review", "verify", "update-docs"]`

- [ ] Planning a new feature produces a `FeatureState` with 4+ phases where the last three quality phases are `review`, `verify`, `update-docs` in that order
  - Input: Run `build_initial_state()` with any dev phase list
  - Expected: Phase records end with `review` (Quality), `verify` (Quality), `update-docs` (Quality)

- [ ] `MIN_PHASE_COUNT` is `4` and validation rejects states with fewer than 4 phases
  - Input: Construct a `FeatureState` with 3 phases (1 dev + review + verify, no update-docs)
  - Expected: `validate()` returns `Err` mentioning "at least 4 phases"

### Phase Dispatch

- [ ] The `execute()` loop dispatches `"update-docs"` to `run_update_docs()`
  - Input: Inspect the `PhaseKind::Quality` match arm in `runner.rs`
  - Expected: `"update-docs" => self.run_update_docs(phase_idx).await` is present alongside `"review"` and `"verify"`

- [ ] `Task::UpdateDocs` variant exists with a `feature_slug: String` field
  - Input: Inspect `crates/coda-core/src/task.rs`
  - Expected: Variant compiles and is used in `run_update_docs()` return value

### Prompt Template

- [ ] Template file exists at `crates/coda-pm/templates/run/update_docs.j2`
  - Input: Check file on disk
  - Expected: File exists and is non-empty

- [ ] Template renders without error when given required variables
  - Input: Call `PromptManager::render("run/update_docs", context)` with `design_spec` (string) and `state` (FeatureState)
  - Expected: Returns `Ok(String)` containing instructions for both `.coda.md` and `README.md`

- [ ] Template is auto-discovered by `PromptManager` (no manual registration needed)
  - Input: Create a `PromptManager` and attempt to render `"run/update_docs"`
  - Expected: Template is found without any code changes to `PromptManager`

### Run Update Docs — Happy Path

- [ ] `run_update_docs()` marks the phase as running, sends prompt, and completes the phase
  - Input: Execute a run where all prior phases succeed and both `.coda.md` and `README.md` already exist in the worktree
  - Expected: Phase transitions through `Running → Completed`, `TaskResult` has `TaskStatus::Completed`

- [ ] After `run_update_docs()` completes, both `.coda.md` and `README.md` are committed in the worktree
  - Input: Check git log in worktree after update-docs phase
  - Expected: A commit exists with message matching `docs(<slug>): update .coda.md and README.md`

- [ ] `run_update_docs()` returns a `TaskResult` with `Task::UpdateDocs { feature_slug }` and accurate metrics (turns, cost_usd, duration)
  - Input: Inspect the returned `TaskResult`
  - Expected: Task variant is `UpdateDocs`, metrics are non-zero

### Retry Logic

- [ ] When `.coda.md` is missing after the first attempt, the runner retries with a fix prompt
  - Input: Agent produces output but does not create `.coda.md` on the first attempt
  - Expected: Runner detects missing file, sends fix prompt, retries

- [ ] When `README.md` is empty after the first attempt, the runner retries
  - Input: Agent creates an empty `README.md`
  - Expected: Runner detects empty file, sends fix prompt, retries

- [ ] When all retry attempts are exhausted, `run_update_docs()` returns `Err(CoreError::AgentError(...))`
  - Input: Agent fails to produce valid files on every attempt (0..=max_retries)
  - Expected: Error propagates, no PR is created

- [ ] Metrics accumulate across all retry attempts
  - Input: Phase requires 2 attempts before succeeding
  - Expected: `TaskResult` turns and cost_usd reflect the sum of all attempts

### Phase Ordering in Execute

- [ ] `update-docs` runs after `verify` and before `create_pr()`
  - Input: Run a full pipeline and observe phase execution order
  - Expected: Log/event sequence shows `... → verify → update-docs → CreatingPr`

- [ ] If `update-docs` fails (after retries), `create_pr()` is never called
  - Input: Force `run_update_docs()` to fail
  - Expected: `execute()` returns `Err`, no `RunEvent::CreatingPr` is emitted

## Edge Cases

- [ ] `README.md` does not exist in worktree before `update-docs` runs — agent should create it
  - Expected: Phase succeeds, `README.md` is created and committed

- [ ] `.coda.md` does not exist in worktree before `update-docs` runs — agent should create it
  - Expected: Phase succeeds, `.coda.md` is created and committed

- [ ] Both files exist but are unchanged by the agent — `commit_doc_updates()` is a no-op (no empty commit)
  - Expected: `commit_coda_artifacts` silently succeeds with no commit created (existing behavior of the helper)

- [ ] Resuming a run where `update-docs` was previously `Running` (crash recovery)
  - Input: Set `update-docs` phase status to `Running` in `state.yml`, restart the run
  - Expected: Runner detects the phase is not `Completed`, re-executes it from scratch

- [ ] Resuming a run where `update-docs` was already `Completed`
  - Input: Set `update-docs` phase status to `Completed` in `state.yml`, restart the run
  - Expected: Phase is skipped, proceeds directly to `create_pr()`

- [ ] `max_retries` is `0` — only one attempt, no retry loop
  - Input: Config with `agent.max_retries: 0`, agent fails on first attempt
  - Expected: Immediately returns `Err(CoreError::AgentError(...))`

## Integration Points

- [ ] `FeatureState` round-trip serialization works with the new 4-phase minimum
  - Serialize a state with `update-docs` phase to YAML, deserialize, and validate

- [ ] `FeatureScanner` correctly discovers features whose `state.yml` includes the `update-docs` phase
  - Scan `.trees/` with a state file containing 4+ phases including `update-docs`

- [ ] `RunEvent` sequence is correct for UI consumers — `PhaseStarting`/`PhaseCompleted` events are emitted for `update-docs`
  - Subscribe to events, run pipeline, verify `update-docs` appears in the event stream

- [ ] The `create_pr.j2` template correctly lists `update-docs` in the phase results table
  - Existing template uses `{% for phase in state.phases %}` — verify the new phase appears in PR body

- [ ] `commit_doc_updates()` uses `commit_coda_artifacts` with `--no-verify` (consistent with other CODA commits)
  - Inspect the call; confirm it delegates to `commit_coda_artifacts` which uses `--no-verify`

- [ ] Run logger records interactions for the `update-docs` phase
  - Check that `run_logger.log_interaction()` is called during `run_update_docs()`