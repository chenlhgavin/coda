//! Core execution engine implementation.
//!
//! The `Engine` orchestrates all CODA operations: initialization, planning,
//! execution, and cleanup. It delegates git/gh operations through the
//! [`GitOps`](crate::git::GitOps) and [`GhOps`](crate::gh::GhOps) traits,
//! and feature discovery through [`FeatureScanner`](crate::scanner::FeatureScanner).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_agent_sdk_rs::{Message, ResultMessage};
use coda_pm::PromptManager;
use futures::StreamExt;
use serde::Serialize;
use tracing::{debug, error, info, warn};

use tokio::sync::mpsc::UnboundedSender;

use crate::CoreError;
use crate::config::CodaConfig;
use crate::gh::{DefaultGhOps, GhOps};
use crate::git::{DefaultGitOps, GitOps};
use crate::planner::PlanSession;
use crate::profile::AgentProfile;
use crate::runner::RunEvent;
use crate::scanner::FeatureScanner;
use crate::task::TaskResult;

/// Directories to skip when building the repository tree.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".coda",
    ".trees",
    "target",
    "node_modules",
    ".next",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".cargo",
    "vendor",
    ".idea",
    ".vscode",
];

/// Key files to sample for repository analysis.
const SAMPLE_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "Makefile",
    "Dockerfile",
    "docker-compose.yml",
    "README.md",
    "CLAUDE.md",
    ".gitignore",
    "tsconfig.json",
    "CMakeLists.txt",
    "build.gradle",
    "pom.xml",
];

/// Maximum number of lines to read from each sample file.
const SAMPLE_MAX_LINES: usize = 40;

/// Maximum tree depth when gathering the repository tree.
const TREE_MAX_DEPTH: usize = 4;

/// Phase names used during the init pipeline.
const INIT_PHASE_ANALYZE: &str = "analyze-repo";

/// Phase name for the project setup step.
const INIT_PHASE_SETUP: &str = "setup-project";

/// Progress events emitted during `coda init` for real-time UI updates.
///
/// These events are sent through an [`UnboundedSender`] channel from
/// [`Engine::init()`] to the UI layer, enabling live progress display
/// with streaming AI output.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_core::InitEvent;
///
/// let event = InitEvent::PhaseStarting {
///     name: "analyze-repo".to_string(),
///     index: 0,
///     total: 2,
/// };
/// assert!(matches!(event, InitEvent::PhaseStarting { index: 0, .. }));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum InitEvent {
    /// Init pipeline starting with the ordered phase names.
    InitStarting {
        /// Ordered list of phase names for the init pipeline.
        phases: Vec<String>,
    },

    /// A phase is about to start executing.
    PhaseStarting {
        /// Phase name (e.g., `"analyze-repo"`, `"setup-project"`).
        name: String,
        /// Zero-based phase index.
        index: usize,
        /// Total number of phases.
        total: usize,
    },

    /// A chunk of streaming text from the AI agent.
    StreamText {
        /// The text chunk received from the AI.
        text: String,
    },

    /// A phase completed successfully.
    PhaseCompleted {
        /// Phase name.
        name: String,
        /// Zero-based phase index.
        index: usize,
        /// Wall-clock duration of the phase.
        duration: Duration,
        /// Cost in USD for this phase.
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

    /// The entire init flow has finished.
    InitFinished {
        /// Whether the init completed successfully.
        success: bool,
    },
}

/// A sampled file for repository analysis, serializable for template rendering.
#[derive(Debug, Serialize)]
struct FileSample {
    /// Relative path of the sampled file.
    path: String,
    /// First N lines of file content.
    content: String,
}

/// The core execution engine for CODA.
///
/// Manages project configuration, prompt templates, and orchestrates
/// interactions with the Claude Agent SDK.
pub struct Engine {
    /// Root directory of the project.
    project_root: PathBuf,

    /// Prompt template manager loaded with built-in and custom templates.
    pm: PromptManager,

    /// Project configuration loaded from `.coda/config.yml`.
    config: CodaConfig,

    /// Feature worktree scanner.
    scanner: FeatureScanner,

    /// Git operations implementation (Arc for sharing with sub-sessions).
    git: Arc<dyn GitOps>,

    /// GitHub CLI operations implementation.
    gh: Arc<dyn GhOps>,
}

impl Engine {
    /// Creates a new engine for the given project root.
    ///
    /// Loads configuration from `.coda/config.yml` (falling back to defaults
    /// if the file doesn't exist), initializes the prompt manager with
    /// built-in templates, and loads any custom templates from configured
    /// extra directories.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if configuration parsing fails or template
    /// loading encounters an error.
    pub async fn new(project_root: PathBuf) -> Result<Self, CoreError> {
        // Load config from .coda/config.yml, or use defaults if not present
        let config_path = project_root.join(".coda/config.yml");
        let config = if config_path.exists() {
            let content = fs::read_to_string(&config_path).map_err(|e| {
                CoreError::ConfigError(format!(
                    "Cannot read config file at {}: {e}",
                    config_path.display()
                ))
            })?;
            serde_yaml::from_str::<CodaConfig>(&content).map_err(|e| {
                CoreError::ConfigError(format!(
                    "Invalid YAML in config file at {}: {e}",
                    config_path.display()
                ))
            })?
        } else {
            info!("No .coda/config.yml found, using default configuration");
            CodaConfig::default()
        };

        // Create PromptManager pre-loaded with built-in templates
        let mut pm = PromptManager::with_builtin_templates()?;
        info!(
            template_count = pm.template_count(),
            "Loaded built-in templates"
        );

        // Load custom templates from configured extra directories
        for extra_dir in &config.prompts.extra_dirs {
            let dir = project_root.join(extra_dir);
            if dir.exists() {
                pm.load_from_dir(&dir)?;
                info!(dir = %dir.display(), "Loaded custom templates");
            }
        }

        let scanner = FeatureScanner::new(&project_root);
        let git: Arc<dyn GitOps> = Arc::new(DefaultGitOps::new(project_root.clone()));
        let gh: Arc<dyn GhOps> = Arc::new(DefaultGhOps::new(project_root.clone()));

        Ok(Self {
            project_root,
            pm,
            config,
            scanner,
            git,
            gh,
        })
    }

    /// Returns a reference to the project root directory.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Returns a reference to the prompt manager.
    pub fn prompt_manager(&self) -> &PromptManager {
        &self.pm
    }

    /// Returns a reference to the project configuration.
    pub fn config(&self) -> &CodaConfig {
        &self.config
    }

    /// Returns a reference to the git operations implementation.
    pub fn git(&self) -> &dyn GitOps {
        self.git.as_ref()
    }

    /// Returns a reference to the GitHub CLI operations implementation.
    pub fn gh(&self) -> &dyn GhOps {
        self.gh.as_ref()
    }

    /// Initializes the current repository as a CODA project.
    ///
    /// The init flow performs two streaming agent calls:
    /// 1. `query_stream(Planner)` with `init/analyze_repo` to analyze the
    ///    repository structure, tech stack, and architecture.
    /// 2. `query_stream(Coder)` with `init/setup_project` to create `.coda/`,
    ///    `.trees/`, `config.yml`, `.coda.md`, and update `.gitignore`.
    ///
    /// When `force` is `true`, skips the `.coda/` existence check and
    /// reinitializes: updates `config.yml` (preserving user settings)
    /// and regenerates `.coda.md`.
    ///
    /// When `progress_tx` is provided, emits [`InitEvent`]s for real-time
    /// progress display. When `None`, the function behaves silently (useful
    /// for testing or CI).
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if the project is already initialized
    /// (`.coda/` exists) and `force` is `false`, or `CoreError::AgentError`
    /// if agent calls fail.
    pub async fn init(
        &self,
        no_commit: bool,
        force: bool,
        progress_tx: Option<UnboundedSender<InitEvent>>,
    ) -> Result<(), CoreError> {
        // 1. Check if .coda/ already exists (skip when force=true)
        if self.project_root.join(".coda").exists() && !force {
            return Err(CoreError::ConfigError(
                "Project already initialized. .coda/ directory exists.".into(),
            ));
        }

        let phases = vec![INIT_PHASE_ANALYZE.to_string(), INIT_PHASE_SETUP.to_string()];
        emit(
            &progress_tx,
            InitEvent::InitStarting {
                phases: phases.clone(),
            },
        );

        // 2. Render system prompt for init
        let system_prompt = self.pm.render("init/system", minijinja::context!())?;

        // 3. Analyze repository structure (phase 0)
        let analysis_result = match self.run_analyze_phase(&system_prompt, &progress_tx).await {
            Ok(result) => result,
            Err(e) => {
                emit(
                    &progress_tx,
                    InitEvent::PhaseFailed {
                        name: INIT_PHASE_ANALYZE.to_string(),
                        index: 0,
                        error: e.to_string(),
                    },
                );
                emit(&progress_tx, InitEvent::InitFinished { success: false });
                return Err(e);
            }
        };

        // 4. Setup project structure (phase 1)
        if let Err(e) = self
            .run_setup_phase(&system_prompt, &analysis_result, force, &progress_tx)
            .await
        {
            emit(
                &progress_tx,
                InitEvent::PhaseFailed {
                    name: INIT_PHASE_SETUP.to_string(),
                    index: 1,
                    error: e.to_string(),
                },
            );
            emit(&progress_tx, InitEvent::InitFinished { success: false });
            return Err(e);
        }

        // 4b. Verify the agent created critical artifacts
        if !self.project_root.join(".coda").exists() {
            let msg = "Setup phase completed but .coda/ directory was not created. \
                       The AI agent may have failed to execute the setup steps."
                .to_string();
            emit(
                &progress_tx,
                InitEvent::PhaseFailed {
                    name: INIT_PHASE_SETUP.to_string(),
                    index: 1,
                    error: msg.clone(),
                },
            );
            emit(&progress_tx, InitEvent::InitFinished { success: false });
            return Err(CoreError::AgentError(msg));
        }

        // 5. Auto-commit init artifacts unless --no-commit
        if !no_commit {
            self.commit_init_artifacts(force)?;
        }

        emit(&progress_tx, InitEvent::InitFinished { success: true });
        info!("Project initialized successfully");
        Ok(())
    }

    /// Commits init artifacts (`.coda/`, `.coda.md`, `CLAUDE.md`, `.gitignore`)
    /// to the current branch so that `coda plan` worktrees inherit them.
    ///
    /// Uses `--no-verify` because these are CODA-internal files that should
    /// not be gated by project-specific pre-commit hooks.
    ///
    /// When `force` is `true`, the commit message reflects re-initialization.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if staging or committing fails.
    fn commit_init_artifacts(&self, force: bool) -> Result<(), CoreError> {
        let paths: &[&str] = &[".coda/", ".coda.md", "CLAUDE.md", ".gitignore"];
        let message = if force {
            "chore: reinitialize CODA project"
        } else {
            "chore: initialize CODA project"
        };
        commit_coda_artifacts(self.git.as_ref(), &self.project_root, paths, message)
    }

    /// Validates the agent `ResultMessage` for errors and budget exhaustion.
    ///
    /// Always emits a `debug!` log with the full `ResultMessage` fields for
    /// post-mortem diagnostics. When `is_error` is set, emits an `error!`
    /// log and returns `CoreError::AgentError` containing the `result` JSON
    /// (truncated to 500 chars) so the detail surfaces in journal and Slack.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` when `is_error` is true, or
    /// `CoreError::BudgetExhausted` when cost reaches the configured limit.
    fn validate_agent_result(
        &self,
        result_msg: &ResultMessage,
        phase_name: &str,
    ) -> Result<(), CoreError> {
        let result_detail = result_msg
            .result
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_default();

        debug!(
            phase = phase_name,
            subtype = %result_msg.subtype,
            is_error = result_msg.is_error,
            num_turns = result_msg.num_turns,
            session_id = %result_msg.session_id,
            duration_ms = result_msg.duration_ms,
            cost = ?result_msg.total_cost_usd,
            result = %result_detail,
            "Agent phase result",
        );

        if result_msg.is_error {
            error!(
                phase = phase_name,
                subtype = %result_msg.subtype,
                num_turns = result_msg.num_turns,
                session_id = %result_msg.session_id,
                duration_ms = result_msg.duration_ms,
                cost = ?result_msg.total_cost_usd,
                result = %result_detail,
                "Agent reported error",
            );
            let detail_preview = if result_detail.len() > 500 {
                format!("{}…", &result_detail[..500])
            } else {
                result_detail
            };
            return Err(CoreError::AgentError(format!(
                "{phase_name}: agent error \
                 (subtype={}, turns={}, cost={:?}, session={}, detail={detail_preview})",
                result_msg.subtype,
                result_msg.num_turns,
                result_msg.total_cost_usd,
                result_msg.session_id,
            )));
        }

        let budget_limit = self.config.agent.max_budget_usd;
        if let Some(spent) = result_msg.total_cost_usd
            && spent >= budget_limit
        {
            return Err(CoreError::BudgetExhausted {
                spent,
                limit: budget_limit,
            });
        }

        Ok(())
    }

    /// Runs the analyze-repo phase: streams the AI analysis and collects text.
    async fn run_analyze_phase(
        &self,
        system_prompt: &str,
        progress_tx: &Option<UnboundedSender<InitEvent>>,
    ) -> Result<String, CoreError> {
        let repo_tree = gather_repo_tree(&self.project_root)?;
        let file_samples = gather_file_samples(&self.project_root)?;

        let analyze_prompt = self.pm.render(
            "init/analyze_repo",
            minijinja::context!(
                repo_tree => repo_tree,
                file_samples => file_samples,
            ),
        )?;

        emit(
            progress_tx,
            InitEvent::PhaseStarting {
                name: INIT_PHASE_ANALYZE.to_string(),
                index: 0,
                total: 2,
            },
        );

        debug!("Analyzing repository structure...");

        let planner_options = AgentProfile::Planner.to_options(
            system_prompt,
            self.project_root.clone(),
            5, // max_turns for analysis
            self.config.agent.max_budget_usd,
            &self.config.agent.model,
        );

        let phase_start = Instant::now();

        let mut stream = claude_agent_sdk_rs::query_stream(analyze_prompt, Some(planner_options))
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut cost_usd = 0.0;

        while let Some(result) = stream.next().await {
            let message = result.map_err(|e| CoreError::AgentError(e.to_string()))?;
            match message {
                Message::Assistant(ref assistant) => {
                    for block in &assistant.message.content {
                        if let claude_agent_sdk_rs::ContentBlock::Text(text_block) = block {
                            emit(
                                progress_tx,
                                InitEvent::StreamText {
                                    text: text_block.text.clone(),
                                },
                            );
                            text_parts.push(text_block.text.clone());
                        }
                    }
                }
                Message::Result(ref result_msg) => {
                    if let Some(c) = result_msg.total_cost_usd {
                        cost_usd = c;
                    }
                    if result_msg.is_error {
                        let text_len: usize = text_parts.iter().map(String::len).sum();
                        warn!(
                            phase = INIT_PHASE_ANALYZE,
                            agent_text_len = text_len,
                            "Agent errored; collected text may contain diagnostics",
                        );
                    }
                    self.validate_agent_result(result_msg, INIT_PHASE_ANALYZE)?;
                }
                _ => {}
            }
        }

        let duration = phase_start.elapsed();
        let analysis_result = text_parts.join("\n");

        debug!(
            analysis_len = analysis_result.len(),
            "Repository analysis complete"
        );

        emit(
            progress_tx,
            InitEvent::PhaseCompleted {
                name: INIT_PHASE_ANALYZE.to_string(),
                index: 0,
                duration,
                cost_usd,
            },
        );

        Ok(analysis_result)
    }

    /// Runs the setup-project phase: streams the AI setup and collects cost.
    async fn run_setup_phase(
        &self,
        system_prompt: &str,
        analysis_result: &str,
        force: bool,
        progress_tx: &Option<UnboundedSender<InitEvent>>,
    ) -> Result<(), CoreError> {
        let setup_prompt = self.pm.render(
            "init/setup_project",
            minijinja::context!(
                project_root => self.project_root.display().to_string(),
                analysis_result => analysis_result,
                force => force,
            ),
        )?;

        emit(
            progress_tx,
            InitEvent::PhaseStarting {
                name: INIT_PHASE_SETUP.to_string(),
                index: 1,
                total: 2,
            },
        );

        debug!("Setting up project structure...");

        let coder_options = AgentProfile::Coder.to_options(
            system_prompt,
            self.project_root.clone(),
            10, // max_turns for setup
            self.config.agent.max_budget_usd,
            &self.config.agent.model,
        );

        let phase_start = Instant::now();

        let mut stream = claude_agent_sdk_rs::query_stream(setup_prompt, Some(coder_options))
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut cost_usd = 0.0;

        while let Some(result) = stream.next().await {
            let message = result.map_err(|e| CoreError::AgentError(e.to_string()))?;
            match message {
                Message::Assistant(ref assistant) => {
                    for block in &assistant.message.content {
                        if let claude_agent_sdk_rs::ContentBlock::Text(text_block) = block {
                            emit(
                                progress_tx,
                                InitEvent::StreamText {
                                    text: text_block.text.clone(),
                                },
                            );
                            text_parts.push(text_block.text.clone());
                        }
                    }
                }
                Message::Result(ref result_msg) => {
                    if let Some(c) = result_msg.total_cost_usd {
                        cost_usd = c;
                    }
                    if result_msg.is_error {
                        let text_len: usize = text_parts.iter().map(String::len).sum();
                        warn!(
                            phase = INIT_PHASE_SETUP,
                            agent_text_len = text_len,
                            "Agent errored; collected text may contain diagnostics",
                        );
                    }
                    self.validate_agent_result(result_msg, INIT_PHASE_SETUP)?;
                }
                _ => {}
            }
        }

        let duration = phase_start.elapsed();

        emit(
            progress_tx,
            InitEvent::PhaseCompleted {
                name: INIT_PHASE_SETUP.to_string(),
                index: 1,
                duration,
                cost_usd,
            },
        );

        Ok(())
    }

    /// Starts an interactive planning session for a feature.
    ///
    /// Validates the slug format and checks for duplicate features before
    /// creating a `PlanSession` wrapping a `ClaudeClient` with the Planner
    /// profile for multi-turn conversation. The session must be explicitly
    /// connected and finalized by the caller (typically the CLI layer).
    ///
    /// # Errors
    ///
    /// Returns `CoreError::PlanError` if the slug is invalid or a feature
    /// with the same slug already exists. Returns other `CoreError` variants
    /// if the session cannot be created.
    pub fn plan(&self, feature_slug: &str) -> Result<PlanSession, CoreError> {
        validate_feature_slug(feature_slug)?;

        let worktree_path = self.project_root.join(".trees").join(feature_slug);
        if worktree_path.exists() {
            return Err(CoreError::PlanError(format!(
                "Feature '{feature_slug}' already exists at {}. \
                 Use `coda status {feature_slug}` to check its state, \
                 or choose a different slug.",
                worktree_path.display(),
            )));
        }

        info!(feature_slug, "Starting planning session");
        PlanSession::new(
            feature_slug.to_string(),
            self.project_root.clone(),
            &self.pm,
            &self.config,
            Arc::clone(&self.git),
        )
    }

    /// Lists all features: active worktrees from `.trees/` and merged
    /// features from `.coda/`.
    ///
    /// Delegates to [`FeatureScanner::list`].
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if neither `.trees/` nor `.coda/`
    /// contains any features and `.trees/` does not exist.
    pub fn list_features(&self) -> Result<Vec<crate::state::FeatureState>, CoreError> {
        self.scanner.list()
    }

    /// Returns detailed state for a specific feature identified by its slug.
    ///
    /// Delegates to [`FeatureScanner::get`].
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.trees/` does not exist, or
    /// `CoreError::StateError` if no matching feature is found.
    pub fn feature_status(
        &self,
        feature_slug: &str,
    ) -> Result<crate::state::FeatureState, CoreError> {
        self.scanner.get(feature_slug)
    }

    /// Executes a feature development run through all phases.
    ///
    /// Reads `state.yml` and resumes from the last completed phase. Uses
    /// a single continuous `ClaudeClient` session with the Coder profile
    /// to execute setup → implement → test → review → verify → PR.
    ///
    /// When `progress_tx` is provided, emits [`RunEvent`]s for real-time
    /// progress display.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the runner cannot be created or any phase
    /// fails after all retries.
    pub async fn run(
        &self,
        feature_slug: &str,
        progress_tx: Option<UnboundedSender<RunEvent>>,
    ) -> Result<Vec<TaskResult>, CoreError> {
        info!(feature_slug, "Starting feature run");
        let mut runner = crate::runner::Runner::new(
            feature_slug,
            self.project_root.clone(),
            &self.pm,
            &self.config,
            Arc::clone(&self.git),
            Arc::clone(&self.gh),
            progress_tx,
        )?;
        runner.execute().await
    }

    /// Cleans up worktrees whose corresponding PR has been merged or closed.
    ///
    /// For each feature in `.trees/`:
    /// 1. If `state.yml` contains a PR number, queries its status via `gh pr view`.
    /// 2. Otherwise, queries `gh pr list --head <branch>` to discover the PR.
    /// 3. If the PR is `MERGED` or `CLOSED`, removes the worktree and deletes
    ///    the local branch.
    ///
    /// Scans features and returns candidates whose PR is merged or closed.
    ///
    /// Does **not** delete anything. Use [`remove_worktrees`](Self::remove_worktrees)
    /// to perform the actual removal after user confirmation.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if `.trees/` does not exist.
    pub fn scan_cleanable_worktrees(&self) -> Result<Vec<CleanedWorktree>, CoreError> {
        let features = self.list_features()?;
        let mut candidates = Vec::new();

        for feature in &features {
            match self.check_feature_pr_status(feature) {
                Ok(Some(result)) => candidates.push(result),
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        slug = %feature.feature.slug,
                        error = %e,
                        "Failed to check PR status, skipping"
                    );
                }
            }
        }

        Ok(candidates)
    }

    /// Removes worktrees and branches for the given candidates.
    ///
    /// For each candidate, removes the git worktree, deletes the local branch,
    /// and cleans up the corresponding log directory at `.coda/<slug>/logs/`.
    ///
    /// Designed to be called after [`scan_cleanable_worktrees`](Self::scan_cleanable_worktrees)
    /// and user confirmation.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if a git operation fails during removal.
    pub fn remove_worktrees(
        &self,
        candidates: &[CleanedWorktree],
    ) -> Result<Vec<CleanedWorktree>, CoreError> {
        let mut removed = Vec::new();

        for c in candidates {
            let worktree_abs = self.project_root.join(".trees").join(&c.slug);
            if !worktree_abs.exists() {
                info!(path = %worktree_abs.display(), "Worktree path does not exist, running prune");
                self.git.worktree_prune()?;
            } else {
                self.git.worktree_remove(&worktree_abs, true)?;
            }

            if let Err(e) = self.git.branch_delete(&c.branch) {
                warn!(branch = %c.branch, error = %e, "Failed to delete local branch (may already be deleted)");
            }

            // Clean up log directory in the main repo (best-effort)
            let _ = remove_feature_logs(&self.project_root, &c.slug);

            removed.push(c.clone());
        }

        Ok(removed)
    }

    /// Removes log directories for all features under `.coda/*/logs/`.
    ///
    /// Scans the `.coda/` directory for feature subdirectories that contain
    /// a `logs/` child, deletes each one, and returns the list of feature
    /// slugs whose logs were successfully cleaned.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.coda/` cannot be read.
    pub fn clean_logs(&self) -> Result<Vec<String>, CoreError> {
        let coda_dir = self.project_root.join(".coda");
        if !coda_dir.is_dir() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(&coda_dir).map_err(|e| {
            CoreError::ConfigError(format!(
                "Cannot read .coda/ directory at {}: {e}",
                coda_dir.display()
            ))
        })?;

        let mut cleaned = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }
            let slug = entry.file_name();
            let slug_str = slug.to_string_lossy();
            let logs_dir = entry.path().join("logs");
            if logs_dir.is_dir() && remove_feature_logs(&self.project_root, &slug_str) {
                cleaned.push(slug_str.into_owned());
            }
        }

        cleaned.sort();
        info!(count = cleaned.len(), "Cleaned all feature logs");
        Ok(cleaned)
    }

    /// Checks a single feature's PR status. Returns a [`CleanedWorktree`]
    /// candidate if the PR is merged or closed, `None` otherwise.
    ///
    /// As a defensive check, skips features whose worktree directory no
    /// longer exists under `.trees/` (e.g. ghost entries from merged branches).
    fn check_feature_pr_status(
        &self,
        feature: &crate::state::FeatureState,
    ) -> Result<Option<CleanedWorktree>, CoreError> {
        let slug = &feature.feature.slug;
        let branch = &feature.git.branch;

        let worktree_dir = self.project_root.join(".trees").join(slug);
        if !worktree_dir.is_dir() {
            debug!(
                slug,
                path = %worktree_dir.display(),
                "Worktree directory does not exist, skipping ghost feature"
            );
            return Ok(None);
        }

        let pr_status = if let Some(ref pr) = feature.pr {
            self.gh.pr_view_state(pr.number)?
        } else {
            self.gh.pr_list_by_branch(branch)?
        };

        let Some(pr_status) = pr_status else {
            debug!(slug, branch, "No PR found, skipping");
            return Ok(None);
        };

        let state_upper = pr_status.state.to_uppercase();
        if state_upper != "MERGED" && state_upper != "CLOSED" {
            debug!(
                slug,
                branch,
                state = %pr_status.state,
                "PR still open, skipping"
            );
            return Ok(None);
        }

        Ok(Some(CleanedWorktree {
            slug: slug.clone(),
            branch: branch.clone(),
            pr_number: Some(pr_status.number),
            pr_state: state_upper,
        }))
    }
}

/// Result of cleaning a single worktree.
#[derive(Debug, Clone)]
pub struct CleanedWorktree {
    /// Feature slug.
    pub slug: String,

    /// Git branch name.
    pub branch: String,

    /// PR number if found.
    pub pr_number: Option<u32>,

    /// PR state (e.g., "MERGED", "CLOSED").
    pub pr_state: String,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("project_root", &self.project_root)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Sends an [`InitEvent`] through the optional channel.
///
/// Silently ignores send failures (receiver dropped) and `None` channels.
/// This avoids verbose `if let Some(tx) = &progress_tx { let _ = tx.send(...); }`
/// at every callsite.
///
/// # Examples
///
/// ```
/// use tokio::sync::mpsc;
/// use coda_core::InitEvent;
///
/// let (tx, mut rx) = mpsc::unbounded_channel();
/// coda_core::emit(&Some(tx), InitEvent::InitFinished { success: true });
/// let event = rx.try_recv().unwrap();
/// assert!(matches!(event, InitEvent::InitFinished { success: true }));
/// ```
pub fn emit(tx: &Option<UnboundedSender<InitEvent>>, event: InitEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Stages files, commits, and handles pre-commit hook failures automatically.
///
/// Implements a retry strategy with unrecoverable error detection:
///
/// 1. **Try commit** — runs pre-commit hooks normally.
/// 2. **Check recoverability** — if the hook error is unrecoverable (e.g.
///    `command not found`, missing tool binaries), falls back to
///    `--no-verify` immediately instead of wasting time on retries.
/// 3. **Re-stage & retry** — fixer hooks (e.g. `end-of-file-fixer`, `prettier`)
///    modify files on the first run and pass on the second. Re-staging picks up
///    their modifications.
/// 4. **LLM fix & retry** — if hooks still fail, the error output is sent to a
///    Claude agent that reads and fixes the affected files, then a final commit
///    is attempted.
///
/// # Errors
///
/// Returns `CoreError` if the commit fails after all attempts.
pub async fn commit_with_hooks(
    git: &dyn GitOps,
    cwd: &Path,
    paths: &[&str],
    message: &str,
    pm: &PromptManager,
    config: &CodaConfig,
) -> Result<(), CoreError> {
    git.add(cwd, paths)?;
    if !git.has_staged_changes(cwd) {
        return Ok(());
    }

    // Attempt 1: normal commit
    let first_err = match git.commit(cwd, message) {
        Ok(()) => return Ok(()),
        Err(e) => {
            info!("Commit failed (hooks may have modified files), retrying");
            e
        }
    };

    // Fast path: if the hook error is unrecoverable (missing tool, bad
    // interpreter, etc.), skip the retry/LLM layers and commit directly
    // with --no-verify. Re-staging and LLM fixes cannot install missing
    // binaries, so retrying would just waste time and money.
    if is_unrecoverable_hook_error(&first_err.to_string()) {
        warn!(
            "Hook failure is unrecoverable (missing tool/command), \
             falling back to --no-verify"
        );
        git.add(cwd, paths)?;
        if !git.has_staged_changes(cwd) {
            return Ok(());
        }
        return git.commit_internal(cwd, message);
    }

    // Attempt 2: re-stage (picks up fixer-hook modifications) and retry
    git.add(cwd, paths)?;
    if !git.has_staged_changes(cwd) {
        info!("Hooks fixed all files, nothing left to commit");
        return Ok(());
    }
    match git.commit(cwd, message) {
        Ok(()) => return Ok(()),
        Err(second_err) => {
            info!("Retry failed, invoking LLM to fix hook errors");
            let hook_errors = second_err.to_string();
            if let Err(e) = fix_hook_errors_with_llm(&hook_errors, cwd, pm, config).await {
                warn!(error = %e, "LLM hook-fix failed, proceeding with final commit attempt");
            }
        }
    }

    // Attempt 3: re-stage LLM fixes and commit
    git.add(cwd, paths)?;
    if !git.has_staged_changes(cwd) {
        info!("LLM fixes resolved all issues, nothing left to commit");
        return Ok(());
    }
    git.commit(cwd, message)
}

/// Checks whether a pre-commit hook error indicates an unrecoverable failure.
///
/// Unrecoverable errors are those that cannot be fixed by re-staging files or
/// by an LLM agent — typically missing tool binaries or broken interpreters.
/// When detected, the caller should fall back to `--no-verify`.
fn is_unrecoverable_hook_error(error_msg: &str) -> bool {
    let lower = error_msg.to_lowercase();
    // "command not found" — shell reports missing binary (exit code 127)
    // "no such file or directory" — missing interpreter or tool binary
    // "not found in PATH" / "not found on PATH" — various tools reporting PATH issues
    lower.contains("command not found")
        || lower.contains("no such file or directory")
        || lower.contains("not found in path")
        || lower.contains("not found on path")
}

/// Invokes a Claude agent to fix files based on pre-commit hook error output.
///
/// Uses `AgentProfile::Coder` with a low turn limit so the agent can read
/// the affected files and write fixes. The agent works directly in `cwd`.
async fn fix_hook_errors_with_llm(
    hook_errors: &str,
    cwd: &Path,
    pm: &PromptManager,
    config: &CodaConfig,
) -> Result<(), CoreError> {
    let fix_prompt = pm.render(
        "commit/fix_hook_errors",
        minijinja::context!(hook_errors => hook_errors),
    )?;

    let options = AgentProfile::Coder.to_options(
        "You are a code formatter. Fix only the issues reported by pre-commit hooks. \
         Do not change semantic content.",
        cwd.to_path_buf(),
        3,
        config.agent.max_budget_usd,
        &config.agent.model,
    );

    let mut stream = claude_agent_sdk_rs::query_stream(fix_prompt, Some(options))
        .await
        .map_err(|e| CoreError::AgentError(e.to_string()))?;

    while let Some(result) = stream.next().await {
        let _ = result.map_err(|e| CoreError::AgentError(e.to_string()))?;
    }

    info!("LLM hook-fix agent completed");
    Ok(())
}

/// Stages and commits CODA-internal artifacts without running pre-commit hooks.
///
/// CODA-generated files (`.coda/`, `.coda.md`, `CLAUDE.md`, etc.) are tooling
/// infrastructure and should not be gated by project-specific linters or
/// formatters. This function uses `git commit --no-verify` to bypass hooks
/// entirely.
///
/// Silently succeeds if there are no staged changes after `git add`.
///
/// # Errors
///
/// Returns `CoreError::GitError` if staging or committing fails.
pub fn commit_coda_artifacts(
    git: &dyn GitOps,
    cwd: &Path,
    paths: &[&str],
    message: &str,
) -> Result<(), CoreError> {
    // Filter to paths that actually exist on disk to avoid git add errors
    let existing: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|p| cwd.join(p).exists())
        .collect();
    if existing.is_empty() {
        return Ok(());
    }
    git.add(cwd, &existing)?;
    if !git.has_staged_changes(cwd) {
        return Ok(());
    }
    git.commit_internal(cwd, message)
}

/// Maximum length for a feature slug (keeps branch names and paths manageable).
const SLUG_MAX_LEN: usize = 64;

/// Validates that a feature slug is URL-safe and suitable for use in
/// branch names and filesystem paths.
///
/// Accepted format: lowercase ASCII alphanumeric characters and hyphens,
/// starting and ending with an alphanumeric character (e.g., `"add-user-auth"`).
///
/// # Errors
///
/// Returns `CoreError::PlanError` with a human-readable explanation when
/// validation fails.
pub fn validate_feature_slug(slug: &str) -> Result<(), CoreError> {
    if slug.is_empty() {
        return Err(CoreError::PlanError(
            "Feature slug cannot be empty.".to_string(),
        ));
    }
    if slug.len() > SLUG_MAX_LEN {
        return Err(CoreError::PlanError(format!(
            "Feature slug is too long ({} chars, max {SLUG_MAX_LEN}).",
            slug.len(),
        )));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(CoreError::PlanError(format!(
            "Feature slug '{slug}' contains invalid characters. \
             Only lowercase letters, digits, and hyphens are allowed.",
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(CoreError::PlanError(format!(
            "Feature slug '{slug}' must not start or end with a hyphen.",
        )));
    }
    if slug.contains("--") {
        return Err(CoreError::PlanError(format!(
            "Feature slug '{slug}' must not contain consecutive hyphens.",
        )));
    }
    Ok(())
}

/// Removes the log directory for a feature at `.coda/<slug>/logs/`.
///
/// Returns `true` if the directory was successfully removed or did not
/// exist. Returns `false` if deletion failed (a warning is logged).
///
/// # Examples
///
/// ```no_run
/// # use std::path::Path;
/// // After cleaning a worktree, remove its logs:
/// let removed = coda_core::remove_feature_logs(Path::new("/repo"), "my-feature");
/// ```
pub fn remove_feature_logs(project_root: &Path, slug: &str) -> bool {
    let logs_dir = project_root.join(".coda").join(slug).join("logs");
    match fs::remove_dir_all(&logs_dir) {
        Ok(()) => {
            info!(slug, path = %logs_dir.display(), "Removed feature log directory");
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(slug, path = %logs_dir.display(), "No log directory to clean");
            true
        }
        Err(e) => {
            warn!(
                slug,
                path = %logs_dir.display(),
                error = %e,
                "Failed to remove feature log directory"
            );
            false
        }
    }
}

/// Builds a simple directory tree listing of the repository.
///
/// Skips common non-source directories (`.git`, `target`, `node_modules`, etc.)
/// and limits depth to [`TREE_MAX_DEPTH`] levels.
fn gather_repo_tree(root: &Path) -> Result<String, CoreError> {
    let mut output = String::new();
    build_tree(root, "", &mut output, 0)?;
    Ok(output)
}

/// Recursively builds the tree string for [`gather_repo_tree`].
fn build_tree(
    current: &Path,
    prefix: &str,
    output: &mut String,
    depth: usize,
) -> Result<(), CoreError> {
    if depth > TREE_MAX_DEPTH {
        return Ok(());
    }

    let mut entries: Vec<_> = fs::read_dir(current)?
        .filter_map(|e| e.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Skip hidden files (except key dotfiles)
            if name_str.starts_with('.')
                && !matches!(
                    name_str.as_ref(),
                    ".gitignore" | ".coda.md" | ".env.example"
                )
            {
                return false;
            }
            // Skip excluded directories
            if entry.file_type().is_ok_and(|ft| ft.is_dir())
                && SKIP_DIRS.contains(&name_str.as_ref())
            {
                return false;
            }
            true
        })
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let total = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let is_last = i == total - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            output.push_str(&format!("{prefix}{connector}{name_str}/\n"));
            build_tree(
                &entry.path(),
                &format!("{prefix}{child_prefix}"),
                output,
                depth + 1,
            )?;
        } else {
            output.push_str(&format!("{prefix}{connector}{name_str}\n"));
        }
    }

    Ok(())
}

/// Reads the first [`SAMPLE_MAX_LINES`] lines from key project files.
///
/// Returns a list of [`FileSample`] structs suitable for template rendering.
/// Only files that actually exist in the repository are included.
fn gather_file_samples(root: &Path) -> Result<Vec<FileSample>, CoreError> {
    let mut samples = Vec::new();

    for &filename in SAMPLE_FILES {
        let path = root.join(filename);
        if path.is_file() {
            let content = fs::read_to_string(&path)?;
            let truncated: String = content
                .lines()
                .take(SAMPLE_MAX_LINES)
                .collect::<Vec<_>>()
                .join("\n");

            samples.push(FileSample {
                path: filename.to_string(),
                content: truncated,
            });
        }
    }

    Ok(samples)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::state::{
        FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseKind, PhaseRecord, PhaseStatus,
        TokenCost, TotalStats,
    };

    fn make_state(slug: &str) -> FeatureState {
        let now = chrono::Utc::now();
        FeatureState {
            feature: FeatureInfo {
                slug: slug.to_string(),
                created_at: now,
                updated_at: now,
            },
            status: FeatureStatus::Planned,
            current_phase: 0,
            git: GitInfo {
                worktree_path: std::path::PathBuf::from(format!(".trees/{slug}")),
                branch: format!("feature/{slug}"),
                base_branch: "main".to_string(),
            },
            phases: vec![
                PhaseRecord {
                    name: "dev".to_string(),
                    kind: PhaseKind::Dev,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "review".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "verify".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "update-docs".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
            ],
            pr: None,
            total: TotalStats::default(),
        }
    }

    fn write_state(root: &std::path::Path, slug: &str, state: &FeatureState) {
        let dir = root.join(".trees").join(slug).join(".coda").join(slug);
        fs::create_dir_all(&dir).expect("create state dir");
        let yaml = serde_yaml::to_string(state).expect("serialize state");
        fs::write(dir.join("state.yml"), yaml).expect("write state.yml");
    }

    async fn make_engine(root: &std::path::Path) -> Engine {
        Engine::new(root.to_path_buf())
            .await
            .expect("create Engine")
    }

    #[test]
    fn test_should_emit_event_when_sender_is_some() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        emit(&Some(tx), InitEvent::InitFinished { success: true });

        let event = rx.try_recv().expect("should receive event");
        assert!(matches!(event, InitEvent::InitFinished { success: true }));
    }

    #[test]
    fn test_should_not_panic_when_emitting_to_none() {
        emit(&None, InitEvent::InitFinished { success: false });
    }

    #[test]
    fn test_should_not_panic_when_receiver_is_dropped() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        emit(&Some(tx), InitEvent::InitFinished { success: true });
    }

    #[test]
    fn test_should_emit_all_init_event_variants() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sender = Some(tx);

        emit(
            &sender,
            InitEvent::InitStarting {
                phases: vec!["analyze-repo".to_string(), "setup-project".to_string()],
            },
        );
        emit(
            &sender,
            InitEvent::PhaseStarting {
                name: "analyze-repo".to_string(),
                index: 0,
                total: 2,
            },
        );
        emit(
            &sender,
            InitEvent::StreamText {
                text: "some output".to_string(),
            },
        );
        emit(
            &sender,
            InitEvent::PhaseCompleted {
                name: "analyze-repo".to_string(),
                index: 0,
                duration: Duration::from_secs(30),
                cost_usd: 0.01,
            },
        );
        emit(
            &sender,
            InitEvent::PhaseFailed {
                name: "setup-project".to_string(),
                index: 1,
                error: "agent error".to_string(),
            },
        );
        emit(&sender, InitEvent::InitFinished { success: false });

        // Verify all 6 events arrived in order
        assert!(matches!(rx.try_recv(), Ok(InitEvent::InitStarting { .. })));
        assert!(matches!(
            rx.try_recv(),
            Ok(InitEvent::PhaseStarting { index: 0, .. })
        ));
        assert!(matches!(rx.try_recv(), Ok(InitEvent::StreamText { .. })));
        assert!(matches!(
            rx.try_recv(),
            Ok(InitEvent::PhaseCompleted { index: 0, .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(InitEvent::PhaseFailed { index: 1, .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(InitEvent::InitFinished { success: false })
        ));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_should_list_features_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".trees")).expect("mkdir");
        let engine = make_engine(tmp.path()).await;

        let features = engine.list_features().expect("list");
        assert!(features.is_empty());
    }

    #[tokio::test]
    async fn test_should_list_features_single() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = make_state("add-auth");
        write_state(tmp.path(), "add-auth", &state);
        let engine = make_engine(tmp.path()).await;

        let features = engine.list_features().expect("list");
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].feature.slug, "add-auth");
    }

    #[tokio::test]
    async fn test_should_list_features_sorted_by_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_state(tmp.path(), "zzz-last", &make_state("zzz-last"));
        write_state(tmp.path(), "aaa-first", &make_state("aaa-first"));
        write_state(tmp.path(), "mmm-middle", &make_state("mmm-middle"));
        let engine = make_engine(tmp.path()).await;

        let features = engine.list_features().expect("list");
        assert_eq!(features.len(), 3);
        assert_eq!(features[0].feature.slug, "aaa-first");
        assert_eq!(features[1].feature.slug, "mmm-middle");
        assert_eq!(features[2].feature.slug, "zzz-last");
    }

    #[tokio::test]
    async fn test_should_list_features_skip_invalid_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_state(tmp.path(), "good", &make_state("good"));
        // Write invalid YAML
        let bad_dir = tmp.path().join(".trees/bad/.coda/bad");
        fs::create_dir_all(&bad_dir).expect("mkdir");
        fs::write(bad_dir.join("state.yml"), "not: valid: yaml: [").expect("write");
        let engine = make_engine(tmp.path()).await;

        let features = engine.list_features().expect("list");
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].feature.slug, "good");
    }

    #[tokio::test]
    async fn test_should_list_features_error_when_no_trees_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engine = make_engine(tmp.path()).await;

        let result = engine.list_features();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains(".trees/"));
    }

    #[tokio::test]
    async fn test_should_get_feature_status_direct_lookup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = make_state("add-auth");
        write_state(tmp.path(), "add-auth", &state);
        let engine = make_engine(tmp.path()).await;

        let found = engine.feature_status("add-auth").expect("status");
        assert_eq!(found.feature.slug, "add-auth");
        assert_eq!(found.git.branch, "feature/add-auth");
    }

    #[tokio::test]
    async fn test_should_get_feature_status_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_state(tmp.path(), "existing", &make_state("existing"));
        let engine = make_engine(tmp.path()).await;

        let result = engine.feature_status("nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonexistent"));
        assert!(err.contains("existing"));
    }

    #[tokio::test]
    async fn test_should_get_feature_status_error_when_no_trees_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engine = make_engine(tmp.path()).await;

        let result = engine.feature_status("anything");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains(".trees/"));
    }

    #[test]
    fn test_should_gather_repo_tree_from_temp_dir() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let root = tmp.path();

        // Create a simple structure
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src/main.rs"), "fn main() {}").expect("write");
        fs::write(root.join("Cargo.toml"), "[package]").expect("write");
        fs::create_dir_all(root.join("target/debug")).expect("mkdir");

        let tree = gather_repo_tree(root).expect("gather_repo_tree");

        assert!(tree.contains("src/"));
        assert!(tree.contains("Cargo.toml"));
        // target/ should be skipped
        assert!(!tree.contains("target"));
    }

    #[test]
    fn test_should_gather_file_samples() {
        let tmp = tempfile::tempdir().expect("failed to create temp dir");
        let root = tmp.path();

        fs::write(root.join("Cargo.toml"), "[package]\nname = \"test\"\n").expect("write");
        fs::write(root.join("README.md"), "# Test\nHello world").expect("write");

        let samples = gather_file_samples(root).expect("gather_file_samples");

        assert_eq!(samples.len(), 2);
        let names: Vec<&str> = samples.iter().map(|s| s.path.as_str()).collect();
        assert!(names.contains(&"Cargo.toml"));
        assert!(names.contains(&"README.md"));
    }

    #[test]
    fn test_should_accept_valid_slugs() {
        assert!(validate_feature_slug("add-auth").is_ok());
        assert!(validate_feature_slug("feature123").is_ok());
        assert!(validate_feature_slug("a").is_ok());
        assert!(validate_feature_slug("a-b-c").is_ok());
    }

    #[test]
    fn test_should_reject_empty_slug() {
        let err = validate_feature_slug("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn test_should_reject_slug_with_invalid_chars() {
        assert!(validate_feature_slug("Add-Auth").is_err());
        assert!(validate_feature_slug("add auth").is_err());
        assert!(validate_feature_slug("add/auth").is_err());
        assert!(validate_feature_slug("add_auth").is_err());
        assert!(validate_feature_slug("add.auth").is_err());
    }

    #[test]
    fn test_should_reject_slug_with_leading_trailing_hyphen() {
        assert!(validate_feature_slug("-add").is_err());
        assert!(validate_feature_slug("add-").is_err());
    }

    #[test]
    fn test_should_reject_slug_with_consecutive_hyphens() {
        assert!(validate_feature_slug("add--auth").is_err());
    }

    #[test]
    fn test_should_reject_slug_too_long() {
        let long_slug = "a".repeat(65);
        assert!(validate_feature_slug(&long_slug).is_err());
    }

    #[test]
    fn test_should_remove_existing_log_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let logs_dir = root.join(".coda/my-feature/logs");
        fs::create_dir_all(&logs_dir).expect("mkdir");
        fs::write(logs_dir.join("run-20260101T000000.log"), "log data").expect("write");

        assert!(remove_feature_logs(root, "my-feature"));

        assert!(!logs_dir.exists());
        // The parent .coda/my-feature/ should still exist
        assert!(root.join(".coda/my-feature").exists());
    }

    #[test]
    fn test_should_ignore_missing_log_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // No .coda directory at all — should not panic and should return true
        assert!(remove_feature_logs(root, "nonexistent"));
    }

    #[tokio::test]
    async fn test_should_clean_logs_for_multiple_features() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Create log directories for two features
        let logs_a = root.join(".coda/feature-a/logs");
        let logs_b = root.join(".coda/feature-b/logs");
        fs::create_dir_all(&logs_a).expect("mkdir");
        fs::create_dir_all(&logs_b).expect("mkdir");
        fs::write(logs_a.join("run.log"), "data").expect("write");
        fs::write(logs_b.join("run.log"), "data").expect("write");

        // Feature without logs should be skipped
        fs::create_dir_all(root.join(".coda/feature-c")).expect("mkdir");

        let engine = make_engine(root).await;
        let cleaned = engine.clean_logs().expect("clean_logs");

        assert_eq!(cleaned, vec!["feature-a", "feature-b"]);
        assert!(!logs_a.exists());
        assert!(!logs_b.exists());
        // Parent dirs remain
        assert!(root.join(".coda/feature-a").exists());
        assert!(root.join(".coda/feature-b").exists());
    }

    #[tokio::test]
    async fn test_should_return_empty_when_no_coda_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let engine = make_engine(tmp.path()).await;

        let cleaned = engine.clean_logs().expect("clean_logs");
        assert!(cleaned.is_empty());
    }

    #[tokio::test]
    async fn test_should_return_empty_when_no_features_have_logs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join(".coda/some-feature")).expect("mkdir");

        let engine = make_engine(root).await;
        let cleaned = engine.clean_logs().expect("clean_logs");
        assert!(cleaned.is_empty());
    }
}
