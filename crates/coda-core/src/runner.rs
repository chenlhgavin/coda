//! Runner for executing feature development through phased stages.
//!
//! Thin orchestrator that connects the [`AgentSession`] and dispatches
//! each phase to the appropriate [`PhaseExecutor`](crate::phases::PhaseExecutor).
//! All phase-specific logic lives in the `phases` module; this module
//! owns the top-level lifecycle: connect → iterate phases → create PR → push.
//!
//! The agent interaction logic (streaming, timeout, reconnect) is delegated
//! to [`AgentSession`](crate::session::AgentSession). Phase-specific behaviour
//! (dev, review, verify, docs) lives in [`crate::phases`].
//!
//! # State Management
//!
//! All state persistence and transition validation is delegated to
//! [`StateManager`](crate::state::StateManager). The `current_phase`
//! counter is derived from phase statuses rather than tracked
//! independently, eliminating divergence after crashes.
//!
//! # Cancellation
//!
//! A [`CancellationToken`] is passed through [`Runner::new`] and stored in
//! [`PhaseContext`]. Between phases, the runner checks
//! [`PhaseContext::check_cancelled`] and returns [`CoreError::Cancelled`]
//! if the token has been triggered. The token is also forwarded to the
//! [`AgentSession`] so that in-flight streaming is interrupted promptly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use coda_pm::PromptManager;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::CoreError;
use crate::async_ops::{AsyncGhOps, AsyncGitOps};
use crate::config::CodaConfig;
use crate::gh::GhOps;
use crate::git::GitOps;
use crate::phases::dev::DevPhaseExecutor;
use crate::phases::docs::DocsPhaseExecutor;
use crate::phases::pr::{create_pr, prepare_squash};
use crate::phases::review::ReviewPhaseExecutor;
use crate::phases::verify::VerifyPhaseExecutor;
use crate::phases::{
    MetricsTracker, PhaseContext, PhaseExecutor, ReviewSummary, RunLogger, VerificationSummary,
};
use crate::profile::AgentProfile;
use crate::session::{AgentSession, SessionConfig, SessionEvent};
use crate::state::{FeatureState, FeatureStatus, PhaseKind, PhaseStatus, StateManager};
use crate::task::{TaskResult, TaskStatus};

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
    /// A specific reviewer (Claude or Codex) completed its review within a round.
    ReviewerCompleted {
        /// Reviewer name (`"claude"` or `"codex"`).
        reviewer: String,
        /// Number of issues this reviewer found.
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
    /// An agent turn completed within the current phase.
    TurnCompleted {
        /// Number of turns completed so far in this phase.
        current_turn: u32,
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
    /// Incremental text delta from the assistant (token-level streaming).
    ///
    /// Emitted when `include_partial_messages` is enabled and the SDK
    /// delivers a `content_block_delta` with a `text_delta` payload.
    ///
    /// # Example
    ///
    /// ```
    /// # use coda_core::RunEvent;
    /// let event = RunEvent::AgentTextDelta {
    ///     text: "Hello".to_string(),
    /// };
    /// ```
    AgentTextDelta {
        /// The text fragment to append to the streaming buffer.
        text: String,
    },
    /// A tool invocation observed during agent execution.
    ///
    /// Emitted when a `Message::Assistant` contains a `ContentBlock::ToolUse`
    /// block, providing visibility into which tools the agent is invoking.
    ///
    /// # Example
    ///
    /// ```
    /// # use coda_core::RunEvent;
    /// let event = RunEvent::ToolActivity {
    ///     tool_name: "Bash".to_string(),
    ///     summary: "cargo build".to_string(),
    /// };
    /// ```
    ToolActivity {
        /// Tool name (e.g., `"Bash"`, `"Write"`, `"Read"`, `"Glob"`, `"Grep"`).
        tool_name: String,
        /// Brief summary of the tool input (file path, command, pattern, etc.).
        summary: String,
    },
    /// Emitted before connecting to the Claude CLI subprocess.
    ///
    /// Allows the TUI to display a "Connecting..." status so the user
    /// knows initialization is in progress.
    Connecting,
    /// Stderr output received from the Claude CLI subprocess.
    ///
    /// Surfaces CLI errors (authentication failures, rate limits, etc.)
    /// that would otherwise be silently dropped.
    StderrOutput {
        /// A single line of stderr output from the CLI.
        line: String,
    },
    /// Emitted when an idle timeout fires but retries remain.
    ///
    /// Gives the user visibility that the agent has been silent and the
    /// system is about to retry (or abort if retries are exhausted).
    IdleWarning {
        /// Which retry attempt this is (1-based).
        attempt: u32,
        /// Maximum retries before aborting.
        max_retries: u32,
        /// How many seconds of silence elapsed.
        idle_secs: u64,
    },
    /// Emitted when the agent subprocess is being reconnected after an
    /// idle timeout.
    ///
    /// The old subprocess is killed, a new one is spawned, and the
    /// prompt is re-sent. This is a recovery mechanism — partial data
    /// from the stalled session is discarded.
    Reconnecting {
        /// Which reconnection attempt this is (1-based).
        attempt: u32,
        /// Maximum reconnection attempts before aborting.
        max_retries: u32,
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

/// Orchestrates the execution of a feature through all phases.
///
/// Uses a single continuous [`AgentSession`] with the Coder profile,
/// preserving context across phases so the agent can reference earlier work.
/// Creates a [`PhaseContext`] and dispatches each phase to the appropriate
/// [`PhaseExecutor`]. State persistence is delegated to [`StateManager`].
pub struct Runner {
    /// Shared context passed to each phase executor.
    ctx: PhaseContext,
}

impl Runner {
    /// Creates a new runner for the given feature.
    ///
    /// Loads the feature state from `state.yml` and configures an
    /// [`AgentSession`] with the Coder profile. Enables partial message
    /// streaming so the UI can display real-time text deltas.
    ///
    /// When resuming an interrupted run, the remaining budget is computed
    /// from the total configured budget minus the cost already spent
    /// across completed phases, so the SDK receives the correct reduced
    /// budget rather than the full maximum.
    ///
    /// The `cancel_token` is forwarded to both the [`AgentSession`] (for
    /// in-flight streaming interruption) and the [`PhaseContext`] (for
    /// between-phase cancellation checks).
    ///
    /// # Errors
    ///
    /// Returns `CoreError::BudgetExhausted` if prior phases already spent
    /// the full budget.
    /// Returns `CoreError` if the state file cannot be read or the
    /// client cannot be configured.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        feature_slug: &str,
        project_root: PathBuf,
        pm: &PromptManager,
        config: &CodaConfig,
        git: Arc<dyn GitOps>,
        gh: Arc<dyn GhOps>,
        progress_tx: Option<UnboundedSender<RunEvent>>,
        cancel_token: CancellationToken,
    ) -> Result<Self, CoreError> {
        // Find feature directory
        let feature_dir = find_feature_dir(&project_root, feature_slug)?;
        let state_path = feature_dir.join("state.yml");

        // Load, migrate, and validate state
        let state_content = std::fs::read_to_string(&state_path)
            .map_err(|e| CoreError::StateError(format!("Cannot read state.yml: {e}")))?;
        let mut state: FeatureState = serde_yaml_ng::from_str(&state_content)?;

        // Migrate legacy states (e.g. missing update-docs phase) before validation.
        state.migrate();

        state.validate().map_err(|e| {
            CoreError::StateError(format!(
                "Invalid state.yml at {}: {e}",
                state_path.display()
            ))
        })?;

        let worktree_path = project_root.join(&state.git.worktree_path);

        // Compute remaining budget: deduct cost of already-completed phases
        // so resumed runs don't over-spend the configured maximum.
        let spent = state.spent_budget();
        let remaining_budget = (config.agent.max_budget_usd - spent).max(0.0);
        if spent > 0.0 {
            info!(
                spent_usd = spent,
                remaining_usd = remaining_budget,
                max_usd = config.agent.max_budget_usd,
                "Resuming with reduced budget (prior phases spent ${:.2})",
                spent,
            );
        }

        // Fail fast if the budget is already exhausted from prior phases.
        if remaining_budget <= 0.0 {
            return Err(CoreError::BudgetExhausted {
                spent,
                limit: config.agent.max_budget_usd,
            });
        }

        // Load .coda.md for system prompt context
        let coda_md = std::fs::read_to_string(project_root.join(".coda.md")).unwrap_or_default();
        let system_prompt = pm.render("run/system", minijinja::context!(coda_md => coda_md))?;

        // Create client with Coder profile, cwd = worktree
        let mut options = AgentProfile::Coder.to_options(
            &system_prompt,
            worktree_path.clone(),
            config.agent.max_turns,
            remaining_budget,
            &config.agent.model,
        );

        // Enable partial messages so the SDK emits StreamEvent messages
        // containing token-level text deltas for real-time UI streaming.
        options.include_partial_messages = true;

        // Forward CLI stderr output as RunEvent::StderrOutput so errors
        // (auth failures, rate limits, etc.) are visible in the TUI.
        if let Some(ref tx) = progress_tx {
            let tx_clone = tx.clone();
            options.stderr = Some(Arc::new(move |line: &str| {
                let _ = tx_clone.send(RunEvent::StderrOutput {
                    line: line.to_string(),
                });
            }));
        }

        let client = code_agent_sdk::AgentSdkClient::new(Some(options), None);
        let session_config = SessionConfig {
            idle_timeout_secs: config.agent.idle_timeout_secs,
            tool_execution_timeout_secs: config.agent.tool_execution_timeout_secs,
            idle_retries: config.agent.idle_retries,
            max_budget_usd: remaining_budget,
        };
        let mut session = AgentSession::new(client, session_config);

        // Set cancellation token on the session for in-flight interruption
        session.set_cancellation_token(cancel_token.clone());

        // Bridge SessionEvent -> RunEvent via a forwarding task
        if let Some(ref tx) = progress_tx {
            let (session_tx, mut session_rx) =
                tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
            let run_tx = tx.clone();
            tokio::spawn(async move {
                while let Some(event) = session_rx.recv().await {
                    let run_event = match event {
                        SessionEvent::TextDelta { text } => RunEvent::AgentTextDelta { text },
                        SessionEvent::ToolActivity { tool_name, summary } => {
                            RunEvent::ToolActivity { tool_name, summary }
                        }
                        SessionEvent::TurnCompleted { current_turn } => {
                            RunEvent::TurnCompleted { current_turn }
                        }
                        SessionEvent::IdleWarning {
                            attempt,
                            max_retries,
                            idle_secs,
                        } => RunEvent::IdleWarning {
                            attempt,
                            max_retries,
                            idle_secs,
                        },
                        SessionEvent::Reconnecting {
                            attempt,
                            max_retries,
                        } => RunEvent::Reconnecting {
                            attempt,
                            max_retries,
                        },
                    };
                    let _ = run_tx.send(run_event);
                }
            });
            session.set_event_sender(session_tx);
        }

        let run_logger = RunLogger::new(&feature_dir);

        // Wrap sync git/gh in async wrappers for blocking safety
        let async_git = AsyncGitOps::new(git);
        let async_gh = AsyncGhOps::new(gh);

        let state_manager = StateManager::new(state, state_path);

        let ctx = PhaseContext {
            session,
            pm: pm.clone(),
            config: config.clone(),
            state_manager,
            worktree_path,
            review_summary: ReviewSummary::default(),
            verification_summary: VerificationSummary::default(),
            progress_tx,
            metrics: MetricsTracker::default(),
            run_logger,
            git: async_git,
            gh: async_gh,
            pre_squash_commits: Vec::new(),
            cancel_token,
        };

        Ok(Self { ctx })
    }

    /// Executes all remaining phases from the current checkpoint.
    ///
    /// Connects the client, iterates through phases from the first
    /// non-completed phase (derived from statuses by [`StateManager`]),
    /// dispatching each phase to the appropriate [`PhaseExecutor`]:
    ///
    /// - **Dev** phases → [`DevPhaseExecutor`]
    /// - **Quality** phases → [`ReviewPhaseExecutor`], [`VerifyPhaseExecutor`],
    ///   or [`DocsPhaseExecutor`] by name
    ///
    /// After all phases complete, creates a PR via [`create_pr`].
    ///
    /// Between phases, checks the cancellation token and returns
    /// [`CoreError::Cancelled`] if triggered. The agent session is
    /// always disconnected when this method returns, regardless of
    /// whether it succeeds, fails, or is cancelled.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::Cancelled` if the cancellation token is triggered.
    /// Returns `CoreError` if any phase fails after all retries.
    pub async fn execute(&mut self) -> Result<Vec<TaskResult>, CoreError> {
        // Connect to Claude
        self.ctx.emit_event(RunEvent::Connecting);
        self.ctx.session.connect().await?;

        // Run the pipeline; disconnect is guaranteed afterwards,
        // regardless of success, failure, or cancellation.
        let result = self.run_pipeline().await;

        // Always disconnect the agent session to clean up the
        // subprocess, even on error or cancellation paths.
        self.ctx.session.disconnect().await;

        // On cancellation, persist state as a safety net so the
        // next `coda run` can resume from the last checkpoint.
        if matches!(&result, Err(CoreError::Cancelled))
            && let Err(e) = self.ctx.state_manager.save()
        {
            warn!(error = %e, "Failed to save state during cancellation cleanup");
        }

        result
    }

    /// Runs the phase pipeline and post-phase operations (PR creation, push).
    ///
    /// Separated from [`execute`](Self::execute) so that `execute` can
    /// guarantee session disconnect in all code paths. Cancellation is
    /// checked between phases and before PR creation. Once the PR is
    /// created, the push is treated as a critical atomic operation that
    /// must not be interrupted by cancellation — otherwise the PR would
    /// exist on GitHub but the branch would not be pushed, and the
    /// persisted `Completed` status would prevent resumption.
    async fn run_pipeline(&mut self) -> Result<Vec<TaskResult>, CoreError> {
        // Mark feature as in progress
        self.ctx
            .state_manager
            .set_feature_status(FeatureStatus::InProgress)?;
        self.ctx.state_manager.save()?;

        let mut results = Vec::new();
        let total_phases = self.ctx.state().phases.len();

        // Determine start phase from actual phase statuses via StateManager.
        // This is reliable even after a crash because it derives from phase
        // statuses rather than the `current_phase` counter.
        let start_phase = self.ctx.state_manager.current_phase_index();

        if start_phase > 0 {
            let (review, verify) = restore_summaries_from_state(self.ctx.state());
            self.ctx.review_summary = review;
            self.ctx.verification_summary = verify;
            info!(
                start_phase = start_phase,
                total = total_phases,
                "Resuming from phase {} (skipping {} completed)",
                self.ctx
                    .state()
                    .phases
                    .get(start_phase)
                    .map_or("create-pr", |p| p.name.as_str()),
                start_phase,
            );
        }

        // Emit initial phase list so the UI can display the full pipeline
        let phase_names: Vec<String> = self
            .ctx
            .state()
            .phases
            .iter()
            .map(|p| p.name.clone())
            .collect();
        self.ctx.emit_event(RunEvent::RunStarting {
            phases: phase_names.clone(),
        });
        let feature_slug_for_log = self.ctx.state().feature.slug.clone();
        if let Some(logger) = &mut self.ctx.run_logger {
            logger.log_header(
                &feature_slug_for_log,
                &self.ctx.config.agent.model,
                &phase_names,
            );
        }
        // Replay events for phases that completed in a previous session so
        // the UI shows them as completed with correct metrics and the
        // summary accumulates their turns/cost.
        for (phase_idx, phase) in self.ctx.state().phases[..start_phase].iter().enumerate() {
            let name = phase_names[phase_idx].clone();
            self.ctx.emit_event(RunEvent::PhaseStarting {
                name: name.clone(),
                index: phase_idx,
                total: total_phases,
            });
            self.ctx.emit_event(RunEvent::PhaseCompleted {
                name,
                index: phase_idx,
                duration: Duration::from_secs(phase.duration_secs),
                turns: phase.turns,
                cost_usd: phase.cost_usd,
            });
        }

        // Circuit breaker: abort if consecutive phases complete with
        // zero cost and minimal turns, indicating a non-functional session.
        let mut consecutive_zero_cost_phases: u32 = 0;
        const MAX_CONSECUTIVE_ZERO_COST: u32 = 2;

        for phase_idx in start_phase..total_phases {
            // Check cancellation between phases
            self.ctx.check_cancelled()?;

            let phase_name = self.ctx.state().phases[phase_idx].name.clone();
            let phase_kind = self.ctx.state().phases[phase_idx].kind.clone();

            info!(phase = %phase_name, index = phase_idx, "Starting phase");

            let kind_str = match &phase_kind {
                PhaseKind::Dev => "dev",
                PhaseKind::Quality => "quality",
            };
            if let Some(logger) = &mut self.ctx.run_logger {
                logger.log_phase_start(&phase_name, phase_idx, total_phases, kind_str);
            }

            self.ctx.emit_event(RunEvent::PhaseStarting {
                name: phase_name.clone(),
                index: phase_idx,
                total: total_phases,
            });

            let result = match phase_kind {
                PhaseKind::Dev => DevPhaseExecutor.execute(&mut self.ctx, phase_idx).await,
                PhaseKind::Quality => match phase_name.as_str() {
                    "review" => ReviewPhaseExecutor.execute(&mut self.ctx, phase_idx).await,
                    "verify" => VerifyPhaseExecutor.execute(&mut self.ctx, phase_idx).await,
                    "update-docs" => DocsPhaseExecutor.execute(&mut self.ctx, phase_idx).await,
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
                    self.ctx.emit_event(RunEvent::PhaseCompleted {
                        name: phase_name.clone(),
                        index: phase_idx,
                        duration: task_result.duration,
                        turns: task_result.turns,
                        cost_usd: task_result.cost_usd,
                    });

                    // Circuit breaker: track consecutive zero-cost phases.
                    if task_result.cost_usd == 0.0 && task_result.turns <= 1 {
                        consecutive_zero_cost_phases += 1;
                        if consecutive_zero_cost_phases >= MAX_CONSECUTIVE_ZERO_COST {
                            error!(
                                consecutive = consecutive_zero_cost_phases,
                                "Circuit breaker triggered: {} consecutive phases \
                                 completed with zero cost and minimal turns",
                                consecutive_zero_cost_phases,
                            );
                            self.ctx.state_manager.fail_phase(phase_idx);
                            self.ctx
                                .state_manager
                                .set_feature_status(FeatureStatus::Failed)?;
                            self.ctx.state_manager.save()?;
                            return Err(CoreError::AgentError(format!(
                                "Circuit breaker: {consecutive_zero_cost_phases} \
                                 consecutive phases completed with zero cost and \
                                 ≤1 turn. The agent session appears non-functional \
                                 (possible API rate-limit or authentication failure).",
                            )));
                        }
                    } else {
                        consecutive_zero_cost_phases = 0;
                    }

                    results.push(task_result);

                    // Save checkpoint (current_phase is derived by StateManager)
                    self.ctx.state_manager.save()?;
                }
                Err(ref e) if matches!(e, CoreError::Cancelled) => {
                    // Cancellation: save state for clean resume without marking
                    // the phase or feature as Failed. The phase stays in its
                    // current status (Running or Pending) so the next `coda run`
                    // picks up exactly where this one was interrupted.
                    // Disconnect is handled by execute() after run_pipeline returns.
                    info!(phase = %phase_name, "Phase cancelled, saving state for resume");
                    if let Some(logger) = &mut self.ctx.run_logger {
                        logger.log_message(&format!("⏸ Phase {phase_name} cancelled.\n",));
                    }
                    self.ctx.state_manager.save()?;
                    return Err(CoreError::Cancelled);
                }
                Err(e) => {
                    error!(phase = %phase_name, error = %e, "Phase failed");
                    if let Some(logger) = &mut self.ctx.run_logger {
                        logger.log_message(&format!(
                            "✗ Phase {phase_name} FAILED: {e}\n  Aborting run.\n",
                        ));
                    }
                    self.ctx.emit_event(RunEvent::PhaseFailed {
                        name: phase_name.clone(),
                        index: phase_idx,
                        error: e.to_string(),
                    });
                    self.ctx.state_manager.fail_phase(phase_idx);
                    self.ctx
                        .state_manager
                        .set_feature_status(FeatureStatus::Failed)?;
                    self.ctx.state_manager.save()?;
                    return Err(e);
                }
            }
        }

        // Compute totals before creating PR so the PR body has accurate stats
        // (excludes create_pr phase itself, which is a meta-operation)
        self.ctx.state_manager.update_totals();
        self.ctx.state_manager.save()?;
        self.ctx.commit_coda_state().await?;

        self.ctx.check_cancelled()?;

        let squashed = self.ctx.config.git.squash_before_push;
        if squashed && let Err(e) = prepare_squash(&mut self.ctx).await {
            warn!(error = %e, "Squash failed, falling back to multi-commit flow");
        }

        self.ctx.check_cancelled()?;

        // All phases complete — create PR
        info!("All phases complete, creating PR...");
        self.ctx.emit_event(RunEvent::CreatingPr);
        let pr_result = create_pr(&mut self.ctx).await?;

        // Extract PR URL before pushing result
        let pr_url = self.ctx.state().pr.as_ref().map(|pr| pr.url.clone());
        self.ctx.emit_event(RunEvent::PrCreated { url: pr_url });

        let pr_succeeded = matches!(pr_result.status, TaskStatus::Completed);
        results.push(pr_result);

        if !pr_succeeded {
            // Code phases completed but PR creation failed.
            // Keep InProgress so a re-run only retries PR creation.
            warn!("Feature development complete but PR creation failed");
        }

        // Save state checkpoint with PR info and totals as InProgress.
        // The Completed transition is deferred until after push succeeds
        // to prevent a cancellation/crash between here and push from
        // leaving the feature in an unresumable Completed state.
        self.ctx.state_manager.update_totals();
        self.ctx.state_manager.save()?;

        // Once the PR is created, the push is a critical atomic operation.
        // We intentionally do NOT check cancellation here — interrupting
        // between PR creation and push would leave the PR on GitHub
        // without the branch pushed, and persisting Completed before push
        // would make the feature non-resumable.
        let branch = self.ctx.state().git.branch.clone();
        if squashed && !self.ctx.pre_squash_commits.is_empty() {
            // Amend post-PR state into the single squashed commit, then force-push
            self.ctx
                .git
                .add(&self.ctx.worktree_path, &[".coda/"])
                .await?;
            if self
                .ctx
                .git
                .has_staged_changes(&self.ctx.worktree_path)
                .await
            {
                self.ctx
                    .git
                    .commit_amend_no_edit_internal(&self.ctx.worktree_path)
                    .await?;
            }
            self.ctx
                .git
                .push_force_with_lease(&self.ctx.worktree_path, &branch)
                .await?;
        } else {
            self.ctx.commit_coda_state().await?;
            self.ctx.git.push(&self.ctx.worktree_path, &branch).await?;
        }

        // Push succeeded — now it is safe to mark Completed.
        if pr_succeeded {
            self.ctx
                .state_manager
                .set_feature_status(FeatureStatus::Completed)?;
            self.ctx.state_manager.save()?;
        }

        self.ctx.emit_event(RunEvent::RunFinished {
            success: pr_succeeded,
        });

        Ok(results)
    }
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("feature", &self.ctx.state().feature.slug)
            .field(
                "current_phase",
                &self.ctx.state_manager.current_phase_index(),
            )
            .field("worktree", &self.ctx.worktree_path)
            .finish_non_exhaustive()
    }
}

// ── Free Functions ──────────────────────────────────────────────────

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

/// Rebuilds `review_summary` and `verification_summary` from persisted
/// phase details so that resumed runs have accurate summaries for PR
/// creation and final display.
fn restore_summaries_from_state(state: &FeatureState) -> (ReviewSummary, VerificationSummary) {
    let mut review = ReviewSummary::default();
    let mut verify = VerificationSummary::default();

    for phase in &state.phases {
        if phase.status != PhaseStatus::Completed {
            continue;
        }
        match phase.name.as_str() {
            "review" => {
                // Support both current key and legacy "issues_resolved"
                // from state files written before the rename.
                let fix_attempted = phase.details["issues_fix_attempted"]
                    .as_u64()
                    .or_else(|| phase.details["issues_resolved"].as_u64())
                    .unwrap_or(0) as u32;
                review = ReviewSummary {
                    rounds: phase.details["rounds"].as_u64().unwrap_or(0) as u32,
                    issues_found: phase.details["issues_found"].as_u64().unwrap_or(0) as u32,
                    issues_fix_attempted: fix_attempted,
                };
            }
            "verify" => {
                verify = VerificationSummary {
                    checks_passed: phase.details["checks_passed"].as_u64().unwrap_or(0) as u32,
                    checks_total: phase.details["checks_total"].as_u64().unwrap_or(0) as u32,
                };
            }
            _ => {}
        }
    }

    (review, verify)
}
