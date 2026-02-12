//! Prompt template data structure.
//!
//! Contains `PromptTemplate`, the core data type representing a named
//! minijinja template with its raw content.

use serde::{Deserialize, Serialize};

/// A named prompt template with its raw minijinja content.
///
/// Template names use `/`-separated paths (e.g., `"init/system"`) to
/// organize templates hierarchically by phase.
///
/// # Examples
///
/// ```
/// use coda_pm::PromptTemplate;
///
/// let tmpl = PromptTemplate::new("greeting", "Hello, {{ name }}!");
/// assert_eq!(tmpl.name, "greeting");
/// assert!(tmpl.content.contains("{{ name }}"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    /// Template identifier (e.g., `"init/system"`, `"run/setup"`).
    pub name: String,

    /// Raw minijinja template content.
    pub content: String,
}

impl PromptTemplate {
    /// Creates a new prompt template with the given name and content.
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            content: content.into(),
        }
    }
}
