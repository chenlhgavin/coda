

# Verification Plan: release-to-cratesio

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] All new tests pass
- [ ] No regression in existing tests (`cargo test` — all existing tests green)
- [ ] `cargo doc --no-deps` builds without warnings for all three crates

## Functional Verification

### Template Embedding

- [ ] Scenario 1: `with_builtin_templates()` loads all 12 templates
  - Input: Call `PromptManager::with_builtin_templates()`
  - Expected: Returns `Ok(pm)` where `pm` contains exactly 12 templates with names matching: `init/analyze_repo`, `init/system`, `init/setup_project`, `plan/system`, `plan/approve`, `plan/verification`, `run/system`, `run/dev_phase`, `run/review`, `run/verify`, `run/resume`, `run/create_pr`

- [ ] Scenario 2: Embedded templates are renderable (not empty or corrupt)
  - Input: Call `pm.render("init/system", context)` with minimal valid context for each of the 12 templates
  - Expected: Each render returns `Ok(content)` with non-empty output

- [ ] Scenario 3: `extra_dirs` overlay still works after refactor
  - Input: Create a temp dir with a custom `init/system.j2`, configure it as an `extra_dirs` entry, initialize Engine
  - Expected: The custom template overrides the built-in one; other built-in templates remain available

- [ ] Scenario 4: Engine initializes without `CARGO_MANIFEST_DIR` source tree
  - Input: `cargo install --path apps/coda-cli --root /tmp/coda-test-install`, then run `/tmp/coda-test-install/bin/coda` from an unrelated directory
  - Expected: Binary starts without template-related errors; built-in prompts are functional

### Publish Metadata

- [ ] Scenario 5: `cargo publish --dry-run -p coda-pm` succeeds
  - Input: Run dry-run publish
  - Expected: Exits 0, no errors about missing fields or files

- [ ] Scenario 6: `cargo publish --dry-run -p coda-core` succeeds
  - Input: Run dry-run publish
  - Expected: Exits 0, no errors about missing fields or files

- [ ] Scenario 7: `cargo publish --dry-run -p coda-cli` succeeds
  - Input: Run dry-run publish
  - Expected: Exits 0, no errors about missing fields or files

### Package Content

- [ ] Scenario 8: `coda-pm` package includes template files
  - Input: `cargo package -p coda-pm --list`
  - Expected: Output includes all 12 `templates/**/*.j2` files, `src/**/*.rs` files, and `Cargo.toml`

- [ ] Scenario 9: Packaged crates do not contain unwanted files
  - Input: `cargo package -p coda-cli --list`, `cargo package -p coda-core --list`
  - Expected: No `.trees/`, `specs/`, `docs/`, `vendors/`, `.coda/`, or test fixtures leaked into packages

### Binary Installation

- [ ] Scenario 10: Full end-to-end `cargo install` from local path
  - Input: `cargo install --path apps/coda-cli --root /tmp/coda-e2e`
  - Expected: Installs successfully; `/tmp/coda-e2e/bin/coda --help` prints usage; binary name is `coda`

## Edge Cases

- [ ] `PromptManager::with_builtin_templates()` is called multiple times — no panic or duplication issues
- [ ] A template `.j2` file in `extra_dirs` with a name NOT in the built-in set is loaded as an addition (not just overrides)
- [ ] `extra_dirs` pointing to a non-existent directory does not cause failure (existing behavior preserved)
- [ ] `extra_dirs` is empty (default config) — only built-in templates are loaded, no errors
- [ ] `env!("CARGO_MANIFEST_DIR")` is no longer referenced anywhere in `coda-core` or `coda-pm` source (grep verification)
- [ ] Template count test fails if a new `.j2` file is added to `templates/` but not to `builtin.rs` (guard against drift)

## Integration Points

- [ ] `Engine::new()` in `coda-core` uses `with_builtin_templates()` and all downstream consumers (`PlanSession`, `Runner`, `Reviewer`) receive a fully loaded `PromptManager`
- [ ] Existing `PromptManager::new()` (empty constructor) still works for unit tests that add templates manually
- [ ] `loader::load_templates_from_dir()` is still available and used by `extra_dirs` loading path
- [ ] Makefile `publish-dry-run` and `publish` targets are callable and produce expected behavior
- [ ] `vendors/claude-agent-sdk-rs` has `publish = false` and is NOT included in any publish operation

## Security

- [ ] `LICENSE.md` copyright holder is updated correctly and matches the `license` field in Cargo.toml
- [ ] No API tokens, secrets, or credentials are included in any packaged crate (`cargo package -p <crate> --list` audit)
- [ ] `.coda/config.yml` (which may contain user-specific config) is not included in any package