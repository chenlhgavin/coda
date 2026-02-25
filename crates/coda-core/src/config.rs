//! Configuration types for CODA projects.
//!
//! Defines `CodaConfig` which is loaded from `.coda/config.yml` in a
//! user's repository. All fields use snake_case to match YAML conventions.
//!
//! ## Per-Operation Agent Configuration
//!
//! The `agents` section allows each operation (`init`, `plan`, `run`, `review`)
//! to independently configure which backend (claude/codex/cursor), model, and
//! effort level to use. Unspecified fields fall back to global defaults
//! (`agent.model`) or legacy fields (`review.engine`, `review.codex_model`).

use std::str::FromStr;

use coda_agent_sdk::backend::BackendKind;
use coda_agent_sdk::options::Effort;
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
/// let yaml = serde_yaml_ng::to_string(&config).unwrap();
/// let loaded: CodaConfig = serde_yaml_ng::from_str(&yaml).unwrap();
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

    /// Per-operation agent backend overrides.
    ///
    /// Each operation (`init`, `plan`, `run`, `review`) can independently
    /// specify a backend, model, and effort level. Unset fields inherit
    /// from global defaults (`agent.model`) or legacy review fields.
    pub agents: AgentsConfig,
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

    /// When `true` (default), quality phases (review, verify, update-docs)
    /// use isolated session IDs instead of the main dev session. This
    /// significantly reduces input token consumption because quality phases
    /// do not carry the accumulated dev conversation history. Their prompt
    /// templates are self-contained with all necessary context.
    ///
    /// Set to `false` to revert to the legacy behavior where all phases
    /// share a single conversation context.
    pub isolate_quality_phases: bool,

    /// Default reasoning effort level applied to all operations when no
    /// per-operation override is set via `agents.<op>.effort`. Controls
    /// how much reasoning the model applies — higher levels produce more
    /// thorough results but cost more.
    ///
    /// Defaults to `High` for a good balance of quality and cost.
    pub default_effort: ReasoningEffort,
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
/// Controls whether reviews are performed by Claude (self-review)
/// or Codex CLI (independent review).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEngine {
    /// Claude reviews its own code (self-review).
    Claude,
    /// Codex CLI performs an independent read-only review (default).
    #[default]
    Codex,
}

impl std::fmt::Display for ReviewEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude => write!(f, "claude"),
            Self::Codex => write!(f, "codex"),
        }
    }
}

/// Which agent CLI backend to use for an operation.
///
/// Controls which subprocess is spawned: Claude Code CLI, OpenAI Codex CLI,
/// or Cursor Agent CLI. Serializes as lowercase (`"claude"`, `"codex"`,
/// `"cursor"`) and parses case-insensitively.
///
/// # Examples
///
/// ```
/// use coda_core::config::AgentBackend;
///
/// let backend: AgentBackend = "codex".parse().unwrap();
/// assert_eq!(backend, AgentBackend::Codex);
/// assert_eq!(backend.to_string(), "codex");
///
/// // Case-insensitive
/// let backend: AgentBackend = "CLAUDE".parse().unwrap();
/// assert_eq!(backend, AgentBackend::Claude);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentBackend {
    /// Claude Code CLI (default).
    #[default]
    Claude,
    /// OpenAI Codex CLI.
    Codex,
    /// Cursor Agent CLI.
    Cursor,
}

impl AgentBackend {
    /// Converts to the SDK's [`BackendKind`].
    pub fn to_backend_kind(self) -> BackendKind {
        match self {
            Self::Claude => BackendKind::Claude,
            Self::Codex => BackendKind::Codex,
            Self::Cursor => BackendKind::Cursor,
        }
    }
}

impl std::fmt::Display for AgentBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude => write!(f, "claude"),
            Self::Codex => write!(f, "codex"),
            Self::Cursor => write!(f, "cursor"),
        }
    }
}

impl FromStr for AgentBackend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "cursor" => Ok(Self::Cursor),
            other => Err(format!("unknown agent backend: {other}")),
        }
    }
}

impl serde::Serialize for AgentBackend {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for AgentBackend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<AgentBackend>().map_err(serde::de::Error::custom)
    }
}

/// Reasoning effort level for agent operations.
///
/// Controls how much reasoning the model applies. Higher levels produce
/// more thorough results but cost more and are slower. The value is
/// passed through to the CLI backend (e.g., as `--effort` for Claude,
/// or `-c model_reasoning_effort=<value>` for Codex).
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
/// // Max variant (maps to SDK Effort::Max)
/// let max: ReasoningEffort = "max".parse().unwrap();
/// assert_eq!(max, ReasoningEffort::Max);
///
/// // Round-trip through serde
/// let yaml = serde_yaml_ng::to_string(&effort).unwrap();
/// let parsed: ReasoningEffort = serde_yaml_ng::from_str(&yaml).unwrap();
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
    /// Maximum reasoning — deepest possible reasoning, highest cost.
    Max,
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

impl ReasoningEffort {
    /// Converts to the SDK's [`Effort`] enum.
    pub fn to_sdk_effort(self) -> Effort {
        match self {
            Self::Low => Effort::Low,
            Self::Medium => Effort::Medium,
            Self::High => Effort::High,
            Self::Max => Effort::Max,
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Max => write!(f, "max"),
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
            "max" => Ok(Self::Max),
            other => Err(format!("unknown reasoning effort: {other}")),
        }
    }
}

// ── Config Schema ──────────────────────────────────────────────────

/// Well-known models for the Claude backend, shown as suggestions
/// in interactive config. Updated with each CODA release.
const CLAUDE_MODELS: &[&str] = &["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"];

/// Well-known models for the Codex backend.
const CODEX_MODELS: &[&str] = &["gpt-5.3-codex", "o4-mini"];

/// Well-known models for the Cursor backend.
const CURSOR_MODELS: &[&str] = &["claude-sonnet-4-6"];

/// Returns suggested model names for the given backend.
///
/// Falls back to Claude suggestions when the backend is `None`.
fn model_suggestions_for(backend: Option<AgentBackend>) -> Vec<String> {
    let models = match backend.unwrap_or_default() {
        AgentBackend::Claude => CLAUDE_MODELS,
        AgentBackend::Codex => CODEX_MODELS,
        AgentBackend::Cursor => CURSOR_MODELS,
    };
    models.iter().map(|s| (*s).to_string()).collect()
}

/// The type of value a config key accepts.
///
/// Used by interactive config UIs to decide which input widget to show
/// (dropdown, text input, boolean toggle, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValueType {
    /// Fixed set of string options (e.g., backend: claude|codex|cursor).
    Enum(Vec<String>),
    /// Suggested options + free-form input (e.g., model names per backend).
    Suggest(Vec<String>),
    /// Boolean: true or false.
    Bool,
    /// Unsigned 32-bit integer.
    U32,
    /// 64-bit floating point.
    F64,
    /// Free-form string.
    String,
}

/// Describes a single settable config key for interactive UIs.
///
/// Returned by [`CodaConfig::config_keys()`] and consumed by CLI
/// (dialoguer) and Slack (Block Kit) frontends to present interactive
/// config selection.
#[derive(Debug, Clone)]
pub struct ConfigKeyDescriptor {
    /// Dot-path key (e.g., `"agents.run.backend"`).
    pub key: String,
    /// Human-readable label (e.g., `"Run backend"`).
    pub label: String,
    /// What type of value this key accepts.
    pub value_type: ConfigValueType,
    /// Current effective value as a display string.
    pub current_value: String,
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
            isolate_quality_phases: true,
            default_effort: ReasoningEffort::High,
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

/// Per-operation agent configuration.
///
/// All fields are optional; unset fields inherit from global defaults
/// via the `resolve_*` methods on [`CodaConfig`].
///
/// # Examples
///
/// ```
/// use coda_core::config::{AgentBackend, OperationAgentConfig, ReasoningEffort};
///
/// let op = OperationAgentConfig {
///     backend: Some(AgentBackend::Codex),
///     model: Some("gpt-5.3-codex".to_string()),
///     effort: Some(ReasoningEffort::High),
/// };
/// assert_eq!(op.backend.unwrap(), AgentBackend::Codex);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OperationAgentConfig {
    /// Which backend CLI to use for this operation.
    pub backend: Option<AgentBackend>,
    /// Model identifier override for this operation.
    pub model: Option<String>,
    /// Reasoning effort override for this operation.
    pub effort: Option<ReasoningEffort>,
}

/// Container for per-operation agent overrides.
///
/// Each field corresponds to a CODA operation and holds optional
/// overrides. Missing fields default to empty [`OperationAgentConfig`].
///
/// # Examples
///
/// ```
/// use coda_core::config::AgentsConfig;
///
/// let agents = AgentsConfig::default();
/// assert!(agents.init.backend.is_none());
/// assert!(agents.review.model.is_none());
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentsConfig {
    /// Overrides for the `init` operation (analyze + setup phases).
    pub init: OperationAgentConfig,
    /// Overrides for the `plan` operation (interactive planning).
    pub plan: OperationAgentConfig,
    /// Overrides for the `run` operation (dev phases execution).
    pub run: OperationAgentConfig,
    /// Overrides for the `review` operation (code review phase).
    pub review: OperationAgentConfig,
}

/// Fully resolved agent configuration for a single operation.
///
/// All fields are populated — no `Option`s. Created by the `resolve_*`
/// methods on [`CodaConfig`] which merge per-operation overrides with
/// global defaults and legacy fields.
#[derive(Debug, Clone)]
pub struct ResolvedAgentConfig {
    /// Which backend CLI to use.
    pub backend: AgentBackend,
    /// Model identifier (e.g., `"claude-opus-4-6"` or `"gpt-5.3-codex"`).
    pub model: String,
    /// Reasoning effort level for this operation.
    pub effort: ReasoningEffort,
}

impl CodaConfig {
    /// Resolves agent configuration for the `init` operation.
    ///
    /// Falls back to `agent.model` for model, `Claude` for backend,
    /// and `agent.default_effort` for effort.
    pub fn resolve_init(&self) -> ResolvedAgentConfig {
        ResolvedAgentConfig {
            backend: self.agents.init.backend.unwrap_or_default(),
            model: self
                .agents
                .init
                .model
                .clone()
                .unwrap_or_else(|| self.agent.model.clone()),
            effort: self.agents.init.effort.unwrap_or(self.agent.default_effort),
        }
    }

    /// Resolves agent configuration for the `plan` operation.
    ///
    /// Falls back to `agent.model` for model, `Claude` for backend,
    /// and `agent.default_effort` for effort.
    pub fn resolve_plan(&self) -> ResolvedAgentConfig {
        ResolvedAgentConfig {
            backend: self.agents.plan.backend.unwrap_or_default(),
            model: self
                .agents
                .plan
                .model
                .clone()
                .unwrap_or_else(|| self.agent.model.clone()),
            effort: self.agents.plan.effort.unwrap_or(self.agent.default_effort),
        }
    }

    /// Resolves agent configuration for the `run` operation.
    ///
    /// Falls back to `agent.model` for model, `Claude` for backend,
    /// and `agent.default_effort` for effort.
    pub fn resolve_run(&self) -> ResolvedAgentConfig {
        ResolvedAgentConfig {
            backend: self.agents.run.backend.unwrap_or_default(),
            model: self
                .agents
                .run
                .model
                .clone()
                .unwrap_or_else(|| self.agent.model.clone()),
            effort: self.agents.run.effort.unwrap_or(self.agent.default_effort),
        }
    }

    /// Resolves agent configuration for the `review` operation.
    ///
    /// Maintains backward compatibility with legacy `review.engine`,
    /// `review.codex_model`, and `review.codex_reasoning_effort` fields:
    ///
    /// - **backend**: `agents.review.backend` → legacy `review.engine` mapping
    /// - **model**: `agents.review.model` → legacy `review.codex_model` (for codex)
    ///   or `agent.model` (for claude/cursor)
    /// - **effort**: `agents.review.effort` → legacy `review.codex_reasoning_effort`
    ///   (for codex) or `agent.default_effort` (for claude/cursor)
    pub fn resolve_review(&self) -> ResolvedAgentConfig {
        let backend = self
            .agents
            .review
            .backend
            .unwrap_or(match self.review.engine {
                ReviewEngine::Claude => AgentBackend::Claude,
                ReviewEngine::Codex => AgentBackend::Codex,
            });

        let model = self
            .agents
            .review
            .model
            .clone()
            .unwrap_or_else(|| match backend {
                AgentBackend::Codex => self.review.codex_model.clone(),
                _ => self.agent.model.clone(),
            });

        let effort = self.agents.review.effort.unwrap_or(match backend {
            AgentBackend::Codex => self.review.codex_reasoning_effort,
            _ => self.agent.default_effort,
        });

        ResolvedAgentConfig {
            backend,
            model,
            effort,
        }
    }

    /// Returns descriptors for all settable config keys.
    ///
    /// Each descriptor includes the dot-path key, a human-readable label,
    /// the expected value type (with valid options where applicable), and
    /// the current effective value. Used by interactive config UIs (CLI
    /// dialoguer prompts and Slack Block Kit dropdowns).
    ///
    /// For per-operation model fields, suggestions are derived from the
    /// operation's current backend. The current value shows `"(default)"`
    /// when no per-operation override is set.
    pub fn config_keys(&self) -> Vec<ConfigKeyDescriptor> {
        let backend_options = vec![
            "claude".to_string(),
            "codex".to_string(),
            "cursor".to_string(),
        ];
        let effort_options = vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "max".to_string(),
        ];

        let ops: &[(&str, &str, &OperationAgentConfig)] = &[
            ("init", "Init", &self.agents.init),
            ("plan", "Plan", &self.agents.plan),
            ("run", "Run", &self.agents.run),
            ("review", "Review", &self.agents.review),
        ];

        let mut keys = Vec::new();

        // Per-operation keys: agents.<op>.backend, .model, .effort
        for &(op, label, op_config) in ops {
            keys.push(ConfigKeyDescriptor {
                key: format!("agents.{op}.backend"),
                label: format!("{label} backend"),
                value_type: ConfigValueType::Enum(backend_options.clone()),
                current_value: op_config
                    .backend
                    .map_or("(default)".to_string(), |b| b.to_string()),
            });

            keys.push(ConfigKeyDescriptor {
                key: format!("agents.{op}.model"),
                label: format!("{label} model"),
                value_type: ConfigValueType::Suggest(model_suggestions_for(op_config.backend)),
                current_value: op_config
                    .model
                    .clone()
                    .unwrap_or_else(|| "(default)".to_string()),
            });

            keys.push(ConfigKeyDescriptor {
                key: format!("agents.{op}.effort"),
                label: format!("{label} effort"),
                value_type: ConfigValueType::Enum(effort_options.clone()),
                current_value: op_config
                    .effort
                    .map_or("(default)".to_string(), |e| e.to_string()),
            });
        }

        // Global agent keys
        keys.push(ConfigKeyDescriptor {
            key: "agent.model".to_string(),
            label: "Default model".to_string(),
            value_type: ConfigValueType::Suggest(model_suggestions_for(None)),
            current_value: self.agent.model.clone(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "agent.default_effort".to_string(),
            label: "Default effort".to_string(),
            value_type: ConfigValueType::Enum(effort_options.clone()),
            current_value: self.agent.default_effort.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "agent.max_budget_usd".to_string(),
            label: "Max budget (USD)".to_string(),
            value_type: ConfigValueType::F64,
            current_value: self.agent.max_budget_usd.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "agent.max_retries".to_string(),
            label: "Max retries".to_string(),
            value_type: ConfigValueType::U32,
            current_value: self.agent.max_retries.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "agent.max_turns".to_string(),
            label: "Max turns".to_string(),
            value_type: ConfigValueType::U32,
            current_value: self.agent.max_turns.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "agent.isolate_quality_phases".to_string(),
            label: "Isolate quality phases".to_string(),
            value_type: ConfigValueType::Bool,
            current_value: self.agent.isolate_quality_phases.to_string(),
        });

        // Git keys
        keys.push(ConfigKeyDescriptor {
            key: "git.auto_commit".to_string(),
            label: "Auto commit".to_string(),
            value_type: ConfigValueType::Bool,
            current_value: self.git.auto_commit.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "git.squash_before_push".to_string(),
            label: "Squash before push".to_string(),
            value_type: ConfigValueType::Bool,
            current_value: self.git.squash_before_push.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "git.branch_prefix".to_string(),
            label: "Branch prefix".to_string(),
            value_type: ConfigValueType::String,
            current_value: self.git.branch_prefix.clone(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "git.base_branch".to_string(),
            label: "Base branch".to_string(),
            value_type: ConfigValueType::String,
            current_value: self.git.base_branch.clone(),
        });

        // Review keys
        keys.push(ConfigKeyDescriptor {
            key: "review.enabled".to_string(),
            label: "Review enabled".to_string(),
            value_type: ConfigValueType::Bool,
            current_value: self.review.enabled.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "review.engine".to_string(),
            label: "Review engine".to_string(),
            value_type: ConfigValueType::Enum(vec!["claude".to_string(), "codex".to_string()]),
            current_value: self.review.engine.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "review.max_review_rounds".to_string(),
            label: "Max review rounds".to_string(),
            value_type: ConfigValueType::U32,
            current_value: self.review.max_review_rounds.to_string(),
        });

        // Verify keys
        keys.push(ConfigKeyDescriptor {
            key: "verify.fail_on_max_attempts".to_string(),
            label: "Fail on max attempts".to_string(),
            value_type: ConfigValueType::Bool,
            current_value: self.verify.fail_on_max_attempts.to_string(),
        });
        keys.push(ConfigKeyDescriptor {
            key: "verify.max_verify_retries".to_string(),
            label: "Max verify retries".to_string(),
            value_type: ConfigValueType::U32,
            current_value: self.verify.max_verify_retries.to_string(),
        });

        keys
    }
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
            agents: AgentsConfig::default(),
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
        let yaml = serde_yaml_ng::to_string(&config).unwrap();
        let deserialized: CodaConfig = serde_yaml_ng::from_str(&yaml).unwrap();
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
  engine: codex
  codex_model: "o4-mini"
  codex_reasoning_effort: "medium"
"#;

        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
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
        assert_eq!(config.review.engine, ReviewEngine::Codex);
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

        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.version, 1);
        assert!((config.agent.max_budget_usd - 100.0).abs() < f64::EPSILON);
        assert_eq!(config.agent.max_retries, 3);
        assert!(!config.checks.is_empty());
        assert!(config.review.enabled);
    }

    #[test]
    fn test_should_deserialize_minimal_config() {
        let yaml = "version: 1\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
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
        ] {
            let yaml = format!("version: 1\nreview:\n  engine: {yaml_val}\n");
            let config: CodaConfig = serde_yaml_ng::from_str(&yaml).unwrap();
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
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
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
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
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
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::Max).unwrap(),
            "\"max\""
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
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"max\"").unwrap(),
            ReasoningEffort::Max
        );
    }

    #[test]
    fn test_should_round_trip_reasoning_effort_serde() {
        for effort in [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
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
        assert_eq!(ReasoningEffort::Max.to_string(), "max");
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
        assert_eq!(
            "MAX".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::Max
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
            ReasoningEffort::Max,
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
            ("max", ReasoningEffort::Max),
        ] {
            let yaml = format!("version: 1\nreview:\n  codex_reasoning_effort: {yaml_val}\n");
            let config: CodaConfig = serde_yaml_ng::from_str(&yaml).unwrap();
            assert_eq!(config.review.codex_reasoning_effort, expected);
        }
    }

    #[test]
    fn test_should_deserialize_legacy_reasoning_effort_aliases() {
        // "minimal" should map to Low
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: minimal\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::Low);

        // "xhigh" should map to High
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: xhigh\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::High);

        // "moderate" should map to Medium
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: moderate\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(
            config.review.codex_reasoning_effort,
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn test_should_deserialize_reasoning_effort_case_insensitive_from_yaml() {
        // Mixed-case "High" should work
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: High\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::High);

        // UPPERCASE "LOW" should work
        let yaml = "version: 1\nreview:\n  codex_reasoning_effort: LOW\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.review.codex_reasoning_effort, ReasoningEffort::Low);
    }

    // ── AgentBackend ──────────────────────────────────────────────────

    #[test]
    fn test_should_round_trip_agent_backend_serde() {
        for backend in [
            AgentBackend::Claude,
            AgentBackend::Codex,
            AgentBackend::Cursor,
        ] {
            let json = serde_json::to_string(&backend).unwrap();
            let parsed: AgentBackend = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, backend);
        }
    }

    #[test]
    fn test_should_parse_agent_backend_case_insensitive() {
        assert_eq!(
            "Claude".parse::<AgentBackend>().unwrap(),
            AgentBackend::Claude
        );
        assert_eq!(
            "CODEX".parse::<AgentBackend>().unwrap(),
            AgentBackend::Codex
        );
        assert_eq!(
            "cursor".parse::<AgentBackend>().unwrap(),
            AgentBackend::Cursor
        );
    }

    #[test]
    fn test_should_reject_invalid_agent_backend() {
        assert!("unknown".parse::<AgentBackend>().is_err());
    }

    #[test]
    fn test_should_display_agent_backend_lowercase() {
        assert_eq!(AgentBackend::Claude.to_string(), "claude");
        assert_eq!(AgentBackend::Codex.to_string(), "codex");
        assert_eq!(AgentBackend::Cursor.to_string(), "cursor");
    }

    #[test]
    fn test_should_default_agent_backend_to_claude() {
        assert_eq!(AgentBackend::default(), AgentBackend::Claude);
    }

    // ── AgentsConfig ──────────────────────────────────────────────────

    #[test]
    fn test_should_deserialize_config_with_agents_section() {
        let yaml = r#"
version: 1
agents:
  init:
    backend: claude
    model: claude-opus-4-6
    effort: high
  plan:
    backend: claude
    model: claude-sonnet-4-6
    effort: medium
  run:
    backend: claude
    model: claude-opus-4-6
  review:
    backend: codex
    model: gpt-5.3-codex
    effort: high
"#;
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.agents.init.backend, Some(AgentBackend::Claude));
        assert_eq!(config.agents.init.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(config.agents.init.effort, Some(ReasoningEffort::High));
        assert_eq!(
            config.agents.plan.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
        assert_eq!(config.agents.plan.effort, Some(ReasoningEffort::Medium));
        assert_eq!(config.agents.run.backend, Some(AgentBackend::Claude));
        assert!(config.agents.run.effort.is_none());
        assert_eq!(config.agents.review.backend, Some(AgentBackend::Codex));
    }

    #[test]
    fn test_should_deserialize_config_without_agents_section() {
        let yaml = "version: 1\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(config.agents.init.backend.is_none());
        assert!(config.agents.plan.backend.is_none());
        assert!(config.agents.run.backend.is_none());
        assert!(config.agents.review.backend.is_none());
    }

    #[test]
    fn test_should_round_trip_agents_config_yaml() {
        let yaml = r#"
version: 1
agents:
  init:
    backend: claude
    model: claude-opus-4-6
  review:
    backend: codex
    model: gpt-5.3-codex
    effort: max
"#;
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        let round_tripped = serde_yaml_ng::to_string(&config).unwrap();
        let reloaded: CodaConfig = serde_yaml_ng::from_str(&round_tripped).unwrap();
        assert_eq!(reloaded.agents.init.backend, Some(AgentBackend::Claude));
        assert_eq!(reloaded.agents.review.effort, Some(ReasoningEffort::Max));
    }

    // ── resolve_* methods ─────────────────────────────────────────────

    #[test]
    fn test_should_resolve_init_with_defaults() {
        let config = CodaConfig::default();
        let resolved = config.resolve_init();
        assert_eq!(resolved.backend, AgentBackend::Claude);
        assert_eq!(resolved.model, "claude-opus-4-6");
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_init_with_explicit_values() {
        let mut config = CodaConfig::default();
        config.agents.init.backend = Some(AgentBackend::Codex);
        config.agents.init.model = Some("gpt-5.3-codex".to_string());
        config.agents.init.effort = Some(ReasoningEffort::Max);
        let resolved = config.resolve_init();
        assert_eq!(resolved.backend, AgentBackend::Codex);
        assert_eq!(resolved.model, "gpt-5.3-codex");
        assert_eq!(resolved.effort, ReasoningEffort::Max);
    }

    #[test]
    fn test_should_resolve_plan_falls_back_to_agent_model() {
        let mut config = CodaConfig::default();
        config.agent.model = "claude-sonnet-4-6".to_string();
        let resolved = config.resolve_plan();
        assert_eq!(resolved.backend, AgentBackend::Claude);
        assert_eq!(resolved.model, "claude-sonnet-4-6");
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_run_with_effort() {
        let mut config = CodaConfig::default();
        config.agents.run.effort = Some(ReasoningEffort::High);
        let resolved = config.resolve_run();
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_review_legacy_codex_defaults() {
        let config = CodaConfig::default();
        let resolved = config.resolve_review();
        // Default review.engine is Codex
        assert_eq!(resolved.backend, AgentBackend::Codex);
        assert_eq!(resolved.model, "gpt-5.3-codex");
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_review_legacy_claude_engine() {
        let mut config = CodaConfig::default();
        config.review.engine = ReviewEngine::Claude;
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Claude);
        assert_eq!(resolved.model, "claude-opus-4-6");
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_review_explicit_overrides_legacy() {
        let mut config = CodaConfig::default();
        config.agents.review.backend = Some(AgentBackend::Claude);
        config.agents.review.model = Some("claude-sonnet-4-6".to_string());
        config.agents.review.effort = Some(ReasoningEffort::Medium);
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Claude);
        assert_eq!(resolved.model, "claude-sonnet-4-6");
        assert_eq!(resolved.effort, ReasoningEffort::Medium);
    }

    #[test]
    fn test_should_resolve_review_codex_with_custom_model() {
        let mut config = CodaConfig::default();
        config.review.codex_model = "o4-mini".to_string();
        config.review.codex_reasoning_effort = ReasoningEffort::Low;
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Codex);
        assert_eq!(resolved.model, "o4-mini");
        assert_eq!(resolved.effort, ReasoningEffort::Low);
    }

    #[test]
    fn test_should_resolve_review_agents_override_partial() {
        let mut config = CodaConfig::default();
        // Set only backend in agents.review, rest should fall back
        config.agents.review.backend = Some(AgentBackend::Codex);
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Codex);
        // Model falls back to review.codex_model since backend is Codex
        assert_eq!(resolved.model, "gpt-5.3-codex");
        // Effort falls back to review.codex_reasoning_effort since backend is Codex
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_review_cursor_fallback() {
        let mut config = CodaConfig::default();
        config.agents.review.backend = Some(AgentBackend::Cursor);
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Cursor);
        // Cursor falls back to agent.model
        assert_eq!(resolved.model, "claude-opus-4-6");
        // Cursor falls back to agent.default_effort
        assert_eq!(resolved.effort, ReasoningEffort::High);
    }

    #[test]
    fn test_should_resolve_init_with_custom_default_effort() {
        let mut config = CodaConfig::default();
        config.agent.default_effort = ReasoningEffort::Medium;
        let resolved = config.resolve_init();
        assert_eq!(resolved.effort, ReasoningEffort::Medium);
    }

    #[test]
    fn test_should_resolve_run_per_op_effort_overrides_default() {
        let mut config = CodaConfig::default();
        config.agent.default_effort = ReasoningEffort::Low;
        config.agents.run.effort = Some(ReasoningEffort::Max);
        let resolved = config.resolve_run();
        assert_eq!(resolved.effort, ReasoningEffort::Max);
    }

    #[test]
    fn test_should_deserialize_default_effort_from_yaml() {
        let yaml = "version: 1\nagent:\n  default_effort: medium\n";
        let config: CodaConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.agent.default_effort, ReasoningEffort::Medium);
    }

    // ── Config Schema ──────────────────────────────────────────────────

    #[test]
    fn test_should_return_config_keys_for_default_config() {
        let config = CodaConfig::default();
        let keys = config.config_keys();
        // 4 ops * 3 keys + 6 agent + 4 git + 3 review + 2 verify = 27
        assert_eq!(keys.len(), 27);
    }

    #[test]
    fn test_should_return_enum_type_for_backend_keys() {
        let config = CodaConfig::default();
        let keys = config.config_keys();
        let backend_key = keys
            .iter()
            .find(|k| k.key == "agents.run.backend")
            .expect("agents.run.backend key should exist");
        assert!(matches!(&backend_key.value_type, ConfigValueType::Enum(opts) if opts.len() == 3));
        assert_eq!(backend_key.current_value, "(default)");
    }

    #[test]
    fn test_should_return_suggest_type_for_model_keys() {
        let config = CodaConfig::default();
        let keys = config.config_keys();
        let model_key = keys
            .iter()
            .find(|k| k.key == "agents.run.model")
            .expect("agents.run.model key should exist");
        assert!(matches!(&model_key.value_type, ConfigValueType::Suggest(_)));
        assert_eq!(model_key.current_value, "(default)");
    }

    #[test]
    fn test_should_suggest_codex_models_when_backend_is_codex() {
        let mut config = CodaConfig::default();
        config.agents.run.backend = Some(AgentBackend::Codex);
        let keys = config.config_keys();
        let model_key = keys
            .iter()
            .find(|k| k.key == "agents.run.model")
            .expect("agents.run.model key should exist");
        match &model_key.value_type {
            ConfigValueType::Suggest(suggestions) => {
                assert!(suggestions.contains(&"gpt-5.3-codex".to_string()));
                assert!(suggestions.contains(&"o4-mini".to_string()));
            }
            other => panic!("Expected Suggest, got {other:?}"),
        }
    }

    #[test]
    fn test_should_return_model_suggestions_for_each_backend() {
        let claude = model_suggestions_for(Some(AgentBackend::Claude));
        assert!(claude.contains(&"claude-opus-4-6".to_string()));

        let codex = model_suggestions_for(Some(AgentBackend::Codex));
        assert!(codex.contains(&"gpt-5.3-codex".to_string()));

        let cursor = model_suggestions_for(Some(AgentBackend::Cursor));
        assert!(cursor.contains(&"claude-sonnet-4-6".to_string()));

        // None defaults to Claude
        let default = model_suggestions_for(None);
        assert_eq!(default, claude);
    }

    #[test]
    fn test_should_show_current_values_for_set_overrides() {
        let mut config = CodaConfig::default();
        config.agents.init.backend = Some(AgentBackend::Codex);
        config.agents.init.model = Some("gpt-5.3-codex".to_string());
        config.agents.init.effort = Some(ReasoningEffort::Max);
        let keys = config.config_keys();

        let backend = keys
            .iter()
            .find(|k| k.key == "agents.init.backend")
            .unwrap();
        assert_eq!(backend.current_value, "codex");

        let model = keys.iter().find(|k| k.key == "agents.init.model").unwrap();
        assert_eq!(model.current_value, "gpt-5.3-codex");

        let effort = keys.iter().find(|k| k.key == "agents.init.effort").unwrap();
        assert_eq!(effort.current_value, "max");
    }

    #[test]
    fn test_should_return_bool_type_for_boolean_keys() {
        let config = CodaConfig::default();
        let keys = config.config_keys();
        let auto_commit = keys
            .iter()
            .find(|k| k.key == "git.auto_commit")
            .expect("git.auto_commit key should exist");
        assert_eq!(auto_commit.value_type, ConfigValueType::Bool);
        assert_eq!(auto_commit.current_value, "true");
    }

    #[test]
    fn test_should_return_f64_type_for_budget_key() {
        let config = CodaConfig::default();
        let keys = config.config_keys();
        let budget = keys
            .iter()
            .find(|k| k.key == "agent.max_budget_usd")
            .expect("agent.max_budget_usd key should exist");
        assert_eq!(budget.value_type, ConfigValueType::F64);
    }

    #[test]
    fn test_should_return_u32_type_for_retry_keys() {
        let config = CodaConfig::default();
        let keys = config.config_keys();
        let retries = keys
            .iter()
            .find(|k| k.key == "agent.max_retries")
            .expect("agent.max_retries key should exist");
        assert_eq!(retries.value_type, ConfigValueType::U32);
        assert_eq!(retries.current_value, "3");
    }
}
