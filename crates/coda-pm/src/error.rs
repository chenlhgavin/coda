//! Error types for the prompt manager.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PromptError {
    #[error("Template not found: {0}")]
    TemplateNotFound(String),

    #[error("Template render error: {0}")]
    RenderError(String),

    #[error("Invalid template: {0}")]
    InvalidTemplate(String),
}
