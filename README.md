# CODA — Claude Orchestrated Development Agent

An AI-driven CLI that orchestrates the full feature development lifecycle: analyze → plan → implement → review → PR.

## What is CODA?

CODA orchestrates feature development using Claude AI. You describe a feature, CODA plans it interactively via a TUI, then executes the full development cycle — coding, reviewing, verifying, and opening a pull request — autonomously.

Each feature gets an isolated git worktree and branch. Execution proceeds through dynamic development phases (derived from the design spec), followed by fixed review and verify phases. State is persisted to `state.yml` so interrupted runs can resume from the last completed phase.

CODA turns a feature description into a reviewed, tested pull request with minimal human intervention. It supports parallel features via worktrees, cost tracking per phase, and configurable quality checks.

## Prerequisites

| Tool | Purpose | Notes |
|------|---------|-------|
| **Rust toolchain** | Build CODA | Edition 2024; nightly required for `cargo fmt` |
| **git** | Worktree management, branching | Must support `git worktree` (Git 2.5+) |
| **gh CLI** | PR creation, PR status checks | Must be authenticated (`gh auth login`) |
| **Anthropic API key** | Claude AI access | Set `ANTHROPIC_API_KEY` env var, or authenticate via Claude CLI |

## Installation

Build from source:

```bash
git clone https://github.com/tyrchen/coda.git
cd coda
cargo build --release
```

The binary is at `target/release/coda`. Add it to your PATH or create a symlink.

Alternatively, install directly:

```bash
cargo install --path apps/coda-cli
```

Verify the installation:

```bash
coda --help
```

## Quick Start

Walk through a complete feature lifecycle using a realistic example.

### Step 1: Initialize the project

```bash
coda init
```

```
Initializing CODA in /path/to/your/repo...
CODA project initialized successfully!
  Created .coda/ directory with config.yml
  Created .trees/ directory for worktrees
  Generated .coda.md repository overview

Next step: run `coda plan <feature-slug>` to start planning a feature.
```

This analyzes your repo structure via Claude, creates `.coda/config.yml`, `.coda.md`, and the `.trees/` directory.

### Step 2: Plan the feature

```bash
coda plan add-user-auth
```

This opens an interactive TUI for a multi-turn planning conversation with the Planner agent. Describe your feature, iterate on the design, and type `/done` to finalize.

```
Planning complete!
  Design spec: /path/to/repo/.trees/add-user-auth/.coda/add-user-auth/specs/design.md
  Verification: /path/to/repo/.trees/add-user-auth/.coda/add-user-auth/specs/verification.md
  State: /path/to/repo/.trees/add-user-auth/.coda/add-user-auth/state.yml
  Worktree: /path/to/repo/.trees/add-user-auth

Next step: run `coda run add-user-auth` to execute the plan.
```

### Step 3: Execute the plan

```bash
coda run add-user-auth
```

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

### Step 4: List features

```bash
coda list
```

```
  Feature                      Status         Branch                          Turns     Cost
  ──────────────────────────────────────────────────────────────────────────────────────────
  add-user-auth                ● completed    feature/add-user-auth              24   $2.8700

  1 feature(s) total
```

### Step 5: Check detailed status

```bash
coda status add-user-auth
```

```
  Feature: add-user-auth
  ═══════════════════════════════════════

  Status:     ● completed
  Created:    2026-02-18 10:00:00 UTC
  Updated:    2026-02-18 10:24:00 UTC

  Git
  ─────────────────────────────────────
  Branch:     feature/add-user-auth
  Base:       main
  Worktree:   .trees/add-user-auth

  Phases
  ─────────────────────────────────────
  Phase        Status          Turns     Cost   Duration
  auth-types   ● completed         3   $0.1200    5m 12s
  auth-middl…  ● completed        12   $1.8500   12m 03s
  review       ● completed         5   $0.5200    3m 45s
  verify       ● completed         4   $0.3800    2m 30s

  Pull Request
  ─────────────────────────────────────
  #42: Add user authentication
  URL: https://github.com/org/repo/pull/42

  Summary
  ─────────────────────────────────────
  Total turns:    24
  Total cost:     $2.8700 USD
  Total duration: 23m 30s
  Tokens:         150000 in / 45000 out
  ═══════════════════════════════════════
```

### Step 6: Clean up merged worktrees

```bash
coda clean
```

```
  Scanning worktrees for merged/closed PRs...

  [~] add-user-auth              merged    (PR #42)

  Remove 1 worktree(s)? [y/N] y

  [✓] Removed add-user-auth              merged    (PR #42)

  Cleaned 1 worktree(s).
```

Use `--dry-run` to preview what would be removed, or `--yes` / `-y` to skip the confirmation prompt.

## CLI Reference

| Command | Description | Arguments / Flags |
|---------|-------------|-------------------|
| `coda init` | Initialize repo as a CODA project | — |
| `coda plan <slug>` | Interactive feature planning | `slug`: feature identifier |
| `coda run <slug>` | Execute feature development | `slug`: feature to run |
| `coda list` | List all features with status | — |
| `coda status <slug>` | Detailed feature status | `slug`: feature to inspect |
| `coda clean` | Remove merged/closed worktrees | `--dry-run`, `--yes` / `-y` |

**Feature slug rules:**
- Lowercase ASCII letters, digits, and hyphens only
- Must not start or end with a hyphen
- No consecutive hyphens
- Maximum 64 characters

## Slack Server

CODA also ships a Slack-integrated server (`coda-server`) that provides team workflows via Slack slash commands. Configure it in `~/.coda-server/config.yml` with your Slack app and bot tokens.

### Slack Commands

| Command | Description |
|---------|-------------|
| `/coda repos` | List GitHub repos and bind one to the channel |
| `/coda init` | Initialize the bound repo as a CODA project |
| `/coda plan <slug>` | Start interactive planning in a Slack thread |
| `/coda run <slug>` | Execute feature development with live progress |
| `/coda switch <branch>` | Switch the bound repo's branch |
| `/coda list` | List all features in the bound repo |
| `/coda status <slug>` | Show detailed feature status |
| `/coda clean` | Clean up merged worktrees |

### Repository Management

When a user selects a repository via `/coda repos`, the server clones it to a stable path under the configured workspace directory:

- **Path format**: `<workspace>/<owner>/<repo>` (e.g., `/home/user/workspace/myorg/my-repo`)
- **Reuse**: If the directory already exists with a `.git` folder, the server updates the existing clone instead of creating a new one
- **Auto-update**: On every bind, the server runs `git fetch` → detects the default branch → `git checkout --force` → `git pull` to ensure the code is current before downstream commands
- **Locking**: Concurrent clone/update operations on the same path are serialized via `RepoLocks`

This eliminates workspace clutter from repeated selections and ensures `coda init` always analyzes up-to-date code.

## Configuration

Location: `.coda/config.yml` (created by `coda init`).

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

All fields have sensible defaults. A minimal config can be as simple as:

```yaml
version: 1
```

## How It Works

`coda init` analyzes the repository and generates a project overview. `coda plan` opens an interactive design session where you collaborate with Claude to produce a design spec and verification plan. `coda run` executes the development phases, runs configurable quality checks between phases, performs code review, verifies the result, and opens a pull request.

Each feature runs in an isolated git worktree. State is tracked in `state.yml` for crash recovery — if a run is interrupted, re-running `coda run` resumes from the last completed phase. Cost is tracked per phase and displayed in real time.

```
┌─────────────────────────────────────────────────────────┐
│                    coda-cli (app)                        │
│  Ratatui TUI · Clap CLI · Line Editor · App Driver      │
└───────────────────────┬─────────────────────────────────┘
                        │
┌───────────────────────▼─────────────────────────────────┐
│                   coda-server (app)                      │
│  Slack Integration · Repo Clone/Update · RepoLocks      │
└───────────────────────┬─────────────────────────────────┘
                        │
┌───────────────────────▼─────────────────────────────────┐
│                   coda-core (lib)                        │
│  Engine · Planner · Runner · Reviewer                   │
│  FeatureScanner · GitOps · GhOps (traits)               │
└──────────┬────────────────────────┬─────────────────────┘
           │                        │
┌──────────▼───────────┐  ┌────────▼─────────────────────┐
│    coda-pm (lib)     │  │  claude-agent-sdk-rs          │
│  Prompt Manager      │  │  Claude AI client             │
│  MiniJinja templates │  │  Streaming, hooks, MCP        │
└──────────────────────┘  └──────────────────────────────┘
```

See `.coda.md` for the full repository overview.

## Development

### Build and test

```bash
cargo build                              # Build all crates
cargo +nightly fmt                       # Format (requires nightly)
cargo clippy -- -D warnings              # Lint
cargo test                               # Run tests
cargo nextest run                        # Run tests (nextest)
cargo deny check                         # License/dependency policy
cargo audit                              # Security audit
```

### Workspace structure

| Path | Purpose |
|------|---------|
| `apps/coda-cli` | CLI binary — TUI, argument parsing, app logic |
| `apps/coda-server` | Server binary — Slack integration, repo management, command dispatch |
| `crates/coda-core` | Core library — engine, planner, runner, reviewer, state |
| `crates/coda-pm` | Prompt manager — Jinja2 template loading and rendering |
| `vendors/claude-agent-sdk-rs` | Vendored Claude Agent SDK |
| `specs/` | Feature specs and design documents |
| `docs/` | Project documentation |

See `CLAUDE.md` for coding conventions and development guidelines.

## License

This project is distributed under the terms of MIT.

See [LICENSE](LICENSE.md) for details.

Copyright 2025 Tyr Chen
