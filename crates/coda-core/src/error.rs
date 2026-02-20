//! Error types for the core engine.
//!
//! Defines `CoreError` as the primary error type for all operations
//! within `coda-core`.

use thiserror::Error;

/// Error type for coda-core operations.
///
/// Variants are grouped by subsystem: agent, git/gh external CLI, planning
/// workflow, state management, I/O, serialization, and configuration.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// An error from the Claude Agent SDK or agent execution.
    ///
    /// Covers both SDK-level failures (connection, streaming) and
    /// logical agent errors (empty responses, unexpected phases).
    #[error("Agent error: {0}")]
    AgentError(String),

    /// The session budget has been exhausted.
    ///
    /// Emitted when `total_cost_usd` reaches or exceeds the configured
    /// `max_budget_usd`. The user should increase
    /// `agent.max_budget_usd` in `.coda/config.yml`.
    #[error(
        "Budget exhausted: spent ${spent:.2} of ${limit:.2} budget. \
         Increase `agent.max_budget_usd` in .coda/config.yml to allow more spending."
    )]
    BudgetExhausted {
        /// Amount spent so far in USD.
        spent: f64,
        /// Configured budget limit in USD.
        limit: f64,
    },

    /// An error from the prompt template manager.
    #[error("Prompt error: {0}")]
    PromptError(#[from] coda_pm::PromptError),

    /// An I/O error from file system operations.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// A configuration error (invalid or missing config).
    #[error("Config error: {0}")]
    ConfigError(String),

    /// A state file error (invalid or missing state).
    #[error("State error: {0}")]
    StateError(String),

    /// A planning workflow error (e.g. finalizing without approval).
    #[error("Plan error: {0}")]
    PlanError(String),

    /// A git/gh external CLI operation error.
    #[error("Git error: {0}")]
    GitError(String),

    /// A YAML serialization/deserialization error.
    #[error("YAML error: {0}")]
    YamlError(#[from] serde_yaml::Error),

    /// A generic error from anyhow.
    #[error(transparent)]
    AnyhowError(#[from] anyhow::Error),
}
