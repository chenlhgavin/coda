//! Error types for Code Agent SDK.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Claude Code not found: {0}")]
    CliNotFound(String),

    #[error("Connection error: {0}")]
    Connection(#[from] std::io::Error),

    #[error("Not connected. Call connect() first.")]
    NotConnected,

    #[error("Process failed with exit code {exit_code}")]
    Process {
        exit_code: i32,
        stderr: Option<String>,
    },

    #[error("JSON decode error: {0}")]
    JsonDecode(#[from] serde_json::Error),

    #[error("Message parse error: {0}")]
    MessageParse(String),

    #[error("Control request timeout: {0}")]
    ControlTimeout(String),

    #[error("Feature '{feature}' is not supported by the {backend} backend")]
    UnsupportedFeature { feature: String, backend: String },

    #[error("Options not supported by {backend} backend: {}", options.join(", "))]
    UnsupportedOptions {
        backend: String,
        options: Vec<String>,
    },

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
