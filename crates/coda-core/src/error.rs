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

    /// The agent was idle for too long with no response, even after
    /// reconnection attempts.
    ///
    /// Distinct from [`AgentError`](Self::AgentError) so callers can
    /// match on this variant for retry logic at the phase level.
    #[error(
        "Agent idle for {total_idle_secs}s with no response ({retries_exhausted} retries \
         exhausted) — possible API issue. Check network and API status. \
         Adjust `agent.idle_timeout_secs` / `agent.idle_retries` in config."
    )]
    IdleTimeout {
        /// Total seconds of silence before giving up.
        total_idle_secs: u64,
        /// Number of reconnection retries that were exhausted.
        retries_exhausted: u32,
    },

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

    /// The operation was cancelled by the caller.
    ///
    /// Emitted when a [`CancellationToken`](tokio_util::sync::CancellationToken)
    /// is triggered, typically by the user pressing Ctrl+C or an abort signal
    /// from the server. The current operation is cleanly abandoned; callers
    /// should persist any in-progress state before propagating this error.
    #[error("Operation cancelled")]
    Cancelled,

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
