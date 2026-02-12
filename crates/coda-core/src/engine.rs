//! Core execution engine implementation.
//!
//! The `Engine` orchestrates all CODA operations: initialization, planning,
//! and execution. It manages configuration, prompt templates, and dispatches
//! tasks to the appropriate agent profiles.

use std::fs;
use std::path::{Path, PathBuf};

use claude_agent_sdk_rs::Message;
use coda_pm::PromptManager;
use serde::Serialize;
use tracing::{debug, info};

use crate::CoreError;
use crate::config::CodaConfig;
use crate::planner::PlanSession;
use crate::profile::AgentProfile;
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

        Ok(Self {
            project_root,
            pm,
            config,
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

    /// Returns a mutable reference to the prompt manager.
    pub fn prompt_manager_mut(&mut self) -> &mut PromptManager {
        &mut self.pm
    }

    /// Returns a reference to the project configuration.
    pub fn config(&self) -> &CodaConfig {
        &self.config
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
    /// (`.coda/` exists), or `CoreError::AgentSdkError` if agent calls fail.
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
        );

        let messages = claude_agent_sdk_rs::query(analyze_prompt, Some(planner_options))
            .await
            .map_err(|e| CoreError::AgentSdkError(e.to_string()))?;

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
        );

        let _messages = claude_agent_sdk_rs::query(setup_prompt, Some(coder_options))
            .await
            .map_err(|e| CoreError::AgentSdkError(e.to_string()))?;

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
        )
    }

    /// Lists all features found in `.coda/` by reading their `state.yml` files.
    ///
    /// Scans the `.coda/` directory for subdirectories containing a `state.yml`
    /// file and returns the parsed feature states sorted by feature ID.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.coda/` does not exist, or
    /// `CoreError` if a `state.yml` file cannot be read or parsed.
    pub fn list_features(&self) -> Result<Vec<crate::state::FeatureState>, CoreError> {
        let coda_dir = self.project_root.join(".coda");
        if !coda_dir.is_dir() {
            return Err(CoreError::ConfigError(
                "No .coda/ directory found. Run `coda init` first.".into(),
            ));
        }

        let mut features = Vec::new();
        let entries = fs::read_dir(&coda_dir)?;

        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }

            let state_path = entry.path().join("state.yml");
            if !state_path.is_file() {
                continue;
            }

            let content = fs::read_to_string(&state_path).map_err(|e| {
                CoreError::StateError(format!("Cannot read {}: {e}", state_path.display()))
            })?;

            match serde_yaml::from_str::<crate::state::FeatureState>(&content) {
                Ok(state) => features.push(state),
                Err(e) => {
                    debug!(
                        path = %state_path.display(),
                        error = %e,
                        "Skipping invalid state.yml"
                    );
                }
            }
        }

        // Sort by feature ID
        features.sort_by(|a, b| a.feature.id.cmp(&b.feature.id));
        Ok(features)
    }

    /// Returns detailed state for a specific feature identified by its slug.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.coda/` does not exist, or
    /// `CoreError::StateError` if no matching feature is found.
    pub fn feature_status(
        &self,
        feature_slug: &str,
    ) -> Result<crate::state::FeatureState, CoreError> {
        let coda_dir = self.project_root.join(".coda");
        if !coda_dir.is_dir() {
            return Err(CoreError::ConfigError(
                "No .coda/ directory found. Run `coda init` first.".into(),
            ));
        }

        let entries = fs::read_dir(&coda_dir)?;
        let mut available = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }

            if name_str.ends_with(feature_slug) {
                let state_path = entry.path().join("state.yml");
                let content = fs::read_to_string(&state_path).map_err(|e| {
                    CoreError::StateError(format!("Cannot read {}: {e}", state_path.display()))
                })?;
                return serde_yaml::from_str(&content).map_err(|e| {
                    CoreError::StateError(format!(
                        "Invalid state.yml at {}: {e}",
                        state_path.display()
                    ))
                });
            }

            available.push(name_str.to_string());
        }

        let hint = if available.is_empty() {
            "No features have been planned yet.".to_string()
        } else {
            format!("Available features: {}", available.join(", "))
        };

        Err(CoreError::StateError(format!(
            "No feature found for slug '{feature_slug}'. {hint}"
        )))
    }

    /// Executes a feature development run through all phases.
    ///
    /// Reads `state.yml` and resumes from the last completed phase. Uses
    /// a single continuous `ClaudeClient` session with the Coder profile
    /// to execute setup → implement → test → review → verify → PR.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the runner cannot be created or any phase
    /// fails after all retries.
    pub async fn run(&self, feature_slug: &str) -> Result<Vec<TaskResult>, CoreError> {
        info!(feature_slug, "Starting feature run");
        let mut runner = crate::runner::Runner::new(
            feature_slug,
            self.project_root.clone(),
            &self.pm,
            &self.config,
        )?;
        runner.execute().await
    }
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
