# coda-core API Reference

> The core engine for orchestrating Claude agent interactions. Provides configuration management, task execution, agent profile selection, and state tracking for CODA's development workflow.

## Re-exports

| Item | Kind | Description |
|------|------|-------------|
| `CodaConfig` | struct | Top-level project configuration loaded from `.coda/config.yml` |
| `Engine` | struct | Core execution engine orchestrating init, plan, run, and clean operations |
| `CleanedWorktree` | struct | Result of cleaning a single worktree (slug, branch, PR info) |
| `CoreError` | enum | Error type for all coda-core operations |
| `DefaultGhOps` | struct | Production `GhOps` implementation that shells out to `gh` |
| `GhOps` | trait | Abstraction over GitHub CLI operations |
| `PrStatus` | struct | PR status as returned by the GitHub CLI |
| `DefaultGitOps` | struct | Production `GitOps` implementation that shells out to `git` |
| `GitOps` | trait | Abstraction over git CLI operations |
| `PlanOutput` | struct | Output produced by finalizing a planning session |
| `PlanSession` | struct | Interactive planning session wrapping a `ClaudeClient` with the Planner profile |
| `AgentProfile` | enum | Agent profile controlling tool access and SDK configuration |
| `build_safety_hooks` | fn | Builds safety hooks for the Coder profile (dangerous command blocking) |
| `find_project_root` | fn | Finds the project root by walking up from the current directory |
| `ReviewResult` | struct | Result of an agent-driven code review cycle |
| `Runner` | struct | Orchestrates feature execution through all phases |
| `RunEvent` | enum | Real-time progress events emitted during a feature run |
| `RunProgress` | struct | Progress tracking for a multi-phase feature development run |
| `CommitInfo` | struct | A commit recorded during execution |
| `ReviewSummary` | struct | Summary of code review results |
| `VerificationSummary` | struct | Summary of verification results |
| `FeatureScanner` | struct | Scans `.trees/` for feature worktrees and reads their state |
| `Task` | enum | A unit of work in CODA's execution pipeline |
| `TaskResult` | struct | Result of executing a task with metrics |
| `TaskStatus` | enum | Outcome status of a task execution |

## Modules

### `mod config`

| Item | Kind | Description |
|------|------|-------------|
| `CodaConfig` | struct | Top-level project configuration with agent, checks, prompts, git, and review sections |
| `AgentConfig` | struct | Agent configuration controlling model and budget limits |
| `PromptsConfig` | struct | Configuration for prompt template directories |
| `GitConfig` | struct | Git workflow configuration (auto-commit, branch prefix, base branch) |
| `ReviewConfig` | struct | Code review configuration (enabled flag, max rounds) |

### `mod gh`

| Item | Kind | Description |
|------|------|-------------|
| `PrStatus` | struct | PR status with state, number, and optional URL |
| `DefaultGhOps` | struct | Production implementation that shells out to `gh` |

```rust
pub trait GhOps: Send + Sync {
    fn pr_view_state(&self, pr_number: u32) -> Result<Option<PrStatus>, CoreError>;
    fn pr_list_by_branch(&self, branch: &str) -> Result<Option<PrStatus>, CoreError>;
    fn pr_url_for_branch(&self, branch: &str, cwd: &Path) -> Option<String>;
}
```

### `mod git`

| Item | Kind | Description |
|------|------|-------------|
| `DefaultGitOps` | struct | Production implementation that shells out to `git` |

```rust
pub trait GitOps: Send + Sync {
    fn worktree_add(&self, path: &Path, branch: &str, base: &str) -> Result<(), CoreError>;
    fn worktree_remove(&self, path: &Path, force: bool) -> Result<(), CoreError>;
    fn worktree_prune(&self) -> Result<(), CoreError>;
    fn branch_delete(&self, branch: &str) -> Result<(), CoreError>;
    fn add(&self, cwd: &Path, paths: &[&str]) -> Result<(), CoreError>;
    fn has_staged_changes(&self, cwd: &Path) -> bool;
    fn commit(&self, cwd: &Path, message: &str) -> Result<(), CoreError>;
    fn diff(&self, cwd: &Path, base: &str) -> Result<String, CoreError>;
    fn log_oneline(&self, cwd: &Path, range: &str) -> Result<String, CoreError>;
    fn push(&self, cwd: &Path, branch: &str) -> Result<(), CoreError>;
    fn detect_default_branch(&self) -> String;
}
```

### `mod parser`

| Item | Kind | Description |
|------|------|-------------|
| `extract_yaml_block` | fn | Extracts a YAML code block from a response string |
| `parse_review_issues` | fn | Parses review issues from agent YAML response, filtering to critical/major only |
| `parse_verification_result` | fn | Parses verification results, returning `(passed_count, failed_details)` |
| `extract_pr_url` | fn | Extracts a GitHub PR URL (`/pull/N`) from text |
| `extract_pr_number` | fn | Extracts the PR number from a GitHub PR URL |

### `mod planner`

| Item | Kind | Description |
|------|------|-------------|
| `PlanOutput` | struct | Output produced by finalizing a planning session (spec paths, worktree path) |
| `PlanSession` | struct | Interactive multi-turn planning session with the Planner profile |
| `extract_dev_phases` | fn | Extracts development phase names from a design specification's `### Phase N:` headings |

### `mod profile`

| Item | Kind | Description |
|------|------|-------------|
| `AgentProfile` | enum | Agent profile enum (`Planner`, `Coder`) controlling tool access |
| `build_safety_hooks` | fn | Builds pre/post tool-use hooks that block dangerous commands |

### `mod project`

| Item | Kind | Description |
|------|------|-------------|
| `find_project_root` | fn | Walks up from CWD looking for `.coda/` or `.git/` markers |

### `mod reviewer`

| Item | Kind | Description |
|------|------|-------------|
| `ReviewResult` | struct | Aggregated review findings with resolution status |
| `ReviewSummary` | re-export | Re-exported from `runner` module |

### `mod runner`

| Item | Kind | Description |
|------|------|-------------|
| `Runner` | struct | Executes a feature through all phases using a single `ClaudeClient` session |
| `RunEvent` | enum | Real-time progress events (`RunStarting`, `PhaseStarting`, `PhaseCompleted`, `PhaseFailed`, `CreatingPr`, `PrCreated`) |
| `RunProgress` | struct | Aggregated results of all completed phases |
| `CommitInfo` | struct | Short SHA and message of a recorded commit |
| `ReviewSummary` | struct | Review round/issue counts |
| `VerificationSummary` | struct | Verification check pass/total counts |

### `mod scanner`

| Item | Kind | Description |
|------|------|-------------|
| `FeatureScanner` | struct | Discovers features in `.trees/` and reads their `state.yml` files |

### `mod state`

| Item | Kind | Description |
|------|------|-------------|
| `FeatureState` | struct | Complete feature execution state persisted to `state.yml` |
| `FeatureInfo` | struct | Basic feature metadata (slug, timestamps) |
| `FeatureStatus` | enum | Overall feature status (`Planned`, `InProgress`, `Completed`, `Failed`) |
| `GitInfo` | struct | Git branch and worktree details |
| `PhaseKind` | enum | Distinguishes `Dev` phases from `Quality` phases |
| `PhaseRecord` | struct | Record of a single execution phase with metrics |
| `PhaseStatus` | enum | Phase execution status (`Pending`, `Running`, `Completed`, `Failed`) |
| `PrInfo` | struct | Pull request URL, number, and title |
| `TokenCost` | struct | Input/output token usage breakdown |
| `TotalStats` | struct | Cumulative statistics across all phases |

### `mod task`

| Item | Kind | Description |
|------|------|-------------|
| `Task` | enum | Unit of work (`Init`, `Plan`, `DevPhase`, `Review`, `Verify`, `CreatePr`) |
| `TaskResult` | struct | Execution result with task identity, status, turns, cost, duration, and artifacts |
| `TaskStatus` | enum | Outcome status (`Completed`, `Failed { error }`) |

## Type Details

### `CoreError`

```rust
#[non_exhaustive]
pub enum CoreError {
    AgentError(String),
    PromptError(#[from] coda_pm::PromptError),
    IoError(#[from] std::io::Error),
    ConfigError(String),
    StateError(String),
    PlanError(String),
    GitError(String),
    YamlError(#[from] serde_yaml::Error),
    AnyhowError(#[from] anyhow::Error),
}
```

| Variant | Description |
|---------|-------------|
| `AgentError` | Error from the Claude Agent SDK or agent execution |
| `PromptError` | Error from the prompt template manager |
| `IoError` | I/O error from file system operations |
| `ConfigError` | Invalid or missing configuration |
| `StateError` | Invalid or missing state file |
| `PlanError` | Planning workflow error (e.g., finalizing without approval) |
| `GitError` | Git/gh external CLI operation error |
| `YamlError` | YAML serialization/deserialization error |
| `AnyhowError` | Generic error from anyhow |
