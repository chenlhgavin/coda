//! Error types for the prompt manager.

use thiserror::Error;

/// Errors that can occur when managing prompt templates.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PromptError {
    /// The requested template was not found by name.
    #[error("Template not found: {0}")]
    TemplateNotFound(String),

    /// An error occurred while rendering a template.
    #[error("Template render error: {0}")]
    RenderError(String),

    /// The template content is invalid (e.g., syntax error).
    #[error("Invalid template: {0}")]
    InvalidTemplate(String),

    /// An I/O error occurred while reading template files.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
