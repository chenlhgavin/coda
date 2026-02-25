//! Phase executor framework for decomposed runner logic.
//!
//! Provides the [`PhaseExecutor`] trait and [`PhaseContext`] struct that
//! allow each phase type (dev, review, verify, docs, PR) to be implemented
//! as a focused, testable component rather than methods on the monolithic
//! `Runner` struct.
//!
//! # Architecture
//!
//! ```text
//! Runner::execute()
//!   └─ for each phase:
//!        1. Build the appropriate PhaseExecutor (DevPhaseExecutor, etc.)
//!        2. Call executor.execute(&mut ctx)
//!        3. Handle result (update state, emit events)
//! ```
//!
//! The [`PhaseContext`] bundles all shared mutable state (session, metrics,
//! summaries, logger) so executors can access everything they need without
//! reaching back into the Runner. State persistence is delegated to
//! [`StateManager`](crate::state::StateManager) which enforces transition
//! invariants and keeps `current_phase` derived from phase statuses.

pub mod dev;
pub mod docs;
pub mod pr;
pub mod review;
pub mod verify;

use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use coda_agent_sdk::ResultMessage;
use coda_pm::PromptManager;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::CoreError;
use crate::async_ops::{AsyncGhOps, AsyncGitOps, commit_coda_artifacts_async};
use crate::config::CodaConfig;
use crate::runner::RunEvent;
use crate::session::{AgentResponse, AgentSession};
use crate::state::{FeatureState, PhaseStatus, StateManager};
use crate::task::TaskResult;

// Re-export PhaseOutcome so executor files can continue to use `super::PhaseOutcome`.
pub use crate::state::PhaseOutcome;

/// Trait for phase-specific execution logic.
///
/// Each phase type (dev, review, verify, docs, PR) implements this trait.
/// The `Runner` creates the appropriate executor and calls `execute()`
/// with a shared [`PhaseContext`].
///
/// Uses native `async fn` in traits (stable since Rust 1.75) rather than
/// `#[async_trait]` because executors are dispatched via `match` on
/// `PhaseKind`/name, not stored as `dyn PhaseExecutor` trait objects.
///
/// # Example
///
/// ```no_run
/// use coda_core::phases::{PhaseExecutor, PhaseContext};
/// use coda_core::task::TaskResult;
/// use coda_core::CoreError;
///
/// struct MyExecutor;
///
/// impl PhaseExecutor for MyExecutor {
///     async fn execute(
///         &mut self,
///         ctx: &mut PhaseContext,
///         phase_idx: usize,
///     ) -> Result<TaskResult, CoreError> {
///         // Phase-specific logic here
///         todo!()
///     }
/// }
/// ```
pub trait PhaseExecutor: Send {
    /// Executes the phase logic, returning a [`TaskResult`] on success.
    ///
    /// The executor receives full mutable access to [`PhaseContext`],
    /// which contains the agent session, state manager, metrics tracker,
    /// and other shared resources.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the phase fails.
    fn execute(
        &mut self,
        ctx: &mut PhaseContext,
        phase_idx: usize,
    ) -> impl Future<Output = Result<TaskResult, CoreError>> + Send;
}

/// Shared mutable context passed to each [`PhaseExecutor`].
///
/// Bundles the agent session, state manager, metrics tracking, run logger,
/// and other resources that all phase types need. The `Runner` creates this
/// once and passes it to each executor in sequence.
///
/// State persistence is delegated to [`StateManager`] which enforces
/// phase transition invariants and derives `current_phase` from statuses.
/// Use [`state()`](Self::state) for read access to [`FeatureState`], and
/// [`state_manager`](Self::state_manager) for mutations.
///
/// Git and GitHub CLI operations are wrapped in [`AsyncGitOps`] /
/// [`AsyncGhOps`] to avoid blocking the async runtime. Use
/// [`git.inner()`](AsyncGitOps::inner) when a sync reference is needed.
///
/// # `Send` / `Sync` note
///
/// `PhaseContext` is `Send` but **not** `Sync` because [`AgentSession`]
/// contains a `dyn Fn() + Send` callback. Methods that perform async git/gh
/// operations are non-async and return `impl Future<Output = ...> + Send`
/// via `async move` blocks, so the returned future owns all captured data
/// and the `&self` borrow ends before the caller's `.await`.
///
/// # Example
///
/// ```no_run
/// # use coda_core::phases::PhaseContext;
/// # fn example(ctx: &PhaseContext) {
/// ctx.emit_event(coda_core::RunEvent::Connecting);
/// # }
/// ```
pub struct PhaseContext {
    /// The shared agent session for sending prompts.
    pub session: AgentSession,
    /// State persistence manager with transition-invariant enforcement.
    pub state_manager: StateManager,
    /// Absolute path to the git worktree.
    pub worktree_path: PathBuf,
    /// Prompt template manager.
    pub pm: PromptManager,
    /// Project configuration.
    pub config: CodaConfig,
    /// Async git operations (wraps blocking calls in `spawn_blocking`).
    pub git: AsyncGitOps,
    /// Async GitHub CLI operations (wraps blocking calls in `spawn_blocking`).
    pub gh: AsyncGhOps,
    /// Progress event sender (None if no subscriber).
    pub progress_tx: Option<UnboundedSender<RunEvent>>,
    /// Per-run structured log writer.
    pub run_logger: Option<RunLogger>,
    /// Cumulative SDK metrics tracker.
    pub metrics: MetricsTracker,
    /// Accumulated review results across rounds.
    pub review_summary: ReviewSummary,
    /// Accumulated verification results.
    pub verification_summary: VerificationSummary,
    /// Original commits collected before squash (for PR body context).
    pub pre_squash_commits: Vec<CommitInfo>,
    /// Cancellation token for graceful shutdown.
    pub cancel_token: CancellationToken,
}

impl PhaseContext {
    /// Returns a read-only reference to the current feature state.
    ///
    /// Shorthand for `self.state_manager.state()`.
    pub fn state(&self) -> &FeatureState {
        self.state_manager.state()
    }

    /// Emits a progress event to the subscriber, if one is registered.
    ///
    /// Silently ignores send failures (e.g., if the receiver was dropped).
    pub fn emit_event(&self, event: RunEvent) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.send(event);
        }
    }

    /// Returns `Err(CoreError::Cancelled)` if the cancellation token has
    /// been triggered.
    ///
    /// Call this between phases or at other natural check points to
    /// enable responsive cancellation.
    pub fn check_cancelled(&self) -> Result<(), CoreError> {
        if self.cancel_token.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        Ok(())
    }

    /// Loads a spec file from the worktree's `.coda/<slug>/specs/` directory.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::StateError` if the spec file cannot be read.
    pub fn load_spec(&self, filename: &str) -> Result<String, CoreError> {
        let spec_path = self
            .worktree_path
            .join(".coda")
            .join(&self.state().feature.slug)
            .join("specs")
            .join(filename);

        std::fs::read_to_string(&spec_path).map_err(|e| {
            CoreError::StateError(format!("Cannot read spec at {}: {e}", spec_path.display()))
        })
    }

    /// Gets the git diff of all changes from the base branch.
    ///
    /// Returns an owned future (via `async move`) that does not capture
    /// `&self`, keeping the `&self` borrow strictly synchronous. This
    /// allows callers to `.await` the result without holding a non-`Send`
    /// `&PhaseContext` reference across the yield point.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` on failure.
    pub fn get_diff(&self) -> impl Future<Output = Result<String, CoreError>> + Send {
        let git = self.git.clone();
        let cwd = self.worktree_path.clone();
        let base = self.state().git.base_branch.clone();
        async move { git.diff(&cwd, &base).await }
    }

    /// Gets the list of changed file paths from the base branch.
    ///
    /// Returns an owned future that does not capture `&self`.
    /// See [`get_diff`](Self::get_diff) for rationale.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` on failure.
    pub fn get_changed_files(&self) -> impl Future<Output = Result<Vec<String>, CoreError>> + Send {
        let git = self.git.clone();
        let cwd = self.worktree_path.clone();
        let base = self.state().git.base_branch.clone();
        async move { git.diff_name_only(&cwd, &base).await }
    }

    /// Returns the worktree-relative path to a spec file.
    pub fn spec_relative_path(&self, filename: &str) -> String {
        format!(".coda/{}/specs/{filename}", self.state().feature.slug)
    }

    /// Commits any pending `.coda/` changes in the worktree.
    ///
    /// Uses `--no-verify` because `.coda/` files are CODA-internal
    /// artifacts that should not be gated by project-specific hooks.
    /// Returns an owned future that does not capture `&self`.
    /// See [`get_diff`](Self::get_diff) for rationale.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` on failure.
    pub fn commit_coda_state(&self) -> impl Future<Output = Result<(), CoreError>> + Send {
        let git = self.git.clone();
        let cwd = self.worktree_path.clone();
        let msg = format!(
            "chore({}): update execution state",
            self.state().feature.slug,
        );
        async move { commit_coda_artifacts_async(&git, &cwd, &[".coda/"], &msg).await }
    }

    /// Sends a prompt and collects the full response via [`AgentSession`].
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the agent interaction fails.
    pub async fn send_and_collect(
        &mut self,
        prompt: &str,
        session_id: Option<&str>,
    ) -> Result<AgentResponse, CoreError> {
        self.session.send(prompt, session_id).await
    }

    /// Gets the list of commits from the base branch to HEAD.
    ///
    /// Returns an owned future that does not capture `&self`.
    /// See [`get_diff`](Self::get_diff) for rationale.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` on failure.
    pub fn get_commits(&self) -> impl Future<Output = Result<Vec<CommitInfo>, CoreError>> + Send {
        let git = self.git.clone();
        let cwd = self.worktree_path.clone();
        let range = format!("{}..HEAD", self.state().git.base_branch);
        async move {
            let stdout = git.log_oneline(&cwd, &range).await?;

            let commits = stdout
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ' ');
                    let sha = parts.next()?.to_string();
                    let message = parts.next().unwrap_or("").to_string();
                    Some(CommitInfo { sha, message })
                })
                .collect();

            Ok(commits)
        }
    }

    /// Builds resume context for interrupted executions.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::PromptError` on template rendering failure.
    pub fn build_resume_context(&self) -> Result<String, CoreError> {
        let state = self.state();
        let current_idx = self.state_manager.current_phase_index();

        let completed_phases: Vec<serde_json::Value> = state
            .phases
            .iter()
            .filter(|p| p.status == PhaseStatus::Completed)
            .map(|p| {
                let summary = format!(
                    "{} turns, {}s, {} input / {} output tokens",
                    p.turns, p.duration_secs, p.cost.input_tokens, p.cost.output_tokens
                );
                serde_json::json!({
                    "name": p.name,
                    "duration_secs": p.duration_secs,
                    "turns": p.turns,
                    "cost": {
                        "input_tokens": p.cost.input_tokens,
                        "output_tokens": p.cost.output_tokens,
                    },
                    "summary": summary,
                })
            })
            .collect();

        let current_phase_name = &state.phases[current_idx].name;
        let current_phase_state = &state.phases[current_idx];

        self.pm
            .render(
                "run/resume",
                minijinja::context!(
                    state => state,
                    completed_phases => completed_phases,
                    current_phase => current_phase_name,
                    current_phase_state => current_phase_state,
                ),
            )
            .map_err(CoreError::from)
    }
}

impl std::fmt::Debug for PhaseContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseContext")
            .field("feature", &self.state().feature.slug)
            .field("worktree", &self.worktree_path)
            .finish_non_exhaustive()
    }
}

// ── Supporting Types ────────────────────────────────────────────────

/// A commit recorded during execution.
///
/// # Example
///
/// ```
/// use coda_core::phases::CommitInfo;
///
/// let commit = CommitInfo {
///     sha: "abc1234".to_string(),
///     message: "feat: add new feature".to_string(),
/// };
/// assert_eq!(commit.sha, "abc1234");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Short SHA of the commit.
    pub sha: String,
    /// Commit message.
    pub message: String,
}

/// Summary of code review results.
///
/// # Example
///
/// ```
/// use coda_core::phases::ReviewSummary;
///
/// let summary = ReviewSummary::default();
/// assert_eq!(summary.rounds, 0);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewSummary {
    /// Number of review rounds performed.
    pub rounds: u32,
    /// Total issues found across all rounds.
    pub issues_found: u32,
    /// Total issues for which a fix was attempted (not necessarily resolved).
    pub issues_fix_attempted: u32,
}

/// Summary of verification results.
///
/// # Example
///
/// ```
/// use coda_core::phases::VerificationSummary;
///
/// let summary = VerificationSummary::default();
/// assert_eq!(summary.checks_passed, 0);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationSummary {
    /// Number of checks that passed.
    pub checks_passed: u32,
    /// Total number of checks.
    pub checks_total: u32,
}

/// Incremental metrics from a single agent interaction.
///
/// Computed by [`MetricsTracker::record`] as the delta between the
/// current and previous cumulative SDK values.
#[derive(Debug, Clone, Copy, Default)]
pub struct IncrementalMetrics {
    /// Incremental cost in USD for this interaction.
    pub cost_usd: f64,
    /// Incremental input tokens consumed.
    pub input_tokens: u64,
    /// Incremental output tokens generated.
    pub output_tokens: u64,
}

/// Tracks cumulative SDK metrics and computes per-interaction deltas.
///
/// The Claude Agent SDK reports cumulative totals for cost and token usage
/// across the entire session. This tracker maintains the running totals and
/// returns the incremental delta for each interaction.
///
/// # Counter Rollback Detection
///
/// When the agent subprocess is reconnected after an idle timeout, the new
/// process starts with fresh cumulative counters (all zeros). This tracker
/// detects such rollbacks — where new cumulative values are lower than the
/// previously recorded values — and treats the new values as absolute
/// deltas rather than computing a difference that would be zero or negative.
///
/// # Example
///
/// ```
/// use coda_core::phases::MetricsTracker;
///
/// let tracker = MetricsTracker::default();
/// assert!((tracker.cumulative_cost_usd - 0.0).abs() < f64::EPSILON);
/// ```
#[derive(Debug, Default)]
pub struct MetricsTracker {
    /// Running cumulative cost from the SDK.
    pub cumulative_cost_usd: f64,
    /// Running cumulative input tokens from the SDK.
    pub cumulative_input_tokens: u64,
    /// Running cumulative output tokens from the SDK.
    pub cumulative_output_tokens: u64,
}

impl MetricsTracker {
    /// Records a new SDK result and returns the incremental delta.
    ///
    /// Detects counter rollback (e.g., after subprocess reconnection where
    /// the new process starts with fresh cumulative counters) and treats
    /// the new values as absolute deltas rather than computing negative
    /// differences.
    pub fn record(&mut self, result: &Option<ResultMessage>) -> IncrementalMetrics {
        let new_cost = result
            .as_ref()
            .and_then(|r| r.total_cost_usd)
            .unwrap_or(self.cumulative_cost_usd);

        // If new_cost < cumulative, a reconnect caused counter rollback.
        // Treat the new value as the absolute delta from the new subprocess.
        let cost_delta = if new_cost < self.cumulative_cost_usd {
            new_cost.max(0.0)
        } else {
            new_cost - self.cumulative_cost_usd
        };
        self.cumulative_cost_usd = new_cost;

        let (new_input, new_output) = result
            .as_ref()
            .and_then(|r| r.usage.as_ref())
            .map(|u| {
                let input = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let output = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                (input, output)
            })
            .unwrap_or((self.cumulative_input_tokens, self.cumulative_output_tokens));

        // If new values < cumulative, counters rolled back. Treat new values as deltas.
        let input_delta = if new_input < self.cumulative_input_tokens {
            new_input
        } else {
            new_input - self.cumulative_input_tokens
        };
        let output_delta = if new_output < self.cumulative_output_tokens {
            new_output
        } else {
            new_output - self.cumulative_output_tokens
        };
        self.cumulative_input_tokens = new_input;
        self.cumulative_output_tokens = new_output;

        IncrementalMetrics {
            cost_usd: cost_delta,
            input_tokens: input_delta,
            output_tokens: output_delta,
        }
    }
}

/// Accumulates metrics across multiple agent interactions within a single phase.
///
/// Used by multi-round phases (review, verify) where each round involves
/// one or more agent calls and the totals must be aggregated.
///
/// # Example
///
/// ```
/// use coda_core::phases::PhaseMetricsAccumulator;
///
/// let acc = PhaseMetricsAccumulator::new();
/// assert_eq!(acc.turns, 0);
/// ```
#[derive(Debug)]
pub struct PhaseMetricsAccumulator {
    /// Start time of the phase.
    pub start: Instant,
    /// Accumulated conversation turns.
    pub turns: u32,
    /// Accumulated cost in USD.
    pub cost_usd: f64,
    /// Accumulated input tokens.
    pub input_tokens: u64,
    /// Accumulated output tokens.
    pub output_tokens: u64,
}

impl PhaseMetricsAccumulator {
    /// Creates a new accumulator, recording the start time.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            turns: 0,
            cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Records metrics from a single agent interaction.
    pub fn record(&mut self, resp: &AgentResponse, metrics: IncrementalMetrics) {
        self.turns += resp.result.as_ref().map_or(1, |r| r.num_turns);
        self.cost_usd += metrics.cost_usd;
        self.input_tokens += metrics.input_tokens;
        self.output_tokens += metrics.output_tokens;
    }

    /// Converts accumulated metrics into a [`PhaseOutcome`].
    pub fn into_outcome(self, details: serde_json::Value) -> PhaseOutcome {
        PhaseOutcome {
            turns: self.turns,
            cost_usd: self.cost_usd,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            duration: self.start.elapsed(),
            details,
        }
    }
}

impl Default for PhaseMetricsAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Run Logger ──────────────────────────────────────────────────────

/// Maximum characters to include from prompt/response text in the log.
const LOG_TEXT_LIMIT: usize = 50_000;

/// Truncates text for log output at a safe UTF-8 boundary.
fn truncate_for_log(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        text
    } else {
        let mut end = limit;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }
}

/// Structured run log writer for debugging agent interactions.
///
/// Writes a human-readable log of every prompt/response exchange to
/// `.coda/<slug>/logs/run-<timestamp>.log`.
pub struct RunLogger {
    file: File,
}

impl RunLogger {
    /// Creates a new logger writing to `.coda/<slug>/logs/run-<timestamp>.log`.
    ///
    /// Returns `None` if the log file cannot be created (best-effort).
    pub fn new(feature_dir: &Path) -> Option<Self> {
        let logs_dir = feature_dir.join("logs");
        if let Err(e) = fs::create_dir_all(&logs_dir) {
            warn!(error = %e, "Cannot create logs directory");
            return None;
        }

        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let log_path = logs_dir.join(format!("run-{timestamp}.log"));

        match OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(file) => {
                info!(path = %log_path.display(), "Run log opened");
                Some(Self { file })
            }
            Err(e) => {
                warn!(error = %e, "Cannot open run log file");
                None
            }
        }
    }

    /// Writes the run header with feature metadata.
    pub fn log_header(&mut self, feature_slug: &str, model: &str, phases: &[String]) {
        let _ = writeln!(self.file, "═══ CODA Run: {feature_slug} ═══");
        let _ = writeln!(
            self.file,
            "Started: {}",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        let _ = writeln!(self.file, "Model: {model}");
        let _ = writeln!(self.file, "Phases: {}", phases.join(" → "));
        let _ = writeln!(self.file);
    }

    /// Logs the start of a phase.
    pub fn log_phase_start(&mut self, name: &str, index: usize, total: usize, kind: &str) {
        let _ = writeln!(self.file, "────────────────────────────────");
        let _ = writeln!(self.file, "Phase {}/{total}: {name} [{kind}]", index + 1);
        let _ = writeln!(self.file, "────────────────────────────────");
        let _ = writeln!(self.file);
    }

    /// Logs a single agent interaction (prompt + response).
    pub fn log_interaction(
        &mut self,
        prompt: &str,
        resp: &AgentResponse,
        metrics: &IncrementalMetrics,
    ) {
        let _ = writeln!(self.file, ">>> PROMPT ({} chars)", prompt.len());
        let truncated_prompt = truncate_for_log(prompt, LOG_TEXT_LIMIT);
        let _ = writeln!(self.file, "{truncated_prompt}");
        if prompt.len() > LOG_TEXT_LIMIT {
            let _ = writeln!(self.file, "... [truncated at {LOG_TEXT_LIMIT} chars]");
        }
        let _ = writeln!(self.file);

        let _ = writeln!(
            self.file,
            "<<< RESPONSE (text: {} chars, tool_output: {} chars)",
            resp.text.len(),
            resp.tool_output.len(),
        );

        if resp.text.is_empty() && resp.tool_output.is_empty() {
            let _ = writeln!(self.file, "⚠ WARNING: Empty response from agent");
        } else {
            if !resp.text.is_empty() {
                let _ = writeln!(self.file, "[text]");
                let truncated = truncate_for_log(&resp.text, LOG_TEXT_LIMIT);
                let _ = writeln!(self.file, "{truncated}");
                if resp.text.len() > LOG_TEXT_LIMIT {
                    let _ = writeln!(self.file, "... [truncated at {LOG_TEXT_LIMIT} chars]");
                }
            }
            if !resp.tool_output.is_empty() {
                let _ = writeln!(self.file, "[tool_output]");
                let truncated = truncate_for_log(&resp.tool_output, LOG_TEXT_LIMIT);
                let _ = writeln!(self.file, "{truncated}");
                if resp.tool_output.len() > LOG_TEXT_LIMIT {
                    let _ = writeln!(self.file, "... [truncated at {LOG_TEXT_LIMIT} chars]");
                }
            }
        }

        let _ = writeln!(
            self.file,
            "[metrics] turns={}, cost=${:.4}, input_tokens={}, output_tokens={}",
            resp.result.as_ref().map_or(0, |r| r.num_turns),
            metrics.cost_usd,
            metrics.input_tokens,
            metrics.output_tokens,
        );
        let _ = writeln!(self.file);
    }

    /// Logs the PR extraction process.
    pub fn log_pr_extraction(
        &mut self,
        text_result: Option<&str>,
        gh_result: Option<&str>,
        final_url: Option<&str>,
    ) {
        let _ = writeln!(self.file, "[PR extraction]");
        let _ = writeln!(
            self.file,
            "  extract_pr_url(all_text) → {}",
            text_result.unwrap_or("None"),
        );
        let _ = writeln!(
            self.file,
            "  check_pr_exists_via_gh  → {}",
            gh_result.unwrap_or("not attempted"),
        );
        let _ = writeln!(
            self.file,
            "  Result: {}",
            final_url
                .map(|u| format!("OK → {u}"))
                .unwrap_or_else(|| "FAILED — no PR URL found".to_string()),
        );
        let _ = writeln!(self.file);
    }

    /// Logs a generic message.
    pub fn log_message(&mut self, msg: &str) {
        let _ = writeln!(self.file, "{msg}");
    }
}

impl std::fmt::Debug for RunLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunLogger").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper to build a ResultMessage ──────────────────────────

    fn make_result(cost: f64, input: u64, output: u64) -> ResultMessage {
        ResultMessage {
            subtype: "success".to_string(),
            duration_ms: 1000,
            duration_api_ms: 800,
            is_error: false,
            num_turns: 3,
            session_id: "test".to_string(),
            total_cost_usd: Some(cost),
            usage: Some(serde_json::json!({
                "input_tokens": input,
                "output_tokens": output,
            })),
            result: None,
            structured_output: None,
        }
    }

    // ── MetricsTracker tests ────────────────────────────────────

    #[test]
    fn test_should_compute_incremental_metrics_from_result() {
        let mut tracker = MetricsTracker::default();

        let m1 = tracker.record(&Some(make_result(0.50, 1000, 500)));
        assert!((m1.cost_usd - 0.50).abs() < f64::EPSILON);
        assert_eq!(m1.input_tokens, 1000);
        assert_eq!(m1.output_tokens, 500);

        let m2 = tracker.record(&Some(make_result(0.80, 2500, 1200)));
        assert!((m2.cost_usd - 0.30).abs() < f64::EPSILON);
        assert_eq!(m2.input_tokens, 1500);
        assert_eq!(m2.output_tokens, 700);
    }

    #[test]
    fn test_should_handle_none_result_gracefully() {
        let mut tracker = MetricsTracker::default();
        let m = tracker.record(&None);
        assert!((m.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(m.input_tokens, 0);
        assert_eq!(m.output_tokens, 0);
    }

    #[test]
    fn test_should_detect_counter_rollback_after_reconnect() {
        let mut tracker = MetricsTracker::default();

        // First interaction: cumulative counters go to 0.50 / 1000 / 500
        let m1 = tracker.record(&Some(make_result(0.50, 1000, 500)));
        assert!((m1.cost_usd - 0.50).abs() < f64::EPSILON);
        assert_eq!(m1.input_tokens, 1000);
        assert_eq!(m1.output_tokens, 500);

        // Simulate reconnect: new subprocess reports lower cumulative values.
        // Without rollback detection, deltas would be 0.0 / 0 / 0.
        // With rollback detection, the new values ARE the deltas.
        let m2 = tracker.record(&Some(make_result(0.30, 800, 400)));
        assert!(
            (m2.cost_usd - 0.30).abs() < f64::EPSILON,
            "Cost delta after rollback should be the new value (0.30), got {}",
            m2.cost_usd,
        );
        assert_eq!(
            m2.input_tokens, 800,
            "Input tokens after rollback should be the new value (800)",
        );
        assert_eq!(
            m2.output_tokens, 400,
            "Output tokens after rollback should be the new value (400)",
        );

        // Verify cumulative state was updated to the new values
        assert!((tracker.cumulative_cost_usd - 0.30).abs() < f64::EPSILON);
        assert_eq!(tracker.cumulative_input_tokens, 800);
        assert_eq!(tracker.cumulative_output_tokens, 400);

        // Next normal interaction from the new subprocess should compute correctly
        let m3 = tracker.record(&Some(make_result(0.60, 1800, 900)));
        assert!((m3.cost_usd - 0.30).abs() < f64::EPSILON);
        assert_eq!(m3.input_tokens, 1000);
        assert_eq!(m3.output_tokens, 500);
    }

    #[test]
    fn test_should_handle_rollback_to_zero() {
        let mut tracker = MetricsTracker::default();

        // Build up cumulative state
        tracker.record(&Some(make_result(1.00, 5000, 2000)));

        // Rollback to zero (new subprocess, no work done yet)
        let m = tracker.record(&Some(make_result(0.0, 0, 0)));
        assert!((m.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(m.input_tokens, 0);
        assert_eq!(m.output_tokens, 0);
    }

    // ── PhaseMetricsAccumulator tests ───────────────────────────

    #[test]
    fn test_should_accumulate_metrics_across_rounds() {
        let mut acc = PhaseMetricsAccumulator::new();

        let resp1 = AgentResponse {
            text: "Review response".to_string(),
            tool_output: String::new(),
            result: Some(ResultMessage {
                subtype: "success".to_string(),
                duration_ms: 1000,
                duration_api_ms: 800,
                is_error: false,
                num_turns: 3,
                session_id: "test".to_string(),
                total_cost_usd: None,
                usage: None,
                result: None,
                structured_output: None,
            }),
        };

        let m1 = IncrementalMetrics {
            cost_usd: 0.10,
            input_tokens: 500,
            output_tokens: 200,
        };
        acc.record(&resp1, m1);

        let resp2 = AgentResponse {
            text: "Fix response".to_string(),
            tool_output: String::new(),
            result: Some(ResultMessage {
                subtype: "success".to_string(),
                duration_ms: 2000,
                duration_api_ms: 1600,
                is_error: false,
                num_turns: 5,
                session_id: "test".to_string(),
                total_cost_usd: None,
                usage: None,
                result: None,
                structured_output: None,
            }),
        };

        let m2 = IncrementalMetrics {
            cost_usd: 0.15,
            input_tokens: 800,
            output_tokens: 300,
        };
        acc.record(&resp2, m2);

        assert_eq!(acc.turns, 8);
        assert!((acc.cost_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(acc.input_tokens, 1300);
        assert_eq!(acc.output_tokens, 500);

        let outcome = acc.into_outcome(serde_json::json!({"test": true}));
        assert_eq!(outcome.turns, 8);
        assert!((outcome.cost_usd - 0.25).abs() < f64::EPSILON);
    }

    // ── truncate_for_log tests ──────────────────────────────────

    #[test]
    fn test_should_not_truncate_short_log_text() {
        assert_eq!(truncate_for_log("hello", 10), "hello");
    }

    #[test]
    fn test_should_truncate_long_log_text() {
        let text = "a".repeat(100);
        let result = truncate_for_log(&text, 50);
        assert_eq!(result.len(), 50);
    }

    #[test]
    fn test_should_truncate_at_utf8_boundary() {
        // "café" = 5 bytes (é is 2 bytes)
        let text = "café";
        let result = truncate_for_log(text, 4);
        // Should not split in the middle of é (byte 4 is inside the é sequence)
        assert_eq!(result, "caf");
    }
}
