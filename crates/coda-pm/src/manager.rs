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

    /// Creates a new prompt manager pre-loaded with all built-in templates.
    ///
    /// Built-in templates are embedded at compile time via [`include_str!`] and
    /// are always available, even when installed via `cargo install`.
    ///
    /// # Errors
    ///
    /// Returns `PromptError::InvalidTemplate` if any built-in template has
    /// invalid minijinja syntax.
    ///
    /// # Examples
    ///
    /// ```
    /// use coda_pm::PromptManager;
    ///
    /// let pm = PromptManager::with_builtin_templates().unwrap();
    /// assert!(pm.get_template("init/system").is_some());
    /// ```
    pub fn with_builtin_templates() -> Result<Self, PromptError> {
        let mut pm = Self::new();
        for template in crate::builtin::builtin_templates() {
            pm.add_template(template)?;
        }
        Ok(pm)
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

    /// Returns the number of templates currently loaded.
    ///
    /// # Examples
    ///
    /// ```
    /// use coda_pm::{PromptManager, PromptTemplate};
    ///
    /// let mut pm = PromptManager::new();
    /// assert_eq!(pm.template_count(), 0);
    /// pm.add_template(PromptTemplate::new("test", "Hello")).unwrap();
    /// assert_eq!(pm.template_count(), 1);
    /// ```
    pub fn template_count(&self) -> usize {
        self.templates.len()
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
