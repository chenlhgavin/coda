//! Prompt manager implementation.
//!
//! Manages a collection of minijinja templates that can be loaded from
//! directories, registered manually, and rendered with context data.

use std::collections::HashMap;
use std::path::Path;

use minijinja::Environment;
use serde::Serialize;

use crate::loader::load_templates_from_dir;
use crate::{PromptError, PromptTemplate};

/// Manages prompt templates using minijinja for rendering.
///
/// Templates can be added individually via [`add_template`](Self::add_template)
/// or loaded in bulk from a directory via [`load_from_dir`](Self::load_from_dir).
#[derive(Clone)]
pub struct PromptManager {
    env: Environment<'static>,
    templates: HashMap<String, PromptTemplate>,
}

impl PromptManager {
    /// Creates a new empty prompt manager with no templates loaded.
    ///
    /// Use [`add_template`](Self::add_template) or [`load_from_dir`](Self::load_from_dir)
    /// to populate with templates.
    pub fn new() -> Self {
        Self {
            env: Environment::new(),
            templates: HashMap::new(),
        }
    }

    /// Registers a single template with the manager.
    ///
    /// # Errors
    ///
    /// Returns `PromptError::InvalidTemplate` if the template content has
    /// invalid minijinja syntax.
    pub fn add_template(&mut self, template: PromptTemplate) -> Result<(), PromptError> {
        self.env
            .add_template_owned(template.name.clone(), template.content.clone())
            .map_err(|e: minijinja::Error| PromptError::InvalidTemplate(e.to_string()))?;
        self.templates.insert(template.name.clone(), template);
        Ok(())
    }

    /// Loads all `.j2` templates from a directory recursively.
    ///
    /// Template names are derived from relative paths (e.g., `init/system`
    /// from `init/system.j2`). Existing templates with the same name are
    /// overwritten.
    ///
    /// # Errors
    ///
    /// Returns `PromptError::IoError` if the directory cannot be read, or
    /// `PromptError::InvalidTemplate` if a template has invalid syntax.
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<(), PromptError> {
        let templates = load_templates_from_dir(dir)?;
        for template in templates {
            self.add_template(template)?;
        }
        Ok(())
    }

    /// Renders a named template with the given context data.
    ///
    /// # Errors
    ///
    /// Returns `PromptError::TemplateNotFound` if no template with the given
    /// name exists, or `PromptError::RenderError` if rendering fails.
    pub fn render<T: Serialize>(&self, name: &str, ctx: T) -> Result<String, PromptError> {
        let tmpl = self
            .env
            .get_template(name)
            .map_err(|_| PromptError::TemplateNotFound(name.to_string()))?;

        tmpl.render(ctx)
            .map_err(|e| PromptError::RenderError(e.to_string()))
    }

    /// Returns a reference to the template with the given name, if it exists.
    pub fn get_template(&self, name: &str) -> Option<&PromptTemplate> {
        self.templates.get(name)
    }
}

impl Default for PromptManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PromptManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptManager")
            .field("template_count", &self.templates.len())
            .field("template_names", &self.templates.keys().collect::<Vec<_>>())
            .finish()
    }
}
