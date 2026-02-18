//! Core execution engine implementation.
//!
//! The `Engine` orchestrates all CODA operations: initialization, planning,
//! execution, and cleanup. It delegates git/gh operations through the
//! [`GitOps`](crate::git::GitOps) and [`GhOps`](crate::gh::GhOps) traits,
//! and feature discovery through [`FeatureScanner`](crate::scanner::FeatureScanner).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_agent_sdk_rs::Message;
use coda_pm::PromptManager;
use serde::Serialize;
use tracing::{debug, info, warn};

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

        // Create PromptManager and load built-in templates
        let mut pm = PromptManager::new();
        let builtin_templates_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("crates/coda-pm/templates"));

        if let Some(ref dir) = builtin_templates_dir
            && dir.exists()
        {
            pm.load_from_dir(dir)?;
            info!(dir = %dir.display(), "Loaded built-in templates");
        }

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
    /// The init flow performs two agent calls:
    /// 1. `query(Planner)` with `init/analyze_repo` to analyze the repository
    ///    structure, tech stack, and architecture.
    /// 2. `query(Coder)` with `init/setup_project` to create `.coda/`, `.trees/`,
    ///    `config.yml`, `.coda.md`, and update `.gitignore`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if the project is already initialized
    /// (`.coda/` exists), or `CoreError::AgentError` if agent calls fail.
    pub async fn init(&self) -> Result<(), CoreError> {
        // 1. Check if .coda/ already exists
        if self.project_root.join(".coda").exists() {
            return Err(CoreError::ConfigError(
                "Project already initialized. .coda/ directory exists.".into(),
            ));
        }

        // 2. Render system prompt for init
        let system_prompt = self.pm.render("init/system", minijinja::context!())?;

        // 3. Analyze repository structure
        let repo_tree = gather_repo_tree(&self.project_root)?;
        let file_samples = gather_file_samples(&self.project_root)?;

        let analyze_prompt = self.pm.render(
            "init/analyze_repo",
            minijinja::context!(
                repo_tree => repo_tree,
                file_samples => file_samples,
            ),
        )?;

        debug!("Analyzing repository structure...");

        let planner_options = AgentProfile::Planner.to_options(
            &system_prompt,
            self.project_root.clone(),
            5, // max_turns for analysis
            self.config.agent.max_budget_usd,
            &self.config.agent.model,
        );

        let messages = claude_agent_sdk_rs::query(analyze_prompt, Some(planner_options))
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;

        // Extract analysis result text from messages
        let analysis_result = extract_text_from_messages(&messages);
        debug!(
            analysis_len = analysis_result.len(),
            "Repository analysis complete"
        );

        // 4. Setup project structure
        let setup_prompt = self.pm.render(
            "init/setup_project",
            minijinja::context!(
                project_root => self.project_root.display().to_string(),
                analysis_result => analysis_result,
            ),
        )?;

        debug!("Setting up project structure...");

        let coder_options = AgentProfile::Coder.to_options(
            &system_prompt,
            self.project_root.clone(),
            10, // max_turns for setup
            self.config.agent.max_budget_usd,
            &self.config.agent.model,
        );

        let _messages = claude_agent_sdk_rs::query(setup_prompt, Some(coder_options))
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;

        info!("Project initialized successfully");
        Ok(())
    }

    /// Starts an interactive planning session for a feature.
    ///
    /// Creates a `PlanSession` wrapping a `ClaudeClient` with the Planner
    /// profile for multi-turn conversation. The session must be explicitly
    /// connected and finalized by the caller (typically the CLI layer).
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the session cannot be created (e.g., template
    /// rendering fails).
    pub fn plan(&self, feature_slug: &str) -> Result<PlanSession, CoreError> {
        info!(feature_slug, "Starting planning session");
        PlanSession::new(
            feature_slug.to_string(),
            self.project_root.clone(),
            &self.pm,
            &self.config,
            Arc::clone(&self.git),
        )
    }

    /// Lists all features by scanning worktrees in `.trees/`.
    ///
    /// Delegates to [`FeatureScanner::list`].
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.trees/` does not exist.
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
        )?;
        if let Some(tx) = progress_tx {
            runner.set_progress_sender(tx);
        }
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

            removed.push(c.clone());
        }

        Ok(removed)
    }

    /// Checks a single feature's PR status. Returns a [`CleanedWorktree`]
    /// candidate if the PR is merged or closed, `None` otherwise.
    fn check_feature_pr_status(
        &self,
        feature: &crate::state::FeatureState,
    ) -> Result<Option<CleanedWorktree>, CoreError> {
        let slug = &feature.feature.slug;
        let branch = &feature.git.branch;

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

/// Extracts all text content from a sequence of Claude SDK messages.
///
/// Iterates through assistant messages, collecting text blocks into a
/// single concatenated string. Non-text content blocks are skipped.
fn extract_text_from_messages(messages: &[Message]) -> String {
    let mut text_parts: Vec<String> = Vec::new();

    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.message.content {
                    if let claude_agent_sdk_rs::ContentBlock::Text(text_block) = block {
                        text_parts.push(text_block.text.clone());
                    }
                }
            }
            Message::Result(result) => {
                if let Some(ref result_text) = result.result {
                    text_parts.push(result_text.clone());
                }
            }
            _ => {}
        }
    }

    text_parts.join("\n")
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
    fn test_should_extract_text_from_assistant_messages() {
        let messages = vec![
            Message::Assistant(claude_agent_sdk_rs::AssistantMessage {
                message: claude_agent_sdk_rs::AssistantMessageInner {
                    content: vec![claude_agent_sdk_rs::ContentBlock::Text(
                        claude_agent_sdk_rs::TextBlock {
                            text: "Hello from assistant".to_string(),
                        },
                    )],
                    model: None,
                    id: None,
                    stop_reason: None,
                    usage: None,
                    error: None,
                },
                parent_tool_use_id: None,
                session_id: None,
                uuid: None,
            }),
            Message::Result(claude_agent_sdk_rs::ResultMessage {
                subtype: "success".to_string(),
                duration_ms: 100,
                duration_api_ms: 80,
                is_error: false,
                num_turns: 1,
                session_id: "test".to_string(),
                total_cost_usd: Some(0.01),
                usage: None,
                result: Some("Result text".to_string()),
                structured_output: None,
            }),
        ];

        let text = extract_text_from_messages(&messages);
        assert!(text.contains("Hello from assistant"));
        assert!(text.contains("Result text"));
    }

    #[test]
    fn test_should_return_empty_for_no_text_messages() {
        let messages: Vec<Message> = vec![];
        let text = extract_text_from_messages(&messages);
        assert!(text.is_empty());
    }
}
