//! Configuration types for CODA projects.
//!
//! Defines `CodaConfig` which is loaded from `.coda/config.yml` in a
//! user's repository. All fields use snake_case to match YAML conventions.

use serde::{Deserialize, Serialize};

/// Top-level CODA project configuration loaded from `.coda/config.yml`.
///
/// # Examples
///
/// ```
/// use coda_core::CodaConfig;
///
/// // Create with defaults
/// let config = CodaConfig::default();
/// assert_eq!(config.version, 1);
/// assert_eq!(config.agent.max_retries, 3);
///
/// // Deserialize from YAML
/// let yaml = serde_yaml::to_string(&config).unwrap();
/// let loaded: CodaConfig = serde_yaml::from_str(&yaml).unwrap();
/// assert_eq!(loaded.version, config.version);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CodaConfig {
    /// Configuration schema version.
    pub version: u32,

    /// Agent behavior configuration.
    pub agent: AgentConfig,

    /// Precommit check commands run after each phase.
    pub checks: Vec<String>,

    /// Prompt template configuration.
    pub prompts: PromptsConfig,

    /// Git workflow configuration.
    pub git: GitConfig,

    /// Code review configuration.
    pub review: ReviewConfig,
}

/// Agent configuration controlling model and budget limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Model identifier to use (e.g., `"claude-opus-4-6"`).
    pub model: String,

    /// Maximum budget in USD for a single `coda run` invocation.
    pub max_budget_usd: f64,

    /// Maximum retry attempts for a single phase on failure.
    pub max_retries: u32,

    /// Maximum agent conversation turns per phase.
    pub max_turns: u32,

    /// Maximum seconds of silence (no messages from CLI) before counting
    /// as one idle timeout. Prevents indefinite hangs when the API is
    /// unresponsive.
    pub idle_timeout_secs: u64,

    /// How many consecutive idle timeouts to tolerate before aborting.
    /// Total maximum silence = `idle_timeout_secs * (idle_retries + 1)`.
    pub idle_retries: u32,
}

/// Configuration for prompt template directories.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptsConfig {
    /// Additional directories to load custom prompt templates from.
    /// Templates in these directories override built-in templates with
    /// the same name.
    pub extra_dirs: Vec<String>,
}

/// Git workflow configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    /// Whether to automatically commit after each phase completes.
    pub auto_commit: bool,

    /// Prefix for feature branch names (e.g., `"feature"` produces
    /// `feature/<slug>`).
    pub branch_prefix: String,

    /// Base branch to fork feature worktrees from (e.g., `"main"` or
    /// `"master"`). When set to `"auto"`, the actual default branch is
    /// detected at runtime via `git symbolic-ref`.
    pub base_branch: String,
}

/// Which review engine to use for code review.
///
/// Controls whether reviews are performed by Claude (self-review),
/// Codex CLI (independent review), or both (hybrid).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEngine {
    /// Claude reviews its own code (self-review).
    Claude,
    /// Codex CLI performs an independent read-only review (default).
    #[default]
    Codex,
    /// Both Codex and Claude review; issues are merged and deduplicated.
    Hybrid,
}

impl std::fmt::Display for ReviewEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude => write!(f, "claude"),
            Self::Codex => write!(f, "codex"),
            Self::Hybrid => write!(f, "hybrid"),
        }
    }
}

/// Code review configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    /// Whether code review is enabled.
    pub enabled: bool,

    /// Maximum number of review rounds to prevent infinite loops.
    pub max_review_rounds: u32,

    /// Which review engine to use.
    pub engine: ReviewEngine,

    /// Model to use for Codex CLI reviews (e.g., `"gpt-5.3-codex"`).
    pub codex_model: String,

    /// Reasoning effort level for Codex CLI reviews.
    ///
    /// Valid values: `"minimal"`, `"low"`, `"medium"`, `"high"`, `"xhigh"`.
    pub codex_reasoning_effort: String,
}

impl Default for CodaConfig {
    fn default() -> Self {
        Self {
            version: 1,
            agent: AgentConfig::default(),
            checks: vec![
                "cargo build".to_string(),
                "cargo +nightly fmt -- --check".to_string(),
                "cargo clippy -- -D warnings".to_string(),
            ],
            prompts: PromptsConfig::default(),
            git: GitConfig::default(),
            review: ReviewConfig::default(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "claude-opus-4-6".to_string(),
            max_budget_usd: 100.0,
            max_retries: 3,
            max_turns: 100,
            idle_timeout_secs: 300,
            idle_retries: 2,
        }
    }
}

impl Default for PromptsConfig {
    fn default() -> Self {
        Self {
            extra_dirs: vec![".coda/prompts".to_string()],
        }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: true,
            branch_prefix: "feature".to_string(),
            base_branch: "auto".to_string(),
        }
    }
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_review_rounds: 5,
            engine: ReviewEngine::default(),
            codex_model: "gpt-5.3-codex".to_string(),
            codex_reasoning_effort: "high".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_create_default_config() {
        let config = CodaConfig::default();
        assert_eq!(config.version, 1);
        assert_eq!(config.agent.max_budget_usd, 100.0);
        assert_eq!(config.agent.max_retries, 3);
        assert_eq!(config.checks.len(), 3);
        assert!(config.git.auto_commit);
        assert!(config.review.enabled);
    }

    #[test]
    fn test_should_round_trip_yaml_serialization() {
        let config = CodaConfig::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let deserialized: CodaConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.version, config.version);
        assert_eq!(deserialized.agent.model, config.agent.model);
        assert_eq!(deserialized.git.branch_prefix, config.git.branch_prefix);
    }

    #[test]
    fn test_should_deserialize_custom_config() {
        let yaml = r#"
version: 2
agent:
  model: "claude-opus-4-20250514"
  max_budget_usd: 100.0
  max_retries: 5
checks:
  - "npm run build"
  - "npm run lint"
prompts:
  extra_dirs:
    - ".coda/custom-prompts"
    - ".prompts"
git:
  auto_commit: false
  branch_prefix: "dev"
review:
  enabled: false
  max_review_rounds: 10
  engine: hybrid
  codex_model: "o4-mini"
  codex_reasoning_effort: "medium"
"#;

        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.version, 2);
        assert_eq!(config.agent.model, "claude-opus-4-20250514");
        assert!((config.agent.max_budget_usd - 100.0).abs() < f64::EPSILON);
        assert_eq!(config.agent.max_retries, 5);
        assert_eq!(config.checks.len(), 2);
        assert_eq!(config.checks[0], "npm run build");
        assert_eq!(config.prompts.extra_dirs.len(), 2);
        assert!(!config.git.auto_commit);
        assert_eq!(config.git.branch_prefix, "dev");
        assert!(!config.review.enabled);
        assert_eq!(config.review.max_review_rounds, 10);
        assert_eq!(config.review.engine, ReviewEngine::Hybrid);
        assert_eq!(config.review.codex_model, "o4-mini");
        assert_eq!(config.review.codex_reasoning_effort, "medium");
    }

    #[test]
    fn test_should_deserialize_partial_config_with_defaults() {
        // Simulates a config.yml generated by the agent with missing/extra fields
        let yaml = r#"
version: 1
agent:
  model: "claude-sonnet-4-20250514"
  permission_mode: "auto"
  max_retries: 3
prompts:
  extra_dirs: []
git:
  auto_commit: true
  branch_prefix: "feature"
  commit_prefix: "feat"
review:
  enabled: true
  checks:
    - "cargo build"
  max_review_rounds: 5
"#;

        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.version, 1);
        // max_budget_usd missing → should use default 100.0
        assert!((config.agent.max_budget_usd - 100.0).abs() < f64::EPSILON);
        assert_eq!(config.agent.max_retries, 3);
        // top-level checks missing → should use default
        assert!(!config.checks.is_empty());
        assert!(config.review.enabled);
    }

    #[test]
    fn test_should_deserialize_minimal_config() {
        let yaml = "version: 1\n";
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.version, 1);
        assert_eq!(config.agent.model, "claude-opus-4-6");
        assert!((config.agent.max_budget_usd - 100.0).abs() < f64::EPSILON);
        assert!(config.git.auto_commit);
        // Review engine defaults
        assert_eq!(config.review.engine, ReviewEngine::Codex);
        assert_eq!(config.review.codex_model, "gpt-5.3-codex");
        assert_eq!(config.review.codex_reasoning_effort, "high");
    }

    #[test]
    fn test_should_deserialize_all_review_engine_variants() {
        for (yaml_val, expected) in [
            ("claude", ReviewEngine::Claude),
            ("codex", ReviewEngine::Codex),
            ("hybrid", ReviewEngine::Hybrid),
        ] {
            let yaml = format!("version: 1\nreview:\n  engine: {yaml_val}\n");
            let config: CodaConfig = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(config.review.engine, expected);
        }
    }
}
