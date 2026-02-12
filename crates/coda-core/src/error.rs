//! Error types for the core engine.
//!
//! Defines `CoreError` as the primary error type for all operations
//! within `coda-core`.

use thiserror::Error;

/// Error type for coda-core operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// An error from the Claude Agent SDK or agent execution.
    #[error("Agent error: {0}")]
    AgentError(String),

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

    /// An error from the Claude Agent SDK.
    #[error("Agent SDK error: {0}")]
    AgentSdkError(String),

    /// A git operation error.
    #[error("Git error: {0}")]
    GitError(String),

    /// A YAML serialization/deserialization error.
    #[error("YAML error: {0}")]
    YamlError(#[from] serde_yaml::Error),

    /// A generic error from anyhow.
    #[error(transparent)]
    AnyhowError(#[from] anyhow::Error),
}
