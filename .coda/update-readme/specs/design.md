

# Task: update-readme

## Overview

Replace the placeholder `README.md` — currently a leftover from the `rust-lib-template` scaffold with a generic description, a stale build badge, and `cargo generate` instructions — with a comprehensive README that accurately describes CODA as it exists today. The new README will serve both end users (developers using CODA on their projects) and contributors, featuring a full end-to-end walkthrough, CLI reference, configuration guide, and contributor quickstart.

## Target Audience

- **Primary**: Developers who want to use CODA to orchestrate AI-driven feature development on their own repositories. They need to understand prerequisites, installation, and the full init → plan → run → list → status → clean workflow.
- **Secondary**: Contributors who want to build, test, and extend CODA itself. They need workspace structure, build commands, and pointers to conventions and design documents.

## Content Outline

### 1. Header & Tagline
- `# CODA — Claude Orchestrated Development Agent`
- One-line description: "An AI-driven CLI that orchestrates the full feature development lifecycle: analyze → plan → implement → review → PR."
- Remove the stale `rust-lib-template` build badge entirely. No replacement badges for now.

### 2. What is CODA? (2-3 paragraphs)
- **What it does**: Orchestrates feature development using Claude AI. You describe a feature, CODA plans it interactively via a TUI, then executes the full development cycle — coding, reviewing, verifying, and opening a PR — autonomously.
- **How it works**: Each feature gets an isolated git worktree and branch. Execution proceeds through dynamic development phases (derived from the design spec), followed by fixed review → verify quality phases. State is persisted to `state.yml` so interrupted runs can resume from the last completed phase.
- **Key value**: Turns a feature description into a reviewed, tested pull request with minimal human intervention. Parallel features via worktrees, cost tracking per phase, configurable quality checks.

### 3. Prerequisites
Document all required external tools:

| Tool | Purpose | Notes |
|------|---------|-------|
| **Rust toolchain** | Build CODA | Pinned via `rust-toolchain.toml`; nightly required for `cargo fmt` |
| **git** | Worktree management, branching | Must support `git worktree` (Git 2.5+) |
| **gh CLI** | PR creation, PR status checks | Must be authenticated (`gh auth login`) |
| **Anthropic API key** | Claude AI access | Set `ANTHROPIC_API_KEY` env var, or authenticate via Claude CLI |

### 4. Installation (Build from Source)
Steps:
1. Clone the repository
2. `cargo build --release`
3. Binary at `target/release/coda-cli` — add to PATH or symlink
4. Alternative: `cargo install --path apps/coda-cli`
5. Verify: `coda --help`

### 5. Quick Start — End-to-End Example
Walk through a complete feature lifecycle using a realistic example (`add-user-auth`). Each step shows the command and representative output.

**Step 1: `coda init`**
- What it does: analyzes repo structure via Claude, creates `.coda/config.yml`, `.coda.md`, `.trees/`, updates `.gitignore`
- Show command and sample success output from `app.rs:57-69`

**Step 2: `coda plan add-user-auth`**
- What it does: opens interactive TUI for multi-turn planning conversation with the Planner agent
- Mention the TUI interaction model, `/done` to finalize
- Show the artifacts produced: design spec path, verification plan path, state.yml path, worktree path (from `PlanOutput` struct)

**Step 3: `coda run add-user-auth`**
- What it does: executes all phases in the worktree — dev phases → review → verify → PR
- Show realistic sample output matching the format in `app.rs:331-373`:
  ```
  CODA Run: add-user-auth
  ═══════════════════════════════════════

  Phases: auth-types → auth-middleware → review → verify → PR

  [▸] auth-types              Running...  (1/4)
  [✓] auth-types              5m 12s    3 turns  $0.1200
  [▸] auth-middleware          Running...  (2/4)
  [✓] auth-middleware          12m 03s   12 turns  $1.8500
  [▸] review                  Running...  (3/4)
  [✓] review                  3m 45s    5 turns  $0.5200
  [▸] verify                  Running...  (4/4)
  [✓] verify                  2m 30s    4 turns  $0.3800
  [▸] create-pr              Running...
  [✓] create-pr              PR: https://github.com/org/repo/pull/42

  ─────────────────────────────────────
  Total: 23m 30s elapsed, 24 turns, $2.8700 USD
  ═══════════════════════════════════════
  ```

**Step 4: `coda list`**
- Show the table format from `app.rs:128-149`

**Step 5: `coda status add-user-auth`**
- Show the detailed status output from `app.rs:167-232` including phases table, PR info, cost summary

**Step 6: `coda clean`**
- Show scanning output, confirmation prompt, and removal output from `app.rs:246-306`
- Mention `--dry-run` and `--yes` flags

### 6. CLI Reference
Compact table of all 6 commands derived from `cli.rs`:

| Command | Description | Arguments / Flags |
|---------|-------------|-------------------|
| `coda init` | Initialize repo as a CODA project | — |
| `coda plan <slug>` | Interactive feature planning | `slug`: lowercase, digits, hyphens, max 64 chars |
| `coda run <slug>` | Execute feature development | `slug`: feature to run |
| `coda list` | List all features with status | — |
| `coda status <slug>` | Detailed feature status | `slug`: feature to inspect |
| `coda clean` | Remove merged/closed worktrees | `--dry-run`, `--yes` / `-y` |

Feature slug rules (from `validate_feature_slug` in `engine.rs:546-577`):
- Lowercase ASCII letters, digits, and hyphens only
- Must not start or end with a hyphen
- No consecutive hyphens
- Maximum 64 characters

### 7. Configuration
Location: `.coda/config.yml` (created by `coda init`).

Show a complete annotated example config matching the actual defaults from `config.rs`:

```yaml
version: 1

agent:
  model: "claude-opus-4-6"       # Claude model to use
  max_budget_usd: 20.0           # Max spend per `coda run`
  max_retries: 3                 # Retries per phase on failure
  max_turns: 100                 # Max agent turns per phase

checks:                          # Commands run after each phase
  - "cargo build"
  - "cargo +nightly fmt -- --check"
  - "cargo clippy -- -D warnings"

prompts:
  extra_dirs:                    # Custom prompt template dirs
    - ".coda/prompts"

git:
  auto_commit: true              # Commit after each phase
  branch_prefix: "feature"       # Branch: feature/<slug>
  base_branch: "auto"            # Auto-detect default branch

review:
  enabled: true                  # Enable code review phase
  max_review_rounds: 5           # Max review iterations
```

Document each section briefly, noting that all fields have sensible defaults and the config can be as minimal as `version: 1`.

### 8. How It Works
- Brief workflow description: `init` analyzes the repo → `plan` opens interactive design session → `run` executes dev phases + review + verify + PR
- Mention: git worktrees for isolation, `state.yml` for crash recovery/resume, configurable quality checks between phases
- Include the architecture diagram from `.coda.md`:
  ```
  ┌─────────────────────────────────────────────────────┐
  │                   coda-cli (app)                    │
  │  Ratatui TUI · Clap CLI · Line Editor · App Driver  │
  └──────────────────────┬──────────────────────────────┘
                         │
  ┌──────────────────────▼──────────────────────────────┐
  │                  coda-core (lib)                    │
  │  Engine · Planner · Runner · Reviewer               │
  │  FeatureScanner · GitOps · GhOps (traits)           │
  └──────────┬───────────────────────┬──────────────────┘
             │                       │
  ┌──────────▼──────────┐  ┌─────────▼──────────────────┐
  │    coda-pm (lib)    │  │  claude-agent-sdk-rs        │
  │  Prompt Manager     │  │  Claude AI client           │
  │  MiniJinja templates│  │  Streaming, hooks, MCP      │
  └─────────────────────┘  └────────────────────────────┘
  ```
- "See `.coda.md` for the full repository overview."

### 9. Development (for Contributors)
- Build / test / lint commands:
  ```bash
  cargo build                              # Build all crates
  cargo +nightly fmt                       # Format (requires nightly)
  cargo clippy -- -D warnings              # Lint
  cargo test                               # Run tests
  cargo nextest run                        # Run tests (nextest)
  cargo deny check                         # License/dependency policy
  cargo audit                              # Security audit
  ```
- Workspace structure table (condensed from `.coda.md` directory guide):

  | Path | Purpose |
  |------|---------|
  | `apps/coda-cli` | CLI binary — TUI, argument parsing, app logic |
  | `crates/coda-core` | Core library — engine, planner, runner, reviewer, state |
  | `crates/coda-pm` | Prompt manager — Jinja2 template loading and rendering |
  | `vendors/claude-agent-sdk-rs` | Vendored Claude Agent SDK |
  | `specs/` | Feature specs and design documents |
  | `docs/` | Project documentation |

- "See `CLAUDE.md` for coding conventions and development guidelines."

### 10. License
- MIT license
- Copyright 2025 Tyr Chen
- Link to `LICENSE.md`

## Development Phases

### Phase 1: Write the README
- **Goal**: Replace `README.md` with the full content per the outline above
- **Tasks**:
  - Write all 10 sections as specified
  - Use realistic example output that matches the actual CLI output format (cross-referenced with `app.rs`)
  - Ensure all CLI commands/flags match `cli.rs`, all config fields match `config.rs`, slug rules match `engine.rs`
  - Keep total length reasonable (~300-400 lines of markdown)
- **Commit message**: `"docs: rewrite README with user guide and end-to-end walkthrough"`

### Phase 2: Verify accuracy
- **Goal**: Cross-check every claim against the actual codebase
- **Tasks**:
  - Verify all 6 CLI subcommands and their flags against `apps/coda-cli/src/cli.rs`
  - Verify all config fields and defaults against `crates/coda-core/src/config.rs`
  - Verify feature slug validation rules against `crates/coda-core/src/engine.rs:546-577`
  - Verify output formats against `apps/coda-cli/src/app.rs`
  - Run `cargo build` to ensure nothing is broken
- **Commit message**: `"docs: verify README accuracy against codebase"`

## References

| Source | What to extract |
|--------|-----------------|
| `apps/coda-cli/src/cli.rs` | All subcommands, arguments, flags |
| `apps/coda-cli/src/app.rs` | CLI output formats for list, status, run, clean |
| `crates/coda-core/src/config.rs` | All config fields, types, and defaults |
| `crates/coda-core/src/engine.rs` | Feature slug validation rules, init flow, run flow |
| `crates/coda-core/src/state.rs` | Feature state model, phase types, status enums |
| `crates/coda-core/src/planner.rs` | PlanOutput struct (artifact paths) |
| `crates/coda-core/src/runner.rs` | RunEvent variants (progress output format) |
| `.coda.md` | Architecture diagram, directory guide |
| `.coda/config.yml` | Real-world config example |
| `rust-toolchain.toml` | Rust version pin |
| `Cargo.toml` | Workspace members, edition, license |

## Risk & Trade-offs

- **Example output drift**: The sample CLI output in the README is illustrative and won't auto-update when output format changes. Mitigated by keeping examples realistic but not overly specific — focus on structure, not exact spacing.
- **Authentication docs may be incomplete**: The exact auth mechanism for `claude-agent-sdk-rs` depends on the vendored SDK's behavior. The README will document `ANTHROPIC_API_KEY` as the primary method, with a note to check SDK docs. Flag during review if there's a different auth flow.
- **No binary distribution**: Only documents building from source per user preference. A future follow-up could add Homebrew, `cargo binstall`, or release binary sections.
- **Single file change**: This is a documentation-only task touching only `README.md`. Zero risk of breaking builds or tests.