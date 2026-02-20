//! Error types for the coda-server application.
//!
//! Defines [`ServerError`] as the primary error type for all operations
//! within `coda-server`. Uses `thiserror` for ergonomic error definitions
//! following the project convention.

use thiserror::Error;

/// Error type for coda-server operations.
///
/// Variants are grouped by subsystem: configuration, Slack API communication,
/// WebSocket transport, envelope dispatch, I/O, and serialization.
///
/// # Examples
///
/// ```
/// use coda_server::error::ServerError;
///
/// let err = ServerError::Config("missing app_token".into());
/// assert!(err.to_string().contains("missing app_token"));
/// ```
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServerError {
    /// A configuration error (missing or invalid config file/values).
    #[error("Config error: {0}")]
    Config(String),

    /// An error from a Slack Web API call.
    #[error("Slack API error: {0}")]
    SlackApi(String),

    /// A WebSocket transport error (connection, read, write).
    #[error("WebSocket error: {0}")]
    WebSocket(String),

    /// An error while dispatching or routing an envelope.
    #[error("Dispatch error: {0}")]
    Dispatch(String),

    /// An I/O error from file system operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A YAML serialization/deserialization error.
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// An error from `coda-core`.
    #[error(transparent)]
    Core(#[from] coda_core::CoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_display_config_error() {
        let err = ServerError::Config("token missing".into());
        assert_eq!(err.to_string(), "Config error: token missing");
    }

    #[test]
    fn test_should_display_slack_api_error() {
        let err = ServerError::SlackApi("invalid_auth".into());
        assert_eq!(err.to_string(), "Slack API error: invalid_auth");
    }

    #[test]
    fn test_should_display_websocket_error() {
        let err = ServerError::WebSocket("connection refused".into());
        assert_eq!(err.to_string(), "WebSocket error: connection refused");
    }

    #[test]
    fn test_should_convert_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err: ServerError = io_err.into();
        assert!(matches!(err, ServerError::Io(_)));
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_should_convert_from_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err: ServerError = json_err.into();
        assert!(matches!(err, ServerError::Json(_)));
    }
}
