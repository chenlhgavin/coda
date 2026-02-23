

# Task: slack-repo

## Overview

The Slack `/coda repos` flow creates a new timestamped clone directory every time a user selects a repository, causing workspace clutter. Additionally, the bound repository's default branch is never updated before `coda init`, so the init pipeline may analyze stale code.

This task replaces the timestamped clone path scheme with a stable `<workspace>/<owner>/<repo>` path that is reused across selections, and adds an automatic `fetch → checkout → pull` step on every bind so the default branch is always current before downstream commands run.

## Root Cause Analysis

1. **Directory clutter**: `build_clone_path()` at `repos.rs:271` appends `<repo>-<YYYYMMDD-HHMMSS>` to every clone path. Each repo selection produces a unique directory, even for the same repository. The config doc comment at `config.rs:46` already describes the intended `<workspace>/github.com/<owner>/<repo>` scheme, but the implementation diverges.

2. **Stale default branch**: `handle_repo_clone()` at `repos.rs:147` clones and binds but performs no `git fetch/checkout/pull`. When `coda init` runs later via `handle_init()`, it operates on whatever commit the local default branch happens to be at.

## Proposed Solution

### Stable clone paths

Replace `build_clone_path()` to produce `<workspace>/<owner>/<repo>` using both segments of the `nameWithOwner` field from the GitHub API (e.g., `myorg/my-repo` → `<workspace>/myorg/my-repo`). For plain names without an owner prefix, use the name as a single-level directory.

### Clone-or-reuse with auto-update on bind

In `handle_repo_clone()`, before calling `clone_repo()`:

- If the target path already exists and contains `.git`, skip cloning and run an update sequence instead.
- The update sequence: `git fetch origin` → detect default branch → `git checkout <branch>` → `git pull --ff-only`, falling back to `git pull --rebase` if fast-forward fails (satisfying the auto-merge requirement).
- If the target path does not exist, clone as before (the default branch is already checked out after a fresh clone).

Update Slack messages to distinguish "Cloned and bound" from "Updated and bound".

## Affected Areas

| File | Change |
|------|--------|
| `apps/coda-server/src/commands/repos.rs` | Replace `build_clone_path` with stable path scheme; add `update_repo` async fn; modify `handle_repo_clone` to reuse existing clones and update before binding; remove `chrono::Local` import |
| `apps/coda-server/Cargo.toml` | Remove `chrono` dependency if it was only used for the timestamp in `build_clone_path` |

## Development Phases

### Phase 1: Stable clone path
- **Goal**: Eliminate directory clutter by producing deterministic, reusable clone paths
- **Tasks**:
  - Rewrite `build_clone_path()` to produce `<workspace>/<owner>/<repo>` from the `nameWithOwner` string. For plain names without `/`, use `<workspace>/<name>`.
  - Remove the `chrono::Local` import and the timestamp logic.
  - Check `apps/coda-server/Cargo.toml` — if `chrono` is no longer used elsewhere in the crate, remove the dependency.
  - Update all three existing unit tests (`test_should_build_clone_path_with_timestamp`, `test_should_build_clone_path_without_owner_prefix`, `test_should_build_clone_path_plain_name`) to assert the new path format. Rename them to reflect the new behavior (e.g., `test_should_build_clone_path_with_owner`).
- **Commit message**: `refactor(server): use stable clone path without timestamp`

### Phase 2: Clone-or-reuse with auto-update
- **Goal**: Reuse existing clones and ensure the default branch is current before binding
- **Tasks**:
  - Add `update_repo(repo_path: &Path) -> Result<String, String>` async fn that performs:
    1. `git fetch origin`
    2. Detect default branch via `git symbolic-ref refs/remotes/origin/HEAD --short` (strip `origin/` prefix, fallback to `main`)
    3. `git checkout --force <default-branch>` (force handles dirty working directory in CODA-managed clones)
    4. `git pull --ff-only`; if that fails, `git pull --rebase`
    5. Returns the branch name on success for display purposes
  - Modify `handle_repo_clone()`:
    - After computing `clone_path`, check if `clone_path.join(".git").exists()`.
    - If yes: call `update_repo(&clone_path)` instead of `clone_repo()`. On success, use "Updated and bound" in the Slack message.
    - If no: call `clone_repo()` as before. Use "Cloned and bound" in the Slack message.
  - Add unit tests for `update_repo` error paths (e.g., test that the function returns an error string on git command failure).
- **Commit message**: `feat(server): reuse existing clones and auto-update default branch on bind`

### Phase 3: Build & verify
- **Goal**: Ensure everything compiles and passes lint/test
- **Tasks**:
  - Run `cargo build`
  - Run `cargo test`
  - Run `cargo +nightly fmt`
  - Run `cargo clippy -- -D warnings`
- **Commit message**: N/A (verification only, no code changes expected)

## Risk & Trade-offs

- **Dirty working directory in existing clones**: If a previous `coda init` or manual edit left uncommitted changes, `git checkout` could fail. Using `git checkout --force` handles this since these are CODA-managed clones where preserving local edits is not expected. If a stricter approach is preferred later, this can be revisited.
- **Concurrent bind to the same repo**: The existing `RepoLocks` mechanism is not used during `handle_repo_clone`. If two channels try to bind the same repo path simultaneously, `clone_repo` or `update_repo` could race. This is a pre-existing gap; adding repo lock acquisition before clone-or-update would mitigate it but is out of scope for this task.
- **Existing bindings**: Config entries pointing to old timestamped paths will continue to work — those directories still exist on disk. New selections will use the stable path. No migration is needed. Users can manually delete old timestamped directories at their convenience.
- **`chrono` removal**: If `chrono` is used elsewhere in `coda-server` (e.g., formatting timestamps for other Slack messages), it should be kept. Phase 1 must verify before removing.