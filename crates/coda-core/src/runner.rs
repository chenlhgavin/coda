//! Runner for executing feature development through phased stages.
//!
//! Manages a single continuous `ClaudeClient` session that progresses
//! through dynamic development phases (from the design spec) followed by
//! fixed review → verify quality phases, with state persistence for crash
//! recovery.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_agent_sdk_rs::{ClaudeClient, ContentBlock, Message, ResultMessage, ToolResultContent};
use coda_pm::PromptManager;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn};

use crate::CoreError;
use crate::config::CodaConfig;
use crate::gh::GhOps;
use crate::git::GitOps;
use crate::parser::{
    extract_pr_number, extract_pr_url, parse_review_issues, parse_verification_result,
};
use crate::profile::AgentProfile;
use crate::state::{FeatureState, FeatureStatus, PhaseKind, PhaseStatus};
use crate::task::{Task, TaskResult, TaskStatus};

/// Real-time progress events emitted during a feature run.
///
/// Subscribe to these events via [`Runner::set_progress_sender`] to display
/// live progress in the CLI or UI layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RunEvent {
    /// Emitted once at the beginning of a run with the full phase list.
    RunStarting {
        /// Ordered list of phase names for the entire pipeline.
        phases: Vec<String>,
    },
    /// A phase is about to start executing.
    PhaseStarting {
        /// Phase name (e.g., `"setup"`, `"implement"`).
        name: String,
        /// Zero-based phase index.
        index: usize,
        /// Total number of phases.
        total: usize,
    },
    /// A phase completed successfully.
    PhaseCompleted {
        /// Phase name.
        name: String,
        /// Zero-based phase index.
        index: usize,
        /// Wall-clock duration of the phase.
        duration: Duration,
        /// Number of agent conversation turns used.
        turns: u32,
        /// Cost in USD.
        cost_usd: f64,
    },
    /// A phase failed.
    PhaseFailed {
        /// Phase name.
        name: String,
        /// Zero-based phase index.
        index: usize,
        /// Error description.
        error: String,
    },
    /// A review round has completed.
    ReviewRound {
        /// Current round number (1-based).
        round: u32,
        /// Maximum allowed rounds.
        max_rounds: u32,
        /// Number of issues found in this round.
        issues_found: u32,
    },
    /// A verification attempt has completed.
    VerifyAttempt {
        /// Current attempt number (1-based).
        attempt: u32,
        /// Maximum allowed attempts.
        max_attempts: u32,
        /// Whether all checks passed in this attempt.
        passed: bool,
    },
    /// Creating pull request after all phases.
    CreatingPr,
    /// PR creation completed.
    PrCreated {
        /// PR URL, if successfully extracted from agent response.
        url: Option<String>,
    },
    /// The entire run has finished (success or failure).
    RunFinished {
        /// Whether the run completed successfully.
        success: bool,
    },
}

/// Progress tracking for a multi-phase feature development run.
///
/// Aggregates the results of all completed phases and indicates
/// whether the entire run was successful.
#[derive(Debug)]
pub struct RunProgress {
    /// Completed phase results.
    pub results: Vec<TaskResult>,
    /// Whether the entire run succeeded.
    pub success: bool,
}

/// A commit recorded during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Short SHA of the commit.
    pub sha: String,
    /// Commit message.
    pub message: String,
}

/// Summary of code review results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewSummary {
    /// Number of review rounds performed.
    pub rounds: u32,
    /// Total issues found across all rounds.
    pub issues_found: u32,
    /// Total issues resolved.
    pub issues_resolved: u32,
}

/// Summary of verification results.
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
struct IncrementalMetrics {
    /// Incremental cost in USD for this interaction.
    cost_usd: f64,
    /// Incremental input tokens consumed.
    input_tokens: u64,
    /// Incremental output tokens generated.
    output_tokens: u64,
}

/// Tracks cumulative SDK metrics and computes per-interaction deltas.
///
/// The Claude Agent SDK reports cumulative totals for cost and token usage
/// across the entire session. This tracker maintains the running totals and
/// returns the incremental delta for each interaction.
#[derive(Debug, Default)]
struct MetricsTracker {
    /// Running cumulative cost from the SDK.
    cumulative_cost_usd: f64,
    /// Running cumulative input tokens from the SDK.
    cumulative_input_tokens: u64,
    /// Running cumulative output tokens from the SDK.
    cumulative_output_tokens: u64,
}

impl MetricsTracker {
    /// Records a new SDK result and returns the incremental delta.
    fn record(&mut self, result: &Option<ResultMessage>) -> IncrementalMetrics {
        let new_cost = result
            .as_ref()
            .and_then(|r| r.total_cost_usd)
            .unwrap_or(self.cumulative_cost_usd);
        let cost_delta = (new_cost - self.cumulative_cost_usd).max(0.0);
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

        let input_delta = new_input.saturating_sub(self.cumulative_input_tokens);
        let output_delta = new_output.saturating_sub(self.cumulative_output_tokens);
        self.cumulative_input_tokens = new_input;
        self.cumulative_output_tokens = new_output;

        IncrementalMetrics {
            cost_usd: cost_delta,
            input_tokens: input_delta,
            output_tokens: output_delta,
        }
    }
}

/// Complete outcome of a phase execution.
///
/// Contains all metrics needed to finalize a phase record. Eliminates the
/// "partial initialization" pattern where callers would set status/timing
/// in one call and cost/tokens in separate assignments.
#[derive(Debug)]
struct PhaseOutcome {
    /// Number of agent conversation turns used.
    turns: u32,
    /// Total cost in USD for this phase.
    cost_usd: f64,
    /// Input tokens consumed.
    input_tokens: u64,
    /// Output tokens generated.
    output_tokens: u64,
    /// Wall-clock duration of the phase.
    duration: Duration,
    /// Phase-specific details (flexible schema).
    details: serde_json::Value,
}

/// Accumulates metrics across multiple agent interactions within a single phase.
///
/// Used by multi-round phases (review, verify) where each round involves
/// one or more agent calls and the totals must be aggregated.
#[derive(Debug)]
struct PhaseMetricsAccumulator {
    /// Start time of the phase.
    start: Instant,
    /// Accumulated conversation turns.
    turns: u32,
    /// Accumulated cost in USD.
    cost_usd: f64,
    /// Accumulated input tokens.
    input_tokens: u64,
    /// Accumulated output tokens.
    output_tokens: u64,
}

impl PhaseMetricsAccumulator {
    /// Creates a new accumulator, recording the start time.
    fn new() -> Self {
        Self {
            start: Instant::now(),
            turns: 0,
            cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Records metrics from a single agent interaction.
    fn record(&mut self, resp: &AgentResponse, metrics: IncrementalMetrics) {
        self.turns += resp.result.as_ref().map_or(1, |r| r.num_turns);
        self.cost_usd += metrics.cost_usd;
        self.input_tokens += metrics.input_tokens;
        self.output_tokens += metrics.output_tokens;
    }

    /// Converts accumulated metrics into a [`PhaseOutcome`].
    fn into_outcome(self, details: serde_json::Value) -> PhaseOutcome {
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

/// Collected output from a single agent interaction.
///
/// Separates assistant text from tool execution output so callers can
/// search both independently (e.g., extracting a PR URL from bash stdout).
#[derive(Debug, Default)]
struct AgentResponse {
    /// Text content from assistant messages.
    text: String,
    /// Combined tool result output (bash stdout/stderr, etc.).
    tool_output: String,
    /// SDK result message with metrics.
    result: Option<ResultMessage>,
}

impl AgentResponse {
    /// Returns all collected text (assistant text + tool output) for searching.
    fn all_text(&self) -> String {
        if self.tool_output.is_empty() {
            self.text.clone()
        } else {
            format!("{}\n{}", self.text, self.tool_output)
        }
    }
}

/// Structured run log writer for debugging agent interactions.
///
/// Writes a human-readable log of every prompt/response exchange to
/// `.coda/<slug>/logs/run-<timestamp>.log`, making it easy to diagnose
/// issues like empty responses or failed PR creation.
struct RunLogger {
    file: File,
}

impl RunLogger {
    /// Creates a new logger, writing to `.coda/<slug>/logs/run-<timestamp>.log`.
    ///
    /// Creates the `logs/` directory if it doesn't exist. Returns `None` if
    /// the log file cannot be created (logging is best-effort, not fatal).
    fn new(feature_dir: &Path) -> Option<Self> {
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
    fn log_header(&mut self, feature_slug: &str, model: &str, phases: &[String]) {
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
    fn log_phase_start(&mut self, name: &str, index: usize, total: usize, kind: &str) {
        let _ = writeln!(self.file, "────────────────────────────────");
        let _ = writeln!(self.file, "Phase {}/{total}: {name} [{kind}]", index + 1,);
        let _ = writeln!(self.file, "────────────────────────────────");
        let _ = writeln!(self.file);
    }

    /// Logs a single agent interaction (prompt + response).
    fn log_interaction(
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
    fn log_pr_extraction(
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
    fn log_message(&mut self, msg: &str) {
        let _ = writeln!(self.file, "{msg}");
    }
}

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

/// Orchestrates the execution of a feature through all phases.
///
/// Uses a single continuous `ClaudeClient` session with the Coder profile,
/// preserving context across phases so the agent can reference earlier work.
pub struct Runner {
    client: ClaudeClient,
    pm: PromptManager,
    config: CodaConfig,
    state: FeatureState,
    state_path: PathBuf,
    worktree_path: PathBuf,
    connected: bool,
    review_summary: ReviewSummary,
    verification_summary: VerificationSummary,
    progress_tx: Option<UnboundedSender<RunEvent>>,
    /// Tracks cumulative SDK metrics for incremental delta computation.
    metrics: MetricsTracker,
    /// Per-run structured log for debugging agent interactions.
    run_logger: Option<RunLogger>,
    /// Git operations implementation.
    git: Arc<dyn GitOps>,
    /// GitHub CLI operations implementation.
    gh: Arc<dyn GhOps>,
}

impl Runner {
    /// Creates a new runner for the given feature.
    ///
    /// Loads the feature state from `state.yml` and configures a
    /// `ClaudeClient` with the Coder profile.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the state file cannot be read or the
    /// client cannot be configured.
    pub fn new(
        feature_slug: &str,
        project_root: PathBuf,
        pm: &PromptManager,
        config: &CodaConfig,
        git: Arc<dyn GitOps>,
        gh: Arc<dyn GhOps>,
    ) -> Result<Self, CoreError> {
        // Find feature directory
        let feature_dir = find_feature_dir(&project_root, feature_slug)?;
        let state_path = feature_dir.join("state.yml");

        // Load and validate state
        let state_content = std::fs::read_to_string(&state_path)
            .map_err(|e| CoreError::StateError(format!("Cannot read state.yml: {e}")))?;
        let state: FeatureState = serde_yaml::from_str(&state_content)?;

        state.validate().map_err(|e| {
            CoreError::StateError(format!(
                "Invalid state.yml at {}: {e}",
                state_path.display()
            ))
        })?;

        let worktree_path = project_root.join(&state.git.worktree_path);

        // Load .coda.md for system prompt context
        let coda_md = std::fs::read_to_string(project_root.join(".coda.md")).unwrap_or_default();
        let system_prompt = pm.render("run/system", minijinja::context!(coda_md => coda_md))?;

        // Create client with Coder profile, cwd = worktree
        let options = AgentProfile::Coder.to_options(
            &system_prompt,
            worktree_path.clone(),
            config.agent.max_turns,
            config.agent.max_budget_usd,
            &config.agent.model,
        );

        let client = ClaudeClient::new(options);
        let run_logger = RunLogger::new(&feature_dir);

        Ok(Self {
            client,
            pm: pm.clone(),
            config: config.clone(),
            state,
            state_path,
            worktree_path,
            connected: false,
            review_summary: ReviewSummary::default(),
            verification_summary: VerificationSummary::default(),
            progress_tx: None,
            metrics: MetricsTracker::default(),
            run_logger,
            git,
            gh,
        })
    }

    /// Sets a progress event sender for real-time status reporting.
    ///
    /// When set, the runner emits [`RunEvent`]s at phase transitions so
    /// the caller can display live progress without polling.
    pub fn set_progress_sender(&mut self, tx: UnboundedSender<RunEvent>) {
        self.progress_tx = Some(tx);
    }

    /// Executes all remaining phases from the current checkpoint.
    ///
    /// Connects the client, iterates through phases from `current_phase`,
    /// dispatching each phase based on its [`PhaseKind`]:
    ///
    /// - **Dev** phases are handled by [`run_dev_phase`](Self::run_dev_phase)
    /// - **Quality** phases dispatch to `run_review` or `run_verify` by name
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if any phase fails after all retries.
    pub async fn execute(&mut self) -> Result<Vec<TaskResult>, CoreError> {
        // Connect to Claude
        self.client
            .connect()
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;
        self.connected = true;

        // Mark feature as in progress
        self.state.status = FeatureStatus::InProgress;
        self.save_state()?;

        let mut results = Vec::new();
        let total_phases = self.state.phases.len();

        // Determine start phase from actual phase statuses, not the
        // `current_phase` counter which can be stale after a crash between
        // `mark_phase_completed` and the counter increment.
        let start_phase = self
            .state
            .phases
            .iter()
            .position(|p| p.status != PhaseStatus::Completed)
            .unwrap_or(total_phases);

        // Sync the counter to match the computed start phase
        if start_phase < total_phases {
            self.state.current_phase = start_phase as u32;
        }

        if start_phase > 0 {
            info!(
                start_phase = start_phase,
                total = total_phases,
                "Resuming from phase {} (skipping {} completed)",
                self.state
                    .phases
                    .get(start_phase)
                    .map_or("create-pr", |p| p.name.as_str()),
                start_phase,
            );
        }

        // Emit initial phase list so the UI can display the full pipeline
        let phase_names: Vec<String> = self.state.phases.iter().map(|p| p.name.clone()).collect();
        self.emit_event(RunEvent::RunStarting {
            phases: phase_names.clone(),
        });

        if let Some(logger) = &mut self.run_logger {
            logger.log_header(
                &self.state.feature.slug,
                &self.config.agent.model,
                &phase_names,
            );
        }

        for phase_idx in start_phase..total_phases {
            let phase_name = self.state.phases[phase_idx].name.clone();
            let phase_kind = self.state.phases[phase_idx].kind.clone();

            info!(phase = %phase_name, index = phase_idx, "Starting phase");

            let kind_str = match &phase_kind {
                PhaseKind::Dev => "dev",
                PhaseKind::Quality => "quality",
            };
            if let Some(logger) = &mut self.run_logger {
                logger.log_phase_start(&phase_name, phase_idx, total_phases, kind_str);
            }

            self.emit_event(RunEvent::PhaseStarting {
                name: phase_name.clone(),
                index: phase_idx,
                total: total_phases,
            });

            let result = match phase_kind {
                PhaseKind::Dev => self.run_dev_phase(phase_idx).await,
                PhaseKind::Quality => match phase_name.as_str() {
                    "review" => self.run_review(phase_idx).await,
                    "verify" => self.run_verify(phase_idx).await,
                    _ => Err(CoreError::AgentError(format!(
                        "Unknown quality phase: {phase_name}"
                    ))),
                },
            };

            match result {
                Ok(task_result) => {
                    info!(
                        phase = %phase_name,
                        turns = task_result.turns,
                        cost_usd = task_result.cost_usd,
                        "Phase completed"
                    );
                    self.emit_event(RunEvent::PhaseCompleted {
                        name: phase_name.clone(),
                        index: phase_idx,
                        duration: task_result.duration,
                        turns: task_result.turns,
                        cost_usd: task_result.cost_usd,
                    });
                    results.push(task_result);

                    // Advance current_phase as a secondary checkpoint
                    self.state.current_phase = ((phase_idx + 1).min(total_phases)) as u32;
                    self.save_state()?;
                }
                Err(e) => {
                    error!(phase = %phase_name, error = %e, "Phase failed");
                    if let Some(logger) = &mut self.run_logger {
                        logger.log_message(&format!(
                            "✗ Phase {phase_name} FAILED: {e}\n  Aborting run.\n",
                        ));
                    }
                    self.emit_event(RunEvent::PhaseFailed {
                        name: phase_name.clone(),
                        index: phase_idx,
                        error: e.to_string(),
                    });
                    self.state.phases[phase_idx].status = PhaseStatus::Failed;
                    self.state.status = FeatureStatus::Failed;
                    self.save_state()?;
                    return Err(e);
                }
            }
        }

        // Compute totals before creating PR so the PR body has accurate stats
        // (excludes create_pr phase itself, which is a meta-operation)
        self.update_totals();
        self.save_state()?;
        self.commit_coda_state()?;

        // All phases complete — create PR
        info!("All phases complete, creating PR...");
        self.emit_event(RunEvent::CreatingPr);
        let pr_result = self.create_pr().await?;

        // Extract PR URL before pushing result
        let pr_url = self.state.pr.as_ref().map(|pr| pr.url.clone());
        self.emit_event(RunEvent::PrCreated { url: pr_url });

        let pr_succeeded = matches!(pr_result.status, TaskStatus::Completed);
        results.push(pr_result);

        // Mark feature status based on PR outcome
        if pr_succeeded {
            self.state.status = FeatureStatus::Completed;
        } else {
            // Code phases completed but PR creation failed.
            // Keep InProgress so a re-run only retries PR creation.
            warn!("Feature development complete but PR creation failed");
        }
        self.update_totals();
        self.save_state()?;

        // Commit and push final state (PR info, status, log) so the PR
        // branch includes all execution metadata.
        self.commit_coda_state()?;
        let branch = &self.state.git.branch.clone();
        self.git.push(&self.worktree_path, branch)?;

        self.emit_event(RunEvent::RunFinished {
            success: pr_succeeded,
        });

        // Disconnect
        if self.connected {
            let _ = self.client.disconnect().await;
            self.connected = false;
        }

        Ok(results)
    }

    /// Executes a development phase from the design spec.
    ///
    /// Renders the `run/dev_phase` prompt template with the phase name,
    /// index, and design spec, then sends it to the agent.
    async fn run_dev_phase(&mut self, phase_idx: usize) -> Result<TaskResult, CoreError> {
        let was_running = self.state.phases[phase_idx].status == PhaseStatus::Running;
        self.mark_phase_running(phase_idx);

        let mut acc = PhaseMetricsAccumulator::new();

        let design_spec = self.load_spec("design.md")?;
        let checks = &self.config.checks;
        let feature_slug = self.state.feature.slug.clone();
        let phase_name = self.state.phases[phase_idx].name.clone();

        // Determine the 1-based phase number among dev phases
        let dev_phase_number = self
            .state
            .phases
            .iter()
            .take(phase_idx + 1)
            .filter(|p| p.kind == PhaseKind::Dev)
            .count();
        let total_dev_phases = self
            .state
            .phases
            .iter()
            .filter(|p| p.kind == PhaseKind::Dev)
            .count();
        let is_first = phase_idx == 0;

        // Build resume context if resuming mid-phase
        let resume_context = if was_running {
            self.build_resume_context()?
        } else {
            String::new()
        };

        let prompt = self.pm.render(
            "run/dev_phase",
            minijinja::context!(
                design_spec => design_spec,
                phase_name => phase_name,
                phase_number => dev_phase_number,
                total_dev_phases => total_dev_phases,
                is_first => is_first,
                checks => checks,
                feature_slug => feature_slug,
                resume_context => resume_context,
            ),
        )?;

        let resp = self.send_and_collect(&prompt).await?;
        let incremental = self.metrics.record(&resp.result);
        if let Some(logger) = &mut self.run_logger {
            logger.log_interaction(&prompt, &resp, &incremental);
        }
        acc.record(&resp, incremental);

        let outcome = acc.into_outcome(serde_json::json!({}));
        let task_result = TaskResult {
            task: Task::DevPhase {
                name: phase_name,
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: outcome.turns,
            cost_usd: outcome.cost_usd,
            duration: outcome.duration,
            artifacts: vec![],
        };
        self.complete_phase(phase_idx, outcome);

        Ok(task_result)
    }

    /// Executes the review phase with fix loop.
    ///
    /// Sends the diff for review, parses the YAML response for issues,
    /// and if critical/major issues are found, asks the agent to fix them
    /// and re-reviews. Loops up to `max_review_rounds`.
    async fn run_review(&mut self, phase_idx: usize) -> Result<TaskResult, CoreError> {
        self.mark_phase_running(phase_idx);

        if !self.config.review.enabled {
            info!("Code review disabled, skipping");
            let outcome = PhaseOutcome {
                turns: 0,
                cost_usd: 0.0,
                input_tokens: 0,
                output_tokens: 0,
                duration: Duration::ZERO,
                details: serde_json::json!({}),
            };
            let task_result = TaskResult {
                task: Task::Review {
                    feature_slug: self.state.feature.slug.clone(),
                },
                status: TaskStatus::Completed,
                turns: 0,
                cost_usd: 0.0,
                duration: Duration::ZERO,
                artifacts: vec![],
            };
            self.complete_phase(phase_idx, outcome);
            return Ok(task_result);
        }

        let design_spec = self.load_spec("design.md")?;
        let max_rounds = self.config.review.max_review_rounds;
        let mut acc = PhaseMetricsAccumulator::new();

        for round in 0..max_rounds {
            info!(round = round + 1, max = max_rounds, "Review round");

            let diff = self.get_diff()?;
            let review_prompt = self.pm.render(
                "run/review",
                minijinja::context!(
                    design_spec => design_spec,
                    diff => diff,
                ),
            )?;

            let resp = self.send_and_collect(&review_prompt).await?;
            let m = self.metrics.record(&resp.result);
            if let Some(logger) = &mut self.run_logger {
                logger.log_interaction(&review_prompt, &resp, &m);
            }
            acc.record(&resp, m);

            self.review_summary.rounds += 1;

            // Parse review issues from response
            let issues = parse_review_issues(&resp.text);
            let issue_count = issues.len() as u32;
            self.review_summary.issues_found += issue_count;

            self.emit_event(RunEvent::ReviewRound {
                round: round + 1,
                max_rounds,
                issues_found: issue_count,
            });

            if issues.is_empty() {
                info!("No critical/major issues found, review passed");
                break;
            }

            info!(issues = issue_count, "Found issues, asking agent to fix");

            // Ask agent to fix the issues with design spec context
            let issues_list = issues
                .iter()
                .enumerate()
                .map(|(i, issue)| format!("{}. {}", i + 1, issue))
                .collect::<Vec<_>>()
                .join("\n");
            let fix_prompt = format!(
                "The code review found {issue_count} critical/major issues that must be fixed.\n\n\
                 ## Issues\n\n{issues_list}\n\n\
                 ## Instructions\n\n\
                 1. Fix each issue listed above\n\
                 2. Run the configured checks to ensure nothing is broken\n\
                 3. Commit the fixes with a descriptive message\n\n\
                 Refer to the design specification provided earlier for the intended behavior.",
            );

            let fix_resp = self.send_and_collect(&fix_prompt).await?;
            let fm = self.metrics.record(&fix_resp.result);
            if let Some(logger) = &mut self.run_logger {
                logger.log_interaction(&fix_prompt, &fix_resp, &fm);
            }
            acc.record(&fix_resp, fm);

            self.review_summary.issues_resolved += issue_count;
        }

        let outcome = acc.into_outcome(serde_json::json!({
            "rounds": self.review_summary.rounds,
            "issues_found": self.review_summary.issues_found,
            "issues_resolved": self.review_summary.issues_resolved,
        }));
        let task_result = TaskResult {
            task: Task::Review {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: outcome.turns,
            cost_usd: outcome.cost_usd,
            duration: outcome.duration,
            artifacts: vec![],
        };
        self.complete_phase(phase_idx, outcome);

        Ok(task_result)
    }

    /// Executes the verify phase with fix loop.
    ///
    /// Runs the verification plan, and if any check fails, asks the
    /// agent to fix the issue and re-verifies.
    async fn run_verify(&mut self, phase_idx: usize) -> Result<TaskResult, CoreError> {
        self.mark_phase_running(phase_idx);

        let verification_spec = self.load_spec("verification.md")?;
        let checks = self.config.checks.clone();
        let max_attempts = self.config.agent.max_retries;
        let mut acc = PhaseMetricsAccumulator::new();

        for attempt in 0..=max_attempts {
            info!(
                attempt = attempt + 1,
                max = max_attempts + 1,
                "Verification attempt"
            );

            let verify_prompt = self.pm.render(
                "run/verify",
                minijinja::context!(
                    verification_spec => verification_spec,
                    checks => &checks,
                ),
            )?;

            let resp = self.send_and_collect(&verify_prompt).await?;
            let m = self.metrics.record(&resp.result);
            if let Some(logger) = &mut self.run_logger {
                logger.log_interaction(&verify_prompt, &resp, &m);
            }
            acc.record(&resp, m);

            // Parse verification result
            let (passed, failed_details) = parse_verification_result(&resp.text);
            self.verification_summary.checks_total = passed + failed_details.len() as u32;
            self.verification_summary.checks_passed = passed;

            let all_passed = failed_details.is_empty();
            self.emit_event(RunEvent::VerifyAttempt {
                attempt: attempt + 1,
                max_attempts: max_attempts + 1,
                passed: all_passed,
            });

            if all_passed {
                info!("All verification checks passed");
                break;
            }

            if attempt == max_attempts {
                warn!("Max verification attempts reached, proceeding with failures");
                break;
            }

            info!(
                failures = failed_details.len(),
                "Verification failed, asking agent to fix"
            );

            let failures = failed_details.join("\n");
            let checks_str = checks.join("`, `");
            let fix_prompt = format!(
                "Verification failed. The following checks did not pass:\n\n\
                 ## Failed Checks\n\n{failures}\n\n\
                 ## Instructions\n\n\
                 1. Analyze each failure and identify the root cause\n\
                 2. Fix the code to address each failure\n\
                 3. Re-run all checks: `{checks_str}`\n\
                 4. Ensure ALL checks pass before reporting back\n\n\
                 Refer to the design specification and verification plan provided earlier.",
            );

            let fix_resp = self.send_and_collect(&fix_prompt).await?;
            let fm = self.metrics.record(&fix_resp.result);
            if let Some(logger) = &mut self.run_logger {
                logger.log_interaction(&fix_prompt, &fix_resp, &fm);
            }
            acc.record(&fix_resp, fm);
        }

        let outcome = acc.into_outcome(serde_json::json!({
            "attempts": self.verification_summary.checks_total,
            "checks_passed": self.verification_summary.checks_passed,
            "checks_total": self.verification_summary.checks_total,
        }));
        let task_result = TaskResult {
            task: Task::Verify {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: outcome.turns,
            cost_usd: outcome.cost_usd,
            duration: outcome.duration,
            artifacts: vec![],
        };
        self.complete_phase(phase_idx, outcome);

        Ok(task_result)
    }

    /// Creates a pull request after all phases complete.
    ///
    /// Sends a PR creation prompt to the agent, then extracts the PR URL from:
    /// 1. Assistant text response
    /// 2. Tool result output (bash stdout from `gh pr create`)
    /// 3. Fallback: queries `gh pr list --head <branch>` directly
    ///
    /// Returns `TaskStatus::Failed` if no PR could be found after all attempts.
    async fn create_pr(&mut self) -> Result<TaskResult, CoreError> {
        let design_spec = self.load_spec("design.md")?;
        let commits = self.get_commits()?;
        let checks = &self.config.checks;
        let start = Instant::now();

        let all_checks_passed = self.verification_summary.checks_passed
            == self.verification_summary.checks_total
            && self.verification_summary.checks_total > 0;
        let is_draft = !all_checks_passed;
        let model = &self.config.agent.model;
        let coda_version = env!("CARGO_PKG_VERSION");

        let pr_prompt = self.pm.render(
            "run/create_pr",
            minijinja::context!(
                design_spec => design_spec,
                commits => commits,
                state => &self.state,
                checks => checks,
                review_summary => &self.review_summary,
                verification_summary => &self.verification_summary,
                all_checks_passed => all_checks_passed,
                is_draft => is_draft,
                model => model,
                coda_version => coda_version,
            ),
        )?;

        let resp = self.send_and_collect(&pr_prompt).await?;
        let pr_metrics = self.metrics.record(&resp.result);
        if let Some(logger) = &mut self.run_logger {
            logger.log_interaction(&pr_prompt, &resp, &pr_metrics);
        }

        // Try to extract PR URL from all collected text (assistant text + tool output)
        let all_text = resp.all_text();
        let url_from_text = extract_pr_url(&all_text);

        let url_from_gh = if url_from_text.is_none() {
            info!("PR URL not found in agent response, checking via gh CLI...");
            self.check_pr_exists_via_gh()
        } else {
            None
        };

        let pr_url = url_from_text.clone().or(url_from_gh.clone());

        if let Some(logger) = &mut self.run_logger {
            logger.log_pr_extraction(
                url_from_text.as_deref(),
                url_from_gh.as_deref(),
                pr_url.as_deref(),
            );
        }

        let status = if let Some(ref url) = pr_url {
            info!(url = %url, "PR created");
            self.state.pr = Some(crate::state::PrInfo {
                url: url.clone(),
                number: extract_pr_number(url).unwrap_or(0),
                title: format!("feat({}): feature implementation", self.state.feature.slug),
            });
            self.save_state()?;
            TaskStatus::Completed
        } else {
            let msg = "PR creation failed: no PR URL found in agent response or via gh CLI";
            warn!(msg);
            TaskStatus::Failed {
                error: msg.to_string(),
            }
        };

        Ok(TaskResult {
            task: Task::CreatePr {
                feature_slug: self.state.feature.slug.clone(),
            },
            status,
            turns: resp.result.as_ref().map_or(1, |r| r.num_turns),
            cost_usd: pr_metrics.cost_usd,
            duration: start.elapsed(),
            artifacts: vec![],
        })
    }

    // ── Helper Methods ──────────────────────────────────────────────

    /// Emits a progress event to the subscriber, if one is registered.
    ///
    /// Silently ignores send failures (e.g., if the receiver was dropped).
    fn emit_event(&self, event: RunEvent) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.send(event);
        }
    }

    /// Commits any pending `.coda/` changes in the worktree.
    ///
    /// Stages the `.coda/` directory and creates a commit if there are
    /// staged changes. This ensures execution state (state.yml, specs)
    /// is tracked in git alongside the feature code.
    ///
    /// Silently succeeds if there are no changes to commit.
    fn commit_coda_state(&self) -> Result<(), CoreError> {
        self.git.add(&self.worktree_path, &[".coda/"])?;

        if self.git.has_staged_changes(&self.worktree_path) {
            let msg = format!("chore({}): update execution state", self.state.feature.slug);
            self.git.commit(&self.worktree_path, &msg)?;
            info!("Committed .coda/ state updates");
        } else {
            debug!("No .coda/ changes to commit");
        }

        Ok(())
    }

    /// Sends a prompt and collects the full response text, tool output, and `ResultMessage`.
    ///
    /// Captures both assistant text blocks and tool result content (e.g., bash
    /// stdout from `gh pr create`) so callers can search all output for
    /// expected patterns.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` if the agent returns an empty response
    /// (no text and no tool output), which indicates a broken session.
    async fn send_and_collect(&mut self, prompt: &str) -> Result<AgentResponse, CoreError> {
        self.client
            .query(prompt)
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;

        let mut resp = AgentResponse::default();

        {
            let mut stream = self.client.receive_response();
            while let Some(result) = stream.next().await {
                let msg = result.map_err(|e| CoreError::AgentError(e.to_string()))?;
                match msg {
                    Message::Assistant(assistant) => {
                        for block in &assistant.message.content {
                            match block {
                                ContentBlock::Text(text) => {
                                    resp.text.push_str(&text.text);
                                }
                                ContentBlock::ToolResult(tr) => {
                                    collect_tool_result_text(
                                        tr.content.as_ref(),
                                        &mut resp.tool_output,
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                    Message::User(user) => {
                        if let Some(blocks) = &user.content {
                            for block in blocks {
                                if let ContentBlock::ToolResult(tr) = block {
                                    collect_tool_result_text(
                                        tr.content.as_ref(),
                                        &mut resp.tool_output,
                                    );
                                }
                            }
                        }
                    }
                    Message::Result(r) => {
                        resp.result = Some(r);
                        break;
                    }
                    _ => {}
                }
            }
        }

        if resp.text.is_empty() && resp.tool_output.is_empty() {
            let reason = resp
                .result
                .as_ref()
                .map(|r| {
                    format!(
                        "turns={}, cost={:?}, is_error={}",
                        r.num_turns, r.total_cost_usd, r.is_error,
                    )
                })
                .unwrap_or_else(|| "no ResultMessage received".to_string());

            error!(reason = %reason, "Agent returned empty response");
            if let Some(logger) = &mut self.run_logger {
                logger.log_message(&format!(
                    "⚠ EMPTY RESPONSE detected\n  prompt_len={}\n  reason: {reason}",
                    prompt.len(),
                ));
            }

            return Err(CoreError::AgentError(format!(
                "Agent returned empty response (session may be disconnected): {reason}",
            )));
        }

        Ok(resp)
    }

    /// Marks a phase as running and saves state.
    fn mark_phase_running(&mut self, phase_idx: usize) {
        self.state.phases[phase_idx].status = PhaseStatus::Running;
        self.state.phases[phase_idx].started_at = Some(chrono::Utc::now());
        if let Err(e) = self.save_state() {
            warn!(error = %e, "Failed to save state when marking phase as running");
        }
    }

    /// Finalizes a phase with the complete outcome.
    ///
    /// Sets all phase-record fields atomically from the [`PhaseOutcome`],
    /// ensuring no caller can forget to set cost or token counts.
    fn complete_phase(&mut self, phase_idx: usize, outcome: PhaseOutcome) {
        let phase = &mut self.state.phases[phase_idx];
        phase.status = PhaseStatus::Completed;
        phase.completed_at = Some(chrono::Utc::now());
        phase.turns = outcome.turns;
        phase.cost_usd = outcome.cost_usd;
        phase.cost.input_tokens = outcome.input_tokens;
        phase.cost.output_tokens = outcome.output_tokens;
        phase.duration_secs = outcome.duration.as_secs();
        phase.details = outcome.details;
        self.state.feature.updated_at = chrono::Utc::now();

        if let Err(e) = self.save_state() {
            warn!(error = %e, "Failed to save state after completing phase");
        }
    }

    /// Persists the current state to `state.yml`.
    fn save_state(&self) -> Result<(), CoreError> {
        let yaml = serde_yaml::to_string(&self.state)?;
        std::fs::write(&self.state_path, yaml).map_err(CoreError::IoError)?;
        debug!(path = %self.state_path.display(), "State saved");
        Ok(())
    }

    /// Loads a spec file from the worktree's `.coda/<slug>/specs/` directory.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::StateError` if the spec file cannot be read.
    fn load_spec(&self, filename: &str) -> Result<String, CoreError> {
        let spec_path = self
            .worktree_path
            .join(".coda")
            .join(&self.state.feature.slug)
            .join("specs")
            .join(filename);

        std::fs::read_to_string(&spec_path).map_err(|e| {
            CoreError::StateError(format!("Cannot read spec at {}: {e}", spec_path.display()))
        })
    }

    /// Gets the git diff of all changes from the base branch.
    fn get_diff(&self) -> Result<String, CoreError> {
        self.git
            .diff(&self.worktree_path, &self.state.git.base_branch)
    }

    /// Gets the list of commits from the base branch to HEAD.
    fn get_commits(&self) -> Result<Vec<CommitInfo>, CoreError> {
        let range = format!("{}..HEAD", self.state.git.base_branch);
        let stdout = self.git.log_oneline(&self.worktree_path, &range)?;

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

    /// Builds resume context for interrupted executions.
    fn build_resume_context(&self) -> Result<String, CoreError> {
        let completed_phases: Vec<serde_json::Value> = self
            .state
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

        let current_phase_name = &self.state.phases[self.state.current_phase as usize].name;
        let current_phase_state = &self.state.phases[self.state.current_phase as usize];

        self.pm
            .render(
                "run/resume",
                minijinja::context!(
                    state => &self.state,
                    completed_phases => completed_phases,
                    current_phase => current_phase_name,
                    current_phase_state => current_phase_state,
                ),
            )
            .map_err(CoreError::from)
    }

    /// Updates cumulative totals from all phase records.
    fn update_totals(&mut self) {
        let mut total_turns = 0u32;
        let mut total_cost = 0.0f64;
        let mut total_duration = 0u64;
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;

        for phase in &self.state.phases {
            total_turns += phase.turns;
            total_cost += phase.cost_usd;
            total_duration += phase.duration_secs;
            total_input_tokens += phase.cost.input_tokens;
            total_output_tokens += phase.cost.output_tokens;
        }

        self.state.total.turns = total_turns;
        self.state.total.cost_usd = total_cost;
        self.state.total.duration_secs = total_duration;
        self.state.total.cost.input_tokens = total_input_tokens;
        self.state.total.cost.output_tokens = total_output_tokens;
    }

    /// Checks whether a PR exists for the given branch using `gh pr list`.
    ///
    /// Falls back to querying the GitHub CLI directly when the agent's text
    /// response does not contain an extractable PR URL.
    fn check_pr_exists_via_gh(&self) -> Option<String> {
        let branch = &self.state.git.branch;
        self.gh.pr_url_for_branch(branch, &self.worktree_path)
    }
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("feature", &self.state.feature.slug)
            .field("current_phase", &self.state.current_phase)
            .field("worktree", &self.worktree_path)
            .finish_non_exhaustive()
    }
}

// ── Free Functions ──────────────────────────────────────────────────

/// Extracts text content from a `ToolResultContent` and appends it to the buffer.
fn collect_tool_result_text(content: Option<&ToolResultContent>, buf: &mut String) {
    match content {
        Some(ToolResultContent::Text(text)) => {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
        Some(ToolResultContent::Blocks(blocks)) => {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
            }
        }
        None => {}
    }
}

/// Runs an external command and returns its stdout, checking the exit status.
///
/// Finds the `.coda/<id>-<slug>` directory for a feature by its slug.
///
/// Finds the feature's `.coda/<slug>/` directory inside its worktree.
///
/// Scans `.trees/` for a worktree matching the slug, then returns
/// the path `<worktree>/.coda/<slug>/` which contains `state.yml`
/// and the specs.
///
/// # Errors
///
/// Returns `CoreError::ConfigError` if `.trees/` does not exist, or
/// `CoreError::StateError` if no matching feature directory is found.
fn find_feature_dir(project_root: &Path, feature_slug: &str) -> Result<PathBuf, CoreError> {
    let trees_dir = project_root.join(".trees");
    if !trees_dir.is_dir() {
        return Err(CoreError::ConfigError(format!(
            "No .trees/ directory found at {}. Run `coda init` first.",
            trees_dir.display()
        )));
    }

    // Look for a worktree whose name matches the slug
    let worktree_path = trees_dir.join(feature_slug);
    let feature_dir = worktree_path.join(".coda").join(feature_slug);

    if feature_dir.is_dir() && feature_dir.join("state.yml").is_file() {
        return Ok(feature_dir);
    }

    // Fall back: scan all worktrees for a matching slug directory
    let entries = std::fs::read_dir(&trees_dir).map_err(CoreError::IoError)?;
    let mut available_features = Vec::new();

    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        let candidate = entry.path().join(".coda").join(feature_slug);
        if candidate.is_dir() && candidate.join("state.yml").is_file() {
            return Ok(candidate);
        }

        // Collect available features for hint message
        let coda_dir = entry.path().join(".coda");
        if coda_dir.is_dir()
            && let Ok(coda_entries) = std::fs::read_dir(&coda_dir)
        {
            for ce in coda_entries.flatten() {
                if ce.file_type().is_ok_and(|ft| ft.is_dir())
                    && ce.path().join("state.yml").is_file()
                {
                    available_features.push(ce.file_name().to_string_lossy().to_string());
                }
            }
        }

        // Also count the worktree name itself if it has no inner coda dir
        if available_features.is_empty() {
            available_features.push(name_str.to_string());
        }
    }

    let hint = if available_features.is_empty() {
        "No features have been planned yet.".to_string()
    } else {
        format!("Available features: {}", available_features.join(", "))
    };

    Err(CoreError::StateError(format!(
        "No feature directory found for slug '{feature_slug}'. {hint}\nRun `coda plan {feature_slug}` first.",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_compute_incremental_metrics_from_result() {
        let mut tracker = MetricsTracker::default();

        // First interaction
        let result1 = ResultMessage {
            subtype: "success".to_string(),
            duration_ms: 1000,
            duration_api_ms: 800,
            is_error: false,
            num_turns: 3,
            session_id: "test".to_string(),
            total_cost_usd: Some(0.50),
            usage: Some(serde_json::json!({
                "input_tokens": 1000,
                "output_tokens": 500,
            })),
            result: None,
            structured_output: None,
        };

        let m1 = tracker.record(&Some(result1));
        assert!((m1.cost_usd - 0.50).abs() < f64::EPSILON);
        assert_eq!(m1.input_tokens, 1000);
        assert_eq!(m1.output_tokens, 500);

        // Second interaction (cumulative values)
        let result2 = ResultMessage {
            subtype: "success".to_string(),
            duration_ms: 2000,
            duration_api_ms: 1600,
            is_error: false,
            num_turns: 2,
            session_id: "test".to_string(),
            total_cost_usd: Some(0.80),
            usage: Some(serde_json::json!({
                "input_tokens": 2500,
                "output_tokens": 1200,
            })),
            result: None,
            structured_output: None,
        };

        let m2 = tracker.record(&Some(result2));
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

        assert_eq!(acc.turns, 8); // 3 + 5
        assert!((acc.cost_usd - 0.25).abs() < f64::EPSILON);
        assert_eq!(acc.input_tokens, 1300);
        assert_eq!(acc.output_tokens, 500);

        let outcome = acc.into_outcome(serde_json::json!({"test": true}));
        assert_eq!(outcome.turns, 8);
        assert!((outcome.cost_usd - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_collect_agent_response_all_text() {
        let resp = AgentResponse {
            text: "assistant text".to_string(),
            tool_output: "tool output".to_string(),
            result: None,
        };
        let all = resp.all_text();
        assert!(all.contains("assistant text"));
        assert!(all.contains("tool output"));

        let resp_no_tool = AgentResponse {
            text: "only text".to_string(),
            tool_output: String::new(),
            result: None,
        };
        assert_eq!(resp_no_tool.all_text(), "only text");
    }
}
