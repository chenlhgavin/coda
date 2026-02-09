//! Error types for the core engine.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Agent error: {0}")]
    AgentError(String),

    #[error("Prompt error: {0}")]
    PromptError(#[from] coda_pm::PromptError),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
