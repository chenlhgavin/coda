

# Verification Plan: update-doc

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] No regression in existing tests (`cargo test`)

> Note: This is a documentation-only feature. The automated checks confirm that no source files were accidentally modified in a way that breaks compilation, formatting, or lint rules.

## Functional Verification

### File Existence & Location

- [ ] Scenario 1: `coda-pm/API.md` exists at the correct path
  - Input: Check for file at `crates/coda-pm/API.md`
  - Expected: File exists and is non-empty

- [ ] Scenario 2: `coda-core/API.md` exists at the correct path
  - Input: Check for file at `crates/coda-core/API.md`
  - Expected: File exists and is non-empty

- [ ] Scenario 3: `docs/index.md` updated with API.md convention
  - Input: Read `docs/index.md`
  - Expected: Contains a section referencing the per-crate `API.md` convention and maintenance expectations

### Template Conformance

- [ ] Scenario 4: `coda-pm/API.md` follows the standardized template
  - Input: Read the file
  - Expected: Contains `# coda-pm API Reference` heading, a `## Re-exports` section with a table, and a `## Modules` section with per-module subsections

- [ ] Scenario 5: `coda-core/API.md` follows the standardized template
  - Input: Read the file
  - Expected: Contains `# coda-core API Reference` heading, a `## Re-exports` section with a table, and a `## Modules` section with per-module subsections

### Completeness — `coda-pm`

- [ ] Scenario 6: All `coda-pm` re-exports are documented
  - Input: Compare re-exports in `crates/coda-pm/src/lib.rs` against the Re-exports table in `API.md`
  - Expected: `PromptManager`, `PromptTemplate`, and `PromptError` all appear in the table

- [ ] Scenario 7: All `coda-pm` public modules are documented
  - Input: Compare `pub mod` declarations in `lib.rs` against module sections in `API.md`
  - Expected: `loader` module has a dedicated subsection

### Completeness — `coda-core`

- [ ] Scenario 8: All `coda-core` re-exports are documented
  - Input: Compare `pub use` items in `crates/coda-core/src/lib.rs` against the Re-exports table in `API.md`
  - Expected: Every re-exported item appears (`Engine`, `CleanedWorktree`, `CodaConfig`, `CoreError`, `GitOps`, `DefaultGitOps`, `GhOps`, `DefaultGhOps`, `PrStatus`, `PlanSession`, `PlanOutput`, `AgentProfile`, `build_safety_hooks`, `find_project_root`, `ReviewResult`, `Runner`, `RunEvent`, `RunProgress`, `CommitInfo`, `ReviewSummary`, `VerificationSummary`, `FeatureScanner`, `Task`, `TaskResult`, `TaskStatus`)

- [ ] Scenario 9: All `coda-core` public modules are documented
  - Input: Compare `pub mod` declarations in `lib.rs` against module sections in `API.md`
  - Expected: Subsections exist for `config`, `gh`, `git`, `parser`, `planner`, `profile`, `project`, `reviewer`, `runner`, `scanner`, `state`, `task`

- [ ] Scenario 10: Complex traits use code blocks instead of tables
  - Input: Read the `mod git` and `mod gh` sections in `coda-core/API.md`
  - Expected: `GitOps` and `GhOps` trait definitions appear in fenced Rust code blocks with method signatures

### Accuracy

- [ ] Scenario 11: Item kinds are correct
  - Input: Spot-check 5+ items in each `API.md` against source code
  - Expected: Each item's "Kind" column (struct/trait/enum/fn) matches its actual definition in source

- [ ] Scenario 12: Descriptions are accurate
  - Input: Spot-check 5+ item descriptions against source doc comments or behavior
  - Expected: One-line descriptions accurately reflect the item's purpose

## Edge Cases

- [ ] No source files (`.rs`) were modified — only `.md` files changed
- [ ] Markdown tables render correctly (no broken pipe alignment, no missing header separators)
- [ ] Fenced Rust code blocks in `API.md` use correct syntax highlighting markers (` ```rust `)
- [ ] No stale items documented — every item listed in `API.md` actually exists as a public item in current source
- [ ] No private or `pub(crate)` items leak into `API.md`

## Integration Points

- [ ] `docs/index.md` links or references are consistent with the actual file paths (`crates/coda-core/API.md`, `crates/coda-pm/API.md`)
- [ ] The crate-level one-liner in each `API.md` matches the `//!` doc comment in the corresponding `lib.rs`
- [ ] Module names in `API.md` subsection headers match the actual `pub mod` names in `lib.rs`

## Performance

Not applicable — documentation-only change.

## Security

Not applicable — documentation-only change with no code modifications.