//! Runner for executing feature development through phased stages.
//!
//! Manages a single continuous `ClaudeClient` session that progresses
//! through setup → implement → test → review → verify phases, with
//! state persistence for crash recovery.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use claude_agent_sdk_rs::{ClaudeClient, ContentBlock, Message, ResultMessage};
use coda_pm::PromptManager;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn};

use crate::CoreError;
use crate::config::CodaConfig;
use crate::profile::AgentProfile;
use crate::state::{FeatureState, FeatureStatus, PhaseStatus};
use crate::task::{Task, TaskResult, TaskStatus};

/// Phase names in execution order.
const PHASE_NAMES: [&str; 5] = ["setup", "implement", "test", "review", "verify"];

/// Real-time progress events emitted during a feature run.
///
/// Subscribe to these events via [`Runner::set_progress_sender`] to display
/// live progress in the CLI or UI layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RunEvent {
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
    /// Creating pull request after all phases.
    CreatingPr,
    /// PR creation completed.
    PrCreated {
        /// PR URL, if successfully extracted from agent response.
        url: Option<String>,
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
            100, // generous turn limit for full run
            config.agent.max_budget_usd,
        );

        let client = ClaudeClient::new(options);

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
    /// and returns results for each completed phase.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if any phase fails after all retries.
    pub async fn execute(&mut self) -> Result<Vec<TaskResult>, CoreError> {
        // Connect to Claude
        self.client
            .connect()
            .await
            .map_err(|e| CoreError::AgentSdkError(e.to_string()))?;
        self.connected = true;

        // Mark feature as in progress
        self.state.status = FeatureStatus::InProgress;
        self.save_state()?;

        let mut results = Vec::new();
        let start_phase = self.state.current_phase as usize;
        let total_phases = PHASE_NAMES.len();

        for (phase_idx, &phase_name) in PHASE_NAMES.iter().enumerate().skip(start_phase) {
            info!(phase = phase_name, index = phase_idx, "Starting phase");
            self.emit_event(RunEvent::PhaseStarting {
                name: phase_name.to_string(),
                index: phase_idx,
                total: total_phases,
            });

            let result = match phase_name {
                "setup" => self.run_setup().await,
                "implement" => self.run_implement().await,
                "test" => self.run_test().await,
                "review" => self.run_review().await,
                "verify" => self.run_verify().await,
                _ => Err(CoreError::AgentError(format!(
                    "Unknown phase: {phase_name}"
                ))),
            };

            match result {
                Ok(task_result) => {
                    info!(
                        phase = phase_name,
                        turns = task_result.turns,
                        cost_usd = task_result.cost_usd,
                        "Phase completed"
                    );
                    self.emit_event(RunEvent::PhaseCompleted {
                        name: phase_name.to_string(),
                        index: phase_idx,
                        duration: task_result.duration,
                        turns: task_result.turns,
                        cost_usd: task_result.cost_usd,
                    });
                    results.push(task_result);

                    // Advance to next phase
                    if phase_idx + 1 < PHASE_NAMES.len() {
                        self.state.current_phase = (phase_idx + 1) as u32;
                    }
                    self.save_state()?;
                }
                Err(e) => {
                    error!(phase = phase_name, error = %e, "Phase failed");
                    self.emit_event(RunEvent::PhaseFailed {
                        name: phase_name.to_string(),
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

        // Commit .coda/ state updates before creating PR
        self.commit_coda_state()?;

        // All phases complete — create PR
        info!("All phases complete, creating PR...");
        self.emit_event(RunEvent::CreatingPr);
        let pr_result = self.create_pr().await?;

        // Extract PR URL before pushing result
        let pr_url = self.state.pr.as_ref().map(|pr| pr.url.clone());
        self.emit_event(RunEvent::PrCreated { url: pr_url });

        results.push(pr_result);

        // Mark feature as completed
        self.state.status = FeatureStatus::Completed;
        self.update_totals();
        self.save_state()?;

        // Disconnect
        if self.connected {
            let _ = self.client.disconnect().await;
            self.connected = false;
        }

        Ok(results)
    }

    /// Executes the setup phase: scaffold directory structure.
    async fn run_setup(&mut self) -> Result<TaskResult, CoreError> {
        let phase_idx = 0;
        self.mark_phase_running(phase_idx);

        let design_spec = self.load_design_spec()?;
        let repo_tree = self.gather_worktree_tree()?;
        let checks = &self.config.checks;
        let feature_slug = self.state.feature.slug.clone();

        let prompt = self.pm.render(
            "run/setup",
            minijinja::context!(
                design_spec => design_spec,
                repo_tree => repo_tree,
                checks => checks,
                feature_slug => feature_slug,
            ),
        )?;

        let (response, result_msg) = self.send_and_collect(&prompt).await?;
        debug!(
            response_len = response.len(),
            "Setup phase response received"
        );

        self.mark_phase_completed(phase_idx, &result_msg);

        Ok(TaskResult {
            task: Task::Setup {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: result_msg.as_ref().map_or(1, |r| r.num_turns),
            cost_usd: result_msg
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0),
            duration: std::time::Duration::from_millis(
                result_msg.as_ref().map_or(0, |r| r.duration_ms),
            ),
            artifacts: vec![],
        })
    }

    /// Executes the implement phase: code changes for each development step.
    async fn run_implement(&mut self) -> Result<TaskResult, CoreError> {
        let phase_idx = 1;
        let was_running = self.state.phases[phase_idx].status == PhaseStatus::Running;
        self.mark_phase_running(phase_idx);

        let design_spec = self.load_design_spec()?;
        let checks = &self.config.checks;
        let feature_slug = self.state.feature.slug.clone();

        // Build resume context if resuming mid-implement
        let resume_context = if was_running {
            self.build_resume_context()?
        } else {
            String::new()
        };

        // Send a single implement prompt (the agent handles all sub-phases)
        let prompt = self.pm.render(
            "run/implement",
            minijinja::context!(
                design_spec => design_spec,
                phase_index => 0,
                checks => checks,
                feature_slug => feature_slug,
                resume_context => resume_context,
            ),
        )?;

        let (response, result_msg) = self.send_and_collect(&prompt).await?;
        debug!(
            response_len = response.len(),
            "Implement phase response received"
        );

        self.mark_phase_completed(phase_idx, &result_msg);

        Ok(TaskResult {
            task: Task::Implement {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: result_msg.as_ref().map_or(1, |r| r.num_turns),
            cost_usd: result_msg
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0),
            duration: std::time::Duration::from_millis(
                result_msg.as_ref().map_or(0, |r| r.duration_ms),
            ),
            artifacts: vec![],
        })
    }

    /// Executes the test phase: write and run tests.
    async fn run_test(&mut self) -> Result<TaskResult, CoreError> {
        let phase_idx = 2;
        self.mark_phase_running(phase_idx);

        let design_spec = self.load_design_spec()?;
        let changed_files = self.get_changed_files()?;
        let checks = &self.config.checks;
        let feature_slug = self.state.feature.slug.clone();

        let prompt = self.pm.render(
            "run/test",
            minijinja::context!(
                design_spec => design_spec,
                changed_files => changed_files,
                checks => checks,
                feature_slug => feature_slug,
            ),
        )?;

        let (response, result_msg) = self.send_and_collect(&prompt).await?;
        debug!(
            response_len = response.len(),
            "Test phase response received"
        );

        self.mark_phase_completed(phase_idx, &result_msg);

        Ok(TaskResult {
            task: Task::Test {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: result_msg.as_ref().map_or(1, |r| r.num_turns),
            cost_usd: result_msg
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0),
            duration: std::time::Duration::from_millis(
                result_msg.as_ref().map_or(0, |r| r.duration_ms),
            ),
            artifacts: vec![],
        })
    }

    /// Executes the review phase with fix loop.
    ///
    /// Sends the diff for review, parses the YAML response for issues,
    /// and if critical/major issues are found, asks the agent to fix them
    /// and re-reviews. Loops up to `max_review_rounds`.
    async fn run_review(&mut self) -> Result<TaskResult, CoreError> {
        let phase_idx = 3;
        self.mark_phase_running(phase_idx);

        if !self.config.review.enabled {
            info!("Code review disabled, skipping");
            self.mark_phase_completed(phase_idx, &None);
            return Ok(TaskResult {
                task: Task::Review {
                    feature_slug: self.state.feature.slug.clone(),
                },
                status: TaskStatus::Completed,
                turns: 0,
                cost_usd: 0.0,
                duration: std::time::Duration::ZERO,
                artifacts: vec![],
            });
        }

        let design_spec = self.load_design_spec()?;
        let max_rounds = self.config.review.max_review_rounds;
        let start = Instant::now();
        let mut total_turns = 0u32;
        let mut total_cost = 0.0f64;

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

            let (response, result_msg) = self.send_and_collect(&review_prompt).await?;
            total_turns += result_msg.as_ref().map_or(1, |r| r.num_turns);
            total_cost += result_msg
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0);

            self.review_summary.rounds += 1;

            // Parse review issues from response
            let issues = parse_review_issues(&response);
            let issue_count = issues.len() as u32;
            self.review_summary.issues_found += issue_count;

            if issues.is_empty() {
                info!("No critical/major issues found, review passed");
                break;
            }

            info!(issues = issue_count, "Found issues, asking agent to fix");

            // Ask agent to fix the issues
            let fix_prompt = format!(
                "The code review found {} critical/major issues. Please fix them:\n\n{}",
                issue_count,
                issues
                    .iter()
                    .enumerate()
                    .map(|(i, issue)| format!("{}. {}", i + 1, issue))
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            let (_fix_response, fix_result) = self.send_and_collect(&fix_prompt).await?;
            total_turns += fix_result.as_ref().map_or(1, |r| r.num_turns);
            total_cost += fix_result
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0);

            self.review_summary.issues_resolved += issue_count;
        }

        self.mark_phase_completed(phase_idx, &None);
        // Update phase metrics manually since we accumulated across rounds
        self.state.phases[phase_idx].turns = total_turns;
        self.state.phases[phase_idx].cost_usd = total_cost;
        self.state.phases[phase_idx].duration_secs = start.elapsed().as_secs();
        self.state.phases[phase_idx].details = serde_json::json!({
            "rounds": self.review_summary.rounds,
            "issues_found": self.review_summary.issues_found,
            "issues_resolved": self.review_summary.issues_resolved,
        });
        self.save_state()?;

        Ok(TaskResult {
            task: Task::Review {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: total_turns,
            cost_usd: total_cost,
            duration: start.elapsed(),
            artifacts: vec![],
        })
    }

    /// Executes the verify phase with fix loop.
    ///
    /// Runs the verification plan, and if any check fails, asks the
    /// agent to fix the issue and re-verifies.
    async fn run_verify(&mut self) -> Result<TaskResult, CoreError> {
        let phase_idx = 4;
        self.mark_phase_running(phase_idx);

        let verification_spec = self.load_verification_spec()?;
        let checks = self.config.checks.clone();
        let max_attempts = self.config.agent.max_retries;
        let start = Instant::now();
        let mut total_turns = 0u32;
        let mut total_cost = 0.0f64;

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

            let (response, result_msg) = self.send_and_collect(&verify_prompt).await?;
            total_turns += result_msg.as_ref().map_or(1, |r| r.num_turns);
            total_cost += result_msg
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0);

            // Parse verification result
            let (passed, failed_details) = parse_verification_result(&response);
            self.verification_summary.checks_total = passed + failed_details.len() as u32;
            self.verification_summary.checks_passed = passed;

            if failed_details.is_empty() {
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

            let fix_prompt = format!(
                "Verification failed. Please fix these issues and re-run the checks:\n\n{}",
                failed_details.join("\n")
            );

            let (_fix_response, fix_result) = self.send_and_collect(&fix_prompt).await?;
            total_turns += fix_result.as_ref().map_or(1, |r| r.num_turns);
            total_cost += fix_result
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0);
        }

        self.mark_phase_completed(phase_idx, &None);
        self.state.phases[phase_idx].turns = total_turns;
        self.state.phases[phase_idx].cost_usd = total_cost;
        self.state.phases[phase_idx].duration_secs = start.elapsed().as_secs();
        self.state.phases[phase_idx].details = serde_json::json!({
            "attempts": self.verification_summary.checks_total,
            "checks_passed": self.verification_summary.checks_passed,
            "checks_total": self.verification_summary.checks_total,
        });
        self.save_state()?;

        Ok(TaskResult {
            task: Task::Verify {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: total_turns,
            cost_usd: total_cost,
            duration: start.elapsed(),
            artifacts: vec![],
        })
    }

    /// Creates a pull request after all phases complete.
    async fn create_pr(&mut self) -> Result<TaskResult, CoreError> {
        let design_spec = self.load_design_spec()?;
        let commits = self.get_commits()?;
        let checks = &self.config.checks;
        let start = Instant::now();

        let pr_prompt = self.pm.render(
            "run/create_pr",
            minijinja::context!(
                design_spec => design_spec,
                commits => commits,
                state => &self.state,
                checks => checks,
                review_summary => &self.review_summary,
                verification_summary => &self.verification_summary,
            ),
        )?;

        let (response, result_msg) = self.send_and_collect(&pr_prompt).await?;

        // Try to extract PR URL from response
        if let Some(pr_url) = extract_pr_url(&response) {
            info!(url = %pr_url, "PR created");
            self.state.pr = Some(crate::state::PrInfo {
                url: pr_url.clone(),
                number: extract_pr_number(&pr_url).unwrap_or(0),
                title: format!("feat({}): feature implementation", self.state.feature.slug),
            });
            self.save_state()?;
        }

        Ok(TaskResult {
            task: Task::CreatePr {
                feature_slug: self.state.feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: result_msg.as_ref().map_or(1, |r| r.num_turns),
            cost_usd: result_msg
                .as_ref()
                .and_then(|r| r.total_cost_usd)
                .unwrap_or(0.0),
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
        // Stage .coda/ directory
        run_command("git", &["add", ".coda/"], &self.worktree_path)?;

        // Check if there are staged changes
        let has_changes =
            run_command("git", &["diff", "--cached", "--quiet"], &self.worktree_path).is_err();

        if has_changes {
            let msg = format!("chore({}): update execution state", self.state.feature.slug);
            run_command("git", &["commit", "-m", &msg], &self.worktree_path)?;
            info!("Committed .coda/ state updates");
        } else {
            debug!("No .coda/ changes to commit");
        }

        Ok(())
    }

    /// Sends a prompt and collects the full response text + ResultMessage.
    async fn send_and_collect(
        &mut self,
        prompt: &str,
    ) -> Result<(String, Option<ResultMessage>), CoreError> {
        self.client
            .query(prompt)
            .await
            .map_err(|e| CoreError::AgentSdkError(e.to_string()))?;

        let mut response = String::new();
        let mut result_msg = None;

        {
            let mut stream = self.client.receive_response();
            while let Some(result) = stream.next().await {
                let msg = result.map_err(|e| CoreError::AgentSdkError(e.to_string()))?;
                match msg {
                    Message::Assistant(assistant) => {
                        for block in &assistant.message.content {
                            if let ContentBlock::Text(text) = block {
                                response.push_str(&text.text);
                            }
                        }
                    }
                    Message::Result(r) => {
                        result_msg = Some(r);
                        break;
                    }
                    _ => {}
                }
            }
        }

        Ok((response, result_msg))
    }

    /// Marks a phase as running and saves state.
    fn mark_phase_running(&mut self, phase_idx: usize) {
        self.state.phases[phase_idx].status = PhaseStatus::Running;
        self.state.phases[phase_idx].started_at = Some(chrono::Utc::now());
        if let Err(e) = self.save_state() {
            warn!(error = %e, "Failed to save state when marking phase as running");
        }
    }

    /// Marks a phase as completed and records metrics from the ResultMessage.
    fn mark_phase_completed(&mut self, phase_idx: usize, result_msg: &Option<ResultMessage>) {
        self.state.phases[phase_idx].status = PhaseStatus::Completed;
        self.state.phases[phase_idx].completed_at = Some(chrono::Utc::now());

        if let Some(r) = result_msg {
            self.state.phases[phase_idx].turns = r.num_turns;
            self.state.phases[phase_idx].cost_usd = r.total_cost_usd.unwrap_or(0.0);
            self.state.phases[phase_idx].duration_secs = r.duration_ms / 1000;
        }

        self.state.feature.updated_at = chrono::Utc::now();
    }

    /// Persists the current state to `state.yml`.
    fn save_state(&self) -> Result<(), CoreError> {
        let yaml = serde_yaml::to_string(&self.state)?;
        std::fs::write(&self.state_path, yaml).map_err(CoreError::IoError)?;
        debug!(path = %self.state_path.display(), "State saved");
        Ok(())
    }

    /// Loads the design spec from the worktree's `.coda/<slug>/specs/` directory.
    fn load_design_spec(&self) -> Result<String, CoreError> {
        let spec_path = self
            .worktree_path
            .join(".coda")
            .join(&self.state.feature.slug)
            .join("specs/design.md");

        std::fs::read_to_string(&spec_path).map_err(|e| {
            CoreError::StateError(format!(
                "Cannot read design spec at {}: {e}",
                spec_path.display()
            ))
        })
    }

    /// Loads the verification spec from the worktree's `.coda/<slug>/specs/` directory.
    fn load_verification_spec(&self) -> Result<String, CoreError> {
        let spec_path = self
            .worktree_path
            .join(".coda")
            .join(&self.state.feature.slug)
            .join("specs/verification.md");

        std::fs::read_to_string(&spec_path).map_err(|e| {
            CoreError::StateError(format!(
                "Cannot read verification spec at {}: {e}",
                spec_path.display()
            ))
        })
    }

    /// Gets the git diff of all changes from the base branch.
    fn get_diff(&self) -> Result<String, CoreError> {
        run_command(
            "git",
            &["diff", &self.state.git.base_branch, "HEAD"],
            &self.worktree_path,
        )
    }

    /// Gets a list of files changed from the base branch.
    fn get_changed_files(&self) -> Result<Vec<String>, CoreError> {
        let stdout = run_command(
            "git",
            &["diff", "--name-only", &self.state.git.base_branch, "HEAD"],
            &self.worktree_path,
        )?;

        let files = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();

        Ok(files)
    }

    /// Gets the list of commits from the base branch to HEAD.
    fn get_commits(&self) -> Result<Vec<CommitInfo>, CoreError> {
        let range = format!("{}..HEAD", self.state.git.base_branch);
        let stdout = run_command(
            "git",
            &["log", &range, "--oneline", "--no-decorate"],
            &self.worktree_path,
        )?;

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

    /// Gathers a simple tree listing of the worktree.
    fn gather_worktree_tree(&self) -> Result<String, CoreError> {
        run_command(
            "find",
            &[
                ".",
                "-not",
                "-path",
                "./.git/*",
                "-not",
                "-path",
                "./target/*",
                "-type",
                "f",
            ],
            &self.worktree_path,
        )
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

/// Runs an external command and returns its stdout, checking the exit status.
///
/// # Errors
///
/// Returns `CoreError::GitError` if the command exits with a non-zero status,
/// including the captured stderr for diagnostics.
fn run_command(cmd: &str, args: &[&str], cwd: &Path) -> Result<String, CoreError> {
    let output = std::process::Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(CoreError::IoError)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoreError::GitError(format!(
            "{cmd} {} failed: {stderr}",
            args.join(" "),
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

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

/// Parses review issues from the agent's YAML response.
///
/// Returns a list of issue description strings for critical/major issues.
fn parse_review_issues(response: &str) -> Vec<String> {
    // Try to find YAML block in response
    let yaml_content = extract_yaml_block(response);

    if let Some(yaml) = yaml_content
        && let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&yaml)
        && let Some(issues) = parsed.get("issues").and_then(|v| v.as_array())
    {
        return issues
            .iter()
            .filter_map(|issue| {
                let severity = issue.get("severity")?.as_str()?;
                if severity == "critical" || severity == "major" {
                    let desc = issue
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Unknown issue");
                    let file = issue
                        .get("file")
                        .and_then(|f| f.as_str())
                        .unwrap_or("unknown");
                    let suggestion = issue
                        .get("suggestion")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    Some(format!(
                        "[{severity}] {file}: {desc}. Suggestion: {suggestion}"
                    ))
                } else {
                    None
                }
            })
            .collect();
    }

    Vec::new()
}

/// Parses verification results from the agent's YAML response.
///
/// Returns `(passed_count, failed_details)`.
fn parse_verification_result(response: &str) -> (u32, Vec<String>) {
    let yaml_content = extract_yaml_block(response);

    if let Some(yaml) = yaml_content
        && let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&yaml)
    {
        let result = parsed
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if result == "passed" {
            let total = parsed
                .get("total_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            return (total, Vec::new());
        }

        let mut failed = Vec::new();
        if let Some(checks) = parsed.get("checks").and_then(|v| v.as_array()) {
            let passed = checks
                .iter()
                .filter(|c| c.get("status").and_then(|s| s.as_str()) == Some("passed"))
                .count() as u32;

            for check in checks {
                if check.get("status").and_then(|s| s.as_str()) == Some("failed") {
                    let name = check
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let details = check
                        .get("details")
                        .and_then(|d| d.as_str())
                        .unwrap_or("no details");
                    failed.push(format!("{name}: {details}"));
                }
            }

            return (passed, failed);
        }
    }

    // If we can't parse, treat as a failure to avoid false positives.
    // The verification loop will ask the agent to fix / re-run, or exhaust retries.
    (
        0,
        vec![
            "Unable to parse verification result from agent response. Manual review required."
                .to_string(),
        ],
    )
}

/// Extracts a YAML code block from a response string.
fn extract_yaml_block(text: &str) -> Option<String> {
    // Look for ```yaml ... ``` block
    if let Some(start) = text.find("```yaml") {
        let content_start = start + "```yaml".len();
        if let Some(end) = text[content_start..].find("```") {
            return Some(text[content_start..content_start + end].trim().to_string());
        }
    }

    // Look for ```\n...\n``` block (unmarked code block)
    if let Some(start) = text.find("```\n") {
        let content_start = start + "```\n".len();
        if let Some(end) = text[content_start..].find("```") {
            let content = text[content_start..content_start + end].trim();
            // Check if it looks like YAML
            if content.starts_with("issues:") || content.starts_with("result:") {
                return Some(content.to_string());
            }
        }
    }

    // Try parsing the whole response as YAML
    if text.contains("issues:") || text.contains("result:") {
        return Some(text.to_string());
    }

    None
}

/// Extracts a PR URL from the agent's response text.
fn extract_pr_url(text: &str) -> Option<String> {
    // Look for a GitHub PR URL anywhere in the text
    for line in text.lines() {
        if let Some(start) = line.find("https://github.com/") {
            let url_part = &line[start..];
            // Find end of URL (whitespace, quote, paren, or end of line)
            let end = url_part
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
                .unwrap_or(url_part.len());
            let url = &url_part[..end];
            if url.contains("/pull/") {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Extracts PR number from a GitHub PR URL.
fn extract_pr_number(url: &str) -> Option<u32> {
    url.rsplit('/').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_parse_review_issues_from_yaml() {
        let response = r#"
Here are the review findings:

```yaml
issues:
  - severity: "critical"
    file: "src/main.rs"
    line: 42
    description: "Use of unwrap in production code"
    suggestion: "Replace with ? operator"
  - severity: "minor"
    file: "src/lib.rs"
    line: 10
    description: "Missing doc comment"
    suggestion: "Add /// documentation"
```
"#;

        let issues = parse_review_issues(response);
        assert_eq!(issues.len(), 1); // Only critical, not minor
        assert!(issues[0].contains("unwrap"));
    }

    #[test]
    fn test_should_return_empty_for_no_issues() {
        let response = r#"
```yaml
issues: []
```
"#;

        let issues = parse_review_issues(response);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_should_parse_review_major_issues() {
        let response = r#"
```yaml
issues:
  - severity: "major"
    file: "src/db.rs"
    line: 100
    description: "SQL injection vulnerability"
    suggestion: "Use parameterized queries"
  - severity: "major"
    file: "src/api.rs"
    line: 55
    description: "Missing authentication check"
    suggestion: "Add auth middleware"
```
"#;

        let issues = parse_review_issues(response);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("SQL injection"));
        assert!(issues[1].contains("authentication"));
    }

    #[test]
    fn test_should_parse_verification_passed() {
        let response = r#"
```yaml
result: "passed"
total_count: 5
checks:
  - name: "cargo build"
    status: "passed"
```
"#;

        let (passed, failed) = parse_verification_result(response);
        assert_eq!(passed, 5);
        assert!(failed.is_empty());
    }

    #[test]
    fn test_should_parse_verification_failed() {
        let response = r#"
```yaml
result: "failed"
checks:
  - name: "cargo build"
    status: "passed"
  - name: "cargo test"
    status: "failed"
    details: "2 tests failed"
failed_count: 1
total_count: 2
```
"#;

        let (passed, failed) = parse_verification_result(response);
        assert_eq!(passed, 1);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains("cargo test"));
    }

    #[test]
    fn test_should_extract_pr_url() {
        let text = "PR created: https://github.com/org/repo/pull/42\nDone!";
        assert_eq!(
            extract_pr_url(text),
            Some("https://github.com/org/repo/pull/42".to_string())
        );
    }

    #[test]
    fn test_should_extract_pr_number() {
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/42"),
            Some(42)
        );
    }

    #[test]
    fn test_should_find_yaml_block() {
        let text = "Some text\n```yaml\nissues: []\n```\nMore text";
        let yaml = extract_yaml_block(text);
        assert_eq!(yaml, Some("issues: []".to_string()));
    }

    #[test]
    fn test_should_return_none_for_no_yaml_block() {
        let text = "This is plain text with no code blocks at all.";
        let yaml = extract_yaml_block(text);
        assert!(yaml.is_none());
    }

    #[test]
    fn test_should_extract_first_yaml_block_from_multiple() {
        let text = r#"
First block:
```yaml
issues:
  - severity: "critical"
    description: "First issue"
```

Second block:
```yaml
result: "passed"
```
"#;

        let yaml = extract_yaml_block(text);
        assert!(yaml.is_some());
        let content = yaml.unwrap();
        // Should extract the first ```yaml block
        assert!(content.contains("issues:"));
    }

    #[test]
    fn test_should_extract_yaml_from_unmarked_code_block() {
        let text = "Here are results:\n```\nissues:\n  - severity: critical\n```\nEnd.";
        let yaml = extract_yaml_block(text);
        assert!(yaml.is_some());
        assert!(yaml.unwrap().contains("issues:"));
    }

    #[test]
    fn test_should_fallback_to_full_text_when_contains_yaml_markers() {
        let text = "result: passed\ntotal_count: 3";
        let yaml = extract_yaml_block(text);
        assert!(yaml.is_some());
        assert_eq!(yaml.unwrap(), text);
    }

    #[test]
    fn test_should_extract_pr_number_from_valid_url() {
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/123"),
            Some(123)
        );
        assert_eq!(
            extract_pr_number("https://github.com/my-org/my-repo/pull/1"),
            Some(1)
        );
    }

    #[test]
    fn test_should_return_none_for_non_numeric_pr_number() {
        assert_eq!(extract_pr_number("https://github.com/org/repo/pull/"), None);
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/abc"),
            None
        );
    }

    #[test]
    fn test_should_return_none_when_no_pr_url_found() {
        let text = "No PR URL here, just some text.";
        assert!(extract_pr_url(text).is_none());
    }

    #[test]
    fn test_should_extract_pr_url_from_markdown_link() {
        let text = "Created [PR #42](https://github.com/org/repo/pull/42) for review.";
        let url = extract_pr_url(text);
        assert_eq!(url, Some("https://github.com/org/repo/pull/42".to_string()));
    }

    #[test]
    fn test_should_not_extract_non_pr_github_url() {
        let text = "See https://github.com/org/repo/issues/10 for details.";
        assert!(extract_pr_url(text).is_none());
    }

    #[test]
    fn test_should_parse_review_with_no_yaml_structure() {
        let text = "The code looks good! No issues found.";
        let issues = parse_review_issues(text);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_should_treat_unparsable_verification_as_failure() {
        let text = "All tests passed successfully!";
        let (passed, failed) = parse_verification_result(text);
        // Unparsable responses must not be treated as "all passed"
        assert_eq!(passed, 0);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains("Unable to parse"));
    }
}
