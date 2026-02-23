

# Verification Plan: slack-repo

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
- [ ] No dead code warnings from removed `chrono` usage

## Functional Verification

### Stable Clone Paths

- [ ] Scenario 1: Select a repo with owner prefix
  - Input: `/coda repos` → select `myorg/my-repo`, workspace is `/home/user/workspace`
  - Expected: Clone path is `/home/user/workspace/myorg/my-repo` (no timestamp suffix)

- [ ] Scenario 2: Select the same repo a second time
  - Input: `/coda repos` → select `myorg/my-repo` again (directory already exists with `.git`)
  - Expected: No new directory created; existing clone is reused and updated; Slack message says "Updated and bound"

- [ ] Scenario 3: Select a different repo
  - Input: `/coda repos` → select `myorg/other-repo`
  - Expected: New clone at `/home/user/workspace/myorg/other-repo`; Slack message says "Cloned and bound"

### Auto-Update on Bind

- [ ] Scenario 4: Existing clone is behind remote
  - Input: Existing clone at stable path, remote has new commits on default branch
  - Expected: `git fetch origin` runs, default branch is checked out, `git pull` brings it up to date, binding succeeds

- [ ] Scenario 5: Existing clone has dirty working directory
  - Input: Existing clone with uncommitted changes from a prior `coda init`
  - Expected: `git checkout --force` overrides local changes, update completes, binding succeeds

- [ ] Scenario 6: Fast-forward pull fails, rebase fallback
  - Input: Existing clone where local default branch has diverged from remote (e.g., local commits from a prior init that were not pushed)
  - Expected: `git pull --ff-only` fails, `git pull --rebase` succeeds, binding succeeds

- [ ] Scenario 7: Fresh clone (no existing directory)
  - Input: Stable path does not exist yet
  - Expected: `gh repo clone` runs as before, Slack message says "Cloned and bound"

### Slack Messages

- [ ] Scenario 8: Clone success message content
  - Input: Fresh clone of `myorg/my-repo`
  - Expected: Slack message contains "Cloned and bound `myorg/my-repo`" with the stable path

- [ ] Scenario 9: Update success message content
  - Input: Reuse existing clone of `myorg/my-repo`
  - Expected: Slack message contains "Updated and bound `myorg/my-repo`" with the stable path

- [ ] Scenario 10: Update failure message
  - Input: Existing clone where `git fetch origin` fails (e.g., network error)
  - Expected: Slack message displays a clear error; channel is NOT bound to a broken repo

## Edge Cases

- [ ] Plain repo name without owner prefix (e.g., `my-repo` instead of `org/my-repo`) produces `<workspace>/my-repo`
- [ ] Repo name with deeply nested owner (e.g., `deep-org/sub-repo`) produces `<workspace>/deep-org/sub-repo`
- [ ] Existing directory at clone path that is NOT a git repo (missing `.git`) triggers a fresh clone (or a clear error), not a blind `git fetch`
- [ ] Workspace directory does not exist yet — parent directories are created via `create_dir_all` as before
- [ ] `git symbolic-ref` fails to detect default branch — fallback to `main` is used
- [ ] `git pull --rebase` also fails after `--ff-only` fails — error is reported to Slack, binding does not proceed
- [ ] Multiple channels bind to the same repo path — both bindings work; second bind triggers update, not clone

## Integration Points

- [ ] `handle_repo_clone()` still correctly calls `state.bindings().set()` after both clone and update paths
- [ ] `BindingStore::set()` validation (`is_dir()` check) passes for the new stable path structure
- [ ] Binding persistence to `~/.coda-server/config.yml` records the stable path correctly
- [ ] `resolve_engine()` in `commands/mod.rs` correctly creates an `Engine` from the stable-path binding (no assumption about path format)
- [ ] `handle_init()` works correctly when run after a bind that used the update path (repo is up-to-date, `.coda/` may or may not exist)
- [ ] `handle_switch()` continues to work — its `switch_branch()` function is independent of clone path format
- [ ] Existing bindings pointing to old timestamped paths in `config.yml` continue to function without errors
- [ ] Interaction handler in `handlers/interactions.rs` routes `REPO_SELECT_ACTION` to the modified `handle_repo_clone` without changes

## Performance (if applicable)

- [ ] Reusing an existing clone (update path) is faster than a full re-clone — no large network transfer for the initial clone
- [ ] The update sequence (`fetch → checkout → pull`) completes within a reasonable time for typical repositories and does not cause Slack interaction timeout (3-second acknowledgment is already handled by the async spawn pattern)

## Security (if applicable)

- [ ] `git checkout --force` only applies to CODA-managed directories under the configured workspace — no risk of overwriting user files outside the workspace
- [ ] The `repo_name` value from the Slack select menu is passed to `gh repo clone` and used in filesystem paths — verify no path traversal is possible (e.g., a crafted `nameWithOwner` like `../../etc/something`). The GitHub API `nameWithOwner` format is `owner/repo` and validated by `gh`, but worth confirming the path stays under workspace