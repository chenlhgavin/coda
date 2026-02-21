

# Feature: auto-update-doc

## Overview

Add an `"update-docs"` quality phase to the CODA run pipeline, positioned after `verify` and before PR creation. This phase instructs the Claude agent to fully regenerate `.coda.md` (the repository overview used as AI context) and update `README.md` to reflect changes introduced by the feature. The phase is always-on (no config toggle), uses the existing `max_retries` for retry logic, and aborts the run if all attempts fail — ensuring documentation is always in sync with code before a PR is opened.

The pipeline order becomes: `dev phases → review → verify → update-docs → create-pr`.

## High-Level Design

```
┌──────────────────────────────────────────────────────────────┐
│                     Runner::execute()                        │
│                                                              │
│  for phase in phases:                                        │
│    Dev      → run_dev_phase()                                │
│    review   → run_review()                                   │
│    verify   → run_verify()                                   │
│    update-docs → run_update_docs()  ◄── NEW                  │
│                                                              │
│  ── all phases complete ──                                   │
│                                                              │
│  update_totals() → commit_coda_state() → create_pr()        │
└──────────────────────────────────────────────────────────────┘
```

**Session scope**: `run_update_docs()` uses the main Claude session (not an isolated session) so the agent retains full context of the changes it made during dev/review/verify phases. This context is essential for writing meaningful README updates.

**Commit scope**: After the agent updates the files, the runner commits `.coda.md` and `README.md` at the worktree root. These changes become part of the feature branch and are included in the PR.

**Retry strategy**: Reuse `config.agent.max_retries`. On each attempt, send the update-docs prompt, then validate that both `.coda.md` and `README.md` exist and are non-empty in the worktree. If validation fails and retries remain, send a fix prompt. If all attempts are exhausted, return `CoreError` to abort the run (no PR created).

**Failure behavior**: Hard failure — if the doc update cannot succeed after retries, `run_update_docs()` returns `Err(CoreError)`, which propagates through the `execute()` loop and aborts before `create_pr()`.

## Interface Design

**New `Task` variant** (`crates/coda-core/src/task.rs`):

```rust
/// Update `.coda.md` and `README.md` after all code phases complete.
UpdateDocs {
    /// URL-safe feature slug.
    feature_slug: String,
},
```

**New runner method** (`crates/coda-core/src/runner.rs`):

```rust
/// Regenerates `.coda.md` and updates `README.md` in the worktree.
///
/// Sends the `run/update_docs` prompt to the agent, then validates
/// that both files exist and are non-empty. Retries up to
/// `config.agent.max_retries` times on failure.
///
/// # Errors
///
/// Returns `CoreError::AgentError` if all retry attempts fail.
async fn run_update_docs(&mut self, phase_idx: usize) -> Result<TaskResult, CoreError>
```

**Phase dispatch** — new match arm in `execute()` loop:

```rust
PhaseKind::Quality => match phase_name.as_str() {
    "review" => self.run_review(phase_idx).await,
    "verify" => self.run_verify(phase_idx).await,
    "update-docs" => self.run_update_docs(phase_idx).await,
    _ => Err(CoreError::AgentError(format!(
        "Unknown quality phase: {phase_name}"
    ))),
},
```

**Commit helper** — new method on `Runner`:

```rust
/// Commits updated documentation files (`.coda.md`, `README.md`) in the worktree.
fn commit_doc_updates(&self) -> Result<(), CoreError> {
    let msg = format!("docs({}): update .coda.md and README.md", self.state.feature.slug);
    commit_coda_artifacts(
        self.git.as_ref(),
        &self.worktree_path,
        &[".coda.md", "README.md"],
        &msg,
    )
}
```

## Directory Structure

```
crates/coda-core/src/
├── task.rs              # Add Task::UpdateDocs variant
├── state.rs             # Bump MIN_PHASE_COUNT 3 → 4
├── planner.rs           # Add "update-docs" to QUALITY_PHASES
├── runner.rs            # Add run_update_docs(), commit_doc_updates(), dispatch arm
├── scanner.rs           # Update test fixtures (phase counts)
└── engine.rs            # Update test fixtures (phase counts)

crates/coda-pm/templates/run/
└── update_docs.j2       # NEW — prompt template for doc regeneration
```

## Core Data Structures

No new structs or enums beyond the `Task::UpdateDocs` variant. The phase uses existing `PhaseRecord` with `PhaseKind::Quality` and name `"update-docs"`.

**Constants changed:**

| Constant | File | Old | New |
|----------|------|-----|-----|
| `QUALITY_PHASES` | `planner.rs:62` | `&["review", "verify"]` | `&["review", "verify", "update-docs"]` |
| `MIN_PHASE_COUNT` | `state.rs:72` | `3` | `4` |

## Development Phases

### Phase 1: Core Pipeline Wiring
- **Goal**: Make the pipeline recognize and dispatch `update-docs` as a quality phase. Code compiles and tests pass with a stub implementation.
- **Tasks**:
  - Add `Task::UpdateDocs { feature_slug: String }` variant to `crates/coda-core/src/task.rs`
  - Add `"update-docs"` to `QUALITY_PHASES` in `crates/coda-core/src/planner.rs` (line 62)
  - Bump `MIN_PHASE_COUNT` from `3` to `4` in `crates/coda-core/src/state.rs` (line 72), update the error message to include `"update-docs"`
  - Add `"update-docs"` match arm in the `PhaseKind::Quality` dispatch in `crates/coda-core/src/runner.rs` (around line 881), calling a stub `run_update_docs()` that immediately returns `TaskStatus::Completed`
  - Add the stub `run_update_docs()` method on `Runner`
  - Update **all test fixtures** across the codebase that construct `FeatureState` with phase lists to include the new `update-docs` phase record:
    - `crates/coda-core/src/state.rs` — tests: `test_should_round_trip_feature_state`, `test_should_validate_correct_state`, `test_should_reject_too_few_phases` (now needs 4), `test_should_reject_path_traversal_in_worktree`
    - `crates/coda-core/src/planner.rs` — assertion indices for quality phase positions (lines 728-729 shift by one, new assertion for `update-docs`)
    - `crates/coda-core/src/scanner.rs` — test fixture phase lists (around line 255)
    - `crates/coda-core/src/engine.rs` — test fixture phase lists (around line 1242)
  - Run `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`
- **Commit message**: `"feat(runner): wire update-docs quality phase into run pipeline"`

### Phase 2: Prompt Template
- **Goal**: Create the prompt template that instructs the agent to regenerate `.coda.md` and update `README.md`.
- **Tasks**:
  - Create `crates/coda-pm/templates/run/update_docs.j2` with the following structure:
    - Context section: design spec, feature slug, diff summary
    - Task 1: Regenerate `.coda.md` — instruct agent to read the full codebase structure, tech stack, directory layout, and produce a fresh `.coda.md` following the established format (Project Overview, Architecture diagram, Directory Guide, Tech Stack, Development Workflow, Key Patterns). Overwrite the existing file completely.
    - Task 2: Update `README.md` — instruct agent to read the current `README.md`, understand what this feature changed (from design spec + agent context), and update relevant sections. If `README.md` doesn't exist, create a basic one.
    - Task 3: Commit changes with `docs: update .coda.md and README.md`
    - Validation: instruct agent to verify both files exist and are well-formatted
  - Template variables: `design_spec`, `state` (for feature context/slug)
  - Verify template renders correctly by checking `PromptManager` auto-discovers it (templates in `run/` are auto-discovered by glob)
  - Run `cargo build`, `cargo test`
- **Commit message**: `"feat(pm): add update_docs prompt template for doc regeneration phase"`

### Phase 3: Full Implementation with Retry Logic
- **Goal**: Replace the stub with the real `run_update_docs()` implementation including retry and validation.
- **Tasks**:
  - Implement `run_update_docs()`:
    - Call `self.mark_phase_running(phase_idx)`
    - Create `PhaseMetricsAccumulator`
    - Load design spec via `self.load_spec("design.md")`
    - Render `run/update_docs` template with `design_spec` and `state`
    - Retry loop `for attempt in 0..=max_retries`:
      - Send prompt via `self.send_and_collect(&prompt, None)` (main session)
      - Record metrics
      - Validate: check `self.worktree_path.join(".coda.md")` and `self.worktree_path.join("README.md")` both exist and are non-empty
      - If valid: `break`
      - If invalid and retries remain: send fix prompt describing what's missing
      - If invalid and no retries left: return `Err(CoreError::AgentError(...))`
    - Call `self.commit_doc_updates()` to stage and commit the doc files
    - Call `self.complete_phase(phase_idx, outcome)`
    - Return `TaskResult` with `Task::UpdateDocs`
  - Implement `commit_doc_updates()` helper method on `Runner`
  - Run `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`
- **Commit message**: `"feat(runner): implement update-docs phase with retry and validation"`

## Dependencies

No new crate dependencies. All required functionality (`std::fs`, `minijinja::context!`, `commit_coda_artifacts`, `PhaseMetricsAccumulator`) already exists in the codebase.

## Risk & Trade-offs

| Risk | Severity | Mitigation |
|------|----------|------------|
| **Increased cost per run** — adds one more agent call with full codebase scan for `.coda.md` regeneration | Low | Acceptable trade-off for always-current docs; single-phase cost is modest |
| **Hard abort on failure** — doc update failure blocks PR creation | Medium | Retry logic with `max_retries` attempts; in practice, file regeneration is a reliable agent task |
| **Large prompt context** — using main session means the agent carries all prior phase context | Low | The context is beneficial for README updates; `.coda.md` regeneration is codebase-driven regardless |
| **Worktree-scoped updates** — docs are updated in the feature worktree, not main | None (desired) | Changes merge via PR, which is the correct workflow |
| **Test fixture churn** — many tests construct phase lists that need a 4th phase | Low | One-time update; straightforward mechanical change |