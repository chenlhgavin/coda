

# Verification Plan: update-readme

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] No regression in existing tests (`cargo test`)
- [ ] Markdown renders correctly (no broken syntax)

## Functional Verification

### Section Presence

- [ ] Scenario 1: All required sections exist in README.md
  - Input: Read the final `README.md`
  - Expected: The following sections are all present as headings: project title/tagline, "What is CODA", Prerequisites, Installation, Quick Start (with all 6 subcommand steps), CLI Reference, Configuration, How It Works, Development, License

- [ ] Scenario 2: Placeholder content is fully removed
  - Input: Search `README.md` for leftover scaffold content
  - Expected: No references to `rust-lib-template`, `cargo generate`, or the old build badge URL (`github.com/tyrchen/rust-lib-template/workflows/build/badge.svg`)

### CLI Reference Accuracy

- [ ] Scenario 3: All CLI subcommands are documented
  - Input: Compare commands listed in README against `Commands` enum in `apps/coda-cli/src/cli.rs`
  - Expected: Exactly 6 commands documented: `init`, `plan`, `run`, `list`, `status`, `clean` — no extras, no omissions

- [ ] Scenario 4: CLI argument names and flags match source
  - Input: Compare each command's arguments/flags in README against the struct fields in `cli.rs:27-62`
  - Expected:
    - `plan` and `run` and `status` each take a `feature_slug` positional argument
    - `clean` has `--dry-run` (long only) and `--yes` / `-y` (long + short) flags
    - `init` and `list` take no arguments

- [ ] Scenario 5: Feature slug rules match validation logic
  - Input: Compare slug rules in README against `validate_feature_slug` in `crates/coda-core/src/engine.rs:546-577`
  - Expected: README states all of: lowercase ASCII + digits + hyphens only, no leading/trailing hyphen, no consecutive hyphens, max 64 characters

### Configuration Accuracy

- [ ] Scenario 6: All config fields are documented
  - Input: Compare config sections in README against structs in `crates/coda-core/src/config.rs`
  - Expected: README documents all fields from `CodaConfig`, `AgentConfig`, `PromptsConfig`, `GitConfig`, `ReviewConfig` — specifically: `version`, `agent.model`, `agent.max_budget_usd`, `agent.max_retries`, `agent.max_turns`, `checks`, `prompts.extra_dirs`, `git.auto_commit`, `git.branch_prefix`, `git.base_branch`, `review.enabled`, `review.max_review_rounds`

- [ ] Scenario 7: Default values in README match code defaults
  - Input: Compare default values shown in README config example against `Default` implementations in `config.rs:102-155`
  - Expected:
    - `model`: `"claude-opus-4-6"`
    - `max_budget_usd`: `20.0`
    - `max_retries`: `3`
    - `max_turns`: `100`
    - `checks`: `["cargo build", "cargo +nightly fmt -- --check", "cargo clippy -- -D warnings"]`
    - `extra_dirs`: `[".coda/prompts"]`
    - `auto_commit`: `true`
    - `branch_prefix`: `"feature"`
    - `base_branch`: `"auto"`
    - `review.enabled`: `true`
    - `max_review_rounds`: `5`

### Example Output Accuracy

- [ ] Scenario 8: `coda init` output matches source
  - Input: Compare init success messages in README against `app.rs:62-69`
  - Expected: README shows the same artifact messages (`.coda/` directory, `.trees/` directory, `.coda.md` overview) and the "Next step" hint

- [ ] Scenario 9: `coda run` output format matches RunEvent handling
  - Input: Compare run output in README against the display logic in `app.rs:328-373`
  - Expected: Output uses the same structure — header with `═`, pipeline line with `→`, phase lines with `[▸]`/`[✓]`/`[✗]` prefixes, PR creation line, total summary line with `─` separator

- [ ] Scenario 10: `coda plan` output matches PlanOutput fields
  - Input: Compare plan completion output in README against `app.rs:93-101` and `PlanOutput` struct in `planner.rs:25-38`
  - Expected: Shows design_spec path, verification path, state path, and worktree path

- [ ] Scenario 11: `coda list` output format matches source
  - Input: Compare list table in README against `app.rs:128-149`
  - Expected: Table has columns: Feature, Status, Branch, Turns, Cost — with status icons

- [ ] Scenario 12: `coda status` output format matches source
  - Input: Compare status output in README against `app.rs:167-232`
  - Expected: Shows feature name, status with icon, created/updated timestamps, git info (branch, base, worktree), phases table (phase, status, turns, cost, duration), PR info section, summary with totals

- [ ] Scenario 13: `coda clean` output matches source
  - Input: Compare clean output in README against `app.rs:246-306`
  - Expected: Shows scanning message, candidate list with `[~]` prefix and PR state, confirmation prompt, removal list with `[✓]` prefix, final count

### Architecture & Structure

- [ ] Scenario 14: Architecture diagram is present and accurate
  - Input: Compare diagram in README against `.coda.md` architecture section
  - Expected: Diagram shows the 4-layer structure: coda-cli → coda-core → (coda-pm + claude-agent-sdk-rs)

- [ ] Scenario 15: Workspace structure table matches actual directories
  - Input: Compare directory table in README against actual workspace layout
  - Expected: Lists `apps/coda-cli`, `crates/coda-core`, `crates/coda-pm`, `vendors/claude-agent-sdk-rs`, `specs/`, `docs/` — all of which exist in the repo

### Prerequisites & Installation

- [ ] Scenario 16: Prerequisites list is complete
  - Input: Review prerequisites section
  - Expected: Documents Rust toolchain (with nightly note), git (with worktree version note), gh CLI (with auth note), and Anthropic API key / Claude CLI auth

- [ ] Scenario 17: Build instructions produce a working binary
  - Input: Follow the installation steps in the README
  - Expected: `cargo build --release` succeeds, binary exists at `target/release/coda-cli`

## Edge Cases

- [ ] No broken Markdown links — all internal references (`.coda.md`, `CLAUDE.md`, `LICENSE.md`, `specs/`) point to files that actually exist in the repo
- [ ] No raw HTML or non-standard Markdown — content renders correctly in GitHub's Markdown renderer
- [ ] Code blocks all have appropriate language annotations (`bash`, `yaml`, or plain text for ASCII art)
- [ ] The annotated config YAML example in the README is valid YAML (comments don't break parsing)
- [ ] No placeholder text like "TODO", "TBD", "FIXME" remains in the final document

## Integration Points

- [ ] README references to `.coda.md` are consistent — "See `.coda.md` for the full repository overview" links to a file that exists and covers architecture in detail
- [ ] README references to `CLAUDE.md` are consistent — "See `CLAUDE.md` for coding conventions" links to a file that exists and covers development guidelines
- [ ] README license section matches `Cargo.toml` workspace metadata (`license = "MIT"`) and references `LICENSE.md`
- [ ] README does not contradict any information in `.coda.md` or `CLAUDE.md` (e.g., build commands, toolchain requirements, project description)

## Security (if applicable)

- [ ] No real API keys, tokens, or credentials appear in example output or configuration samples
- [ ] No real GitHub URLs appear in example output (use placeholder like `https://github.com/org/repo/pull/42`)
- [ ] No real usernames or email addresses are introduced (existing copyright attribution from `Cargo.toml` is acceptable)