//! Configuration types for CODA projects.
//!
//! Defines `CodaConfig` which is loaded from `.coda/config.yml` in a
//! user's repository. All fields use snake_case to match YAML conventions.

use std::str::FromStr;

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

    /// Verification phase configuration.
    pub verify: VerifyConfig,
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

    /// Maximum seconds of silence during tool execution (e.g., `cargo build`,
    /// `cargo test`) before counting as one idle timeout. Tool executions
    /// can produce long legitimate silence periods, so this is typically
    /// longer than `idle_timeout_secs`.
    pub tool_execution_timeout_secs: u64,

    /// How many consecutive idle timeouts to tolerate before aborting.
    /// Each timeout triggers a full subprocess reconnection attempt.
    /// Total maximum silence ≈ `idle_timeout_secs * (idle_retries + 1)`
    /// (plus backoff delays between reconnection attempts).
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

    /// Whether to squash all feature branch commits into one before
    /// creating the PR. When enabled, CODA soft-resets to the merge-base
    /// and the agent creates a single commit with a concise message
    /// describing the core feature. After PR creation, CODA amends the
    /// commit with execution metadata and force-pushes with `--force-with-lease`.
    pub squash_before_push: bool,

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

/// Reasoning effort level for Codex CLI reviews.
///
/// Controls how much reasoning the model applies during review. Higher
/// levels produce more thorough reviews but cost more and are slower.
/// The value is passed through to the Codex CLI as
/// `-c model_reasoning_effort=<value>`.
///
/// Deserialization is case-insensitive and accepts legacy aliases:
/// `minimal` maps to `Low`, `xhigh` maps to `High`.
///
/// # Examples
///
/// ```
/// use coda_core::config::ReasoningEffort;
///
/// let effort: ReasoningEffort = "high".parse().unwrap();
/// assert_eq!(effort, ReasoningEffort::High);
/// assert_eq!(effort.to_string(), "high");
///
/// // Legacy aliases are accepted
/// let minimal: ReasoningEffort = "minimal".parse().unwrap();
/// assert_eq!(minimal, ReasoningEffort::Low);
///
/// // Round-trip through serde
/// let yaml = serde_yaml::to_string(&effort).unwrap();
/// let parsed: ReasoningEffort = serde_yaml::from_str(&yaml).unwrap();
/// assert_eq!(parsed, effort);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// Minimal reasoning — fastest, cheapest, least thorough.
    Low,
    /// Moderate reasoning — balanced between speed and thoroughness.
    Medium,
    /// Deep reasoning — most thorough, slowest, most expensive.
    High,
}

/// Custom serialization: always lowercase canonical names.
impl serde::Serialize for ReasoningEffort {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// Custom deserialization: case-insensitive with legacy alias support.
///
/// Accepts canonical values (`low`, `medium`, `high`) and legacy aliases
/// (`minimal` → `Low`, `xhigh` → `High`) regardless of case.
impl<'de> serde::Deserialize<'de> for ReasoningEffort {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<ReasoningEffort>()
            .map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" | "minimal" => Ok(Self::Low),
            "medium" | "moderate" => Ok(Self::Medium),
            "high" | "xhigh" => Ok(Self::High),
            other => Err(format!("unknown reasoning effort: {other}")),
        }
    }
}

/// Verification phase configuration.
///
/// # Examples
///
/// ```
/// use coda_core::config::VerifyConfig;
///
/// let config = VerifyConfig::default();
/// assert_eq!(config.max_verify_retries, 3);
/// assert!(!config.fail_on_max_attempts);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerifyConfig {
    /// When `true`, the verify phase returns an error if any check still
    /// fails after all retry attempts, causing the run to abort. When
    /// `false` (default), a warning is logged and the run continues with
    /// a draft PR.
    pub fail_on_max_attempts: bool,

    /// Maximum number of retry attempts after the initial verification
    /// fails. Total verification attempts = 1 (initial) + `max_verify_retries`.
    /// Defaults to 3 retries (4 total attempts).
    ///
    /// Backward-compatible with the legacy `max_retries` key via serde alias.
    #[serde(alias = "max_retries")]
    pub max_verify_retries: u32,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            fail_on_max_attempts: false,
            max_verify_retries: 3,
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
    pub codex_reasoning_effort: ReasoningEffort,
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
            verify: VerifyConfig::default(),
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
            tool_execution_timeout_secs: 600,
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
            squash_before_push: true,
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
            codex_reasoning_effort: ReasoningEffort::High,
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
        assert!(config.git.squash_before_push);
        assert!(config.review.enabled);
        assert_eq!(config.verify.max_verify_retries, 3);
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
  squash_before_push: false
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
        assert!(!config.git.squash_before_push);
        assert_eq!(config.git.branch_prefix, "dev");
        assert!(!config.review.enabled);
        assert_eq!(config.review.max_review_rounds, 10);
        assert_eq!(config.review.engine, ReviewEngine::Hybrid);
        assert_eq!(config.review.codex_model, "o4-mini");
        assert_eq!(
            config.review.codex_reasoning_effort,
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn test_should_deserialize_partial_config_with_defaults() {
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
        assert!((config.agent.max_budget_usd - 100.0).abs() < f64::EPSILON);
        assert_eq!(config.agent.max_retries, 3);
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
        assert!(config.git.squash_before_push);
        assert_eq!(config.review.engine, ReviewEngine::Codex);
        assert_eq!(config.review.codex_model, "gpt-5.3-codex");
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::High);
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

    #[test]
    fn test_should_deserialize_legacy_max_retries_as_max_verify_retries() {
        let yaml = r#"
version: 1
verify:
  fail_on_max_attempts: true
  max_retries: 5
"#;
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.verify.max_verify_retries, 5);
        assert!(config.verify.fail_on_max_attempts);
    }

    #[test]
    fn test_should_deserialize_new_max_verify_retries() {
        let yaml = r#"
version: 1
verify:
  max_verify_retries: 7
"#;
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.verify.max_verify_retries, 7);
    }

    #[test]
    fn test_should_default_max_verify_retries_to_three() {
        let config = CodaConfig::default();
        assert_eq!(config.verify.max_verify_retries, 3);
    }

    // ── ReasoningEffort ─────────────────────────────────────────────

    #[test]
    fn test_should_serialize_reasoning_effort_lowercase() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::High).unwrap(),
            "\"high\""
        );
    }

    #[test]
    fn test_should_deserialize_reasoning_effort_lowercase() {
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"low\"").unwrap(),
            ReasoningEffort::Low
        );
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"medium\"").unwrap(),
            ReasoningEffort::Medium
        );
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"high\"").unwrap(),
            ReasoningEffort::High
        );
    }

    #[test]
    fn test_should_round_trip_reasoning_effort_serde() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            let json = serde_json::to_string(&effort).unwrap();
            let parsed: ReasoningEffort = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, effort);
        }
    }

    #[test]
    fn test_should_display_reasoning_effort_lowercase() {
        assert_eq!(ReasoningEffort::Low.to_string(), "low");
        assert_eq!(ReasoningEffort::Medium.to_string(), "medium");
        assert_eq!(ReasoningEffort::High.to_string(), "high");
    }

    #[test]
    fn test_should_parse_reasoning_effort_from_str_case_insensitive() {
        assert_eq!(
            "Low".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Low
        );
        assert_eq!(
            "MEDIUM".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Medium
        );
        assert_eq!(
            "high".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::High
        );
    }

    #[test]
    fn test_should_reject_invalid_reasoning_effort() {
        assert!("unknown".parse::<ReasoningEffort>().is_err());
    }

    #[test]
    fn test_should_display_and_from_str_roundtrip_for_reasoning_effort() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            let displayed = effort.to_string();
            let parsed: ReasoningEffort = displayed.parse().unwrap();
            assert_eq!(parsed, effort);
        }
    }

    #[test]
    fn test_should_deserialize_all_reasoning_effort_variants_from_yaml() {
        for (yaml_val, expected) in [
            ("low", ReasoningEffort::Low),
            ("medium", ReasoningEffort::Medium),
            ("high", ReasoningEffort::High),
        ] {
            let yaml = format!("version: 1\nreview:\n  codex_reasoning_effort: {yaml_val}\n");
            let config: CodaConfig = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(config.review.codex_reasoning_effort, expected);
        }
    }

    #[test]
    fn test_should_deserialize_legacy_reasoning_effort_aliases() {
        // "minimal" should map to Low
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: minimal\n";
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::Low);

        // "xhigh" should map to High
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: xhigh\n";
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::High);

        // "moderate" should map to Medium
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: moderate\n";
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            config.review.codex_reasoning_effort,
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn test_should_deserialize_reasoning_effort_case_insensitive_from_yaml() {
        // Mixed-case "High" should work
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: High\n";
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::High);

        // UPPERCASE "LOW" should work
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: LOW\n";
        let config: CodaConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::Low);
    }
}
