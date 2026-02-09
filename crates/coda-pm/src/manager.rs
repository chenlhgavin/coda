//! Prompt manager implementation.

use std::collections::HashMap;

use minijinja::Environment;
use serde::Serialize;

use crate::{PromptError, PromptTemplate};

pub struct PromptManager {
    env: Environment<'static>,
    templates: HashMap<String, PromptTemplate>,
}

impl PromptManager {
    pub fn new() -> Self {
        Self {
            env: Environment::new(),
            templates: HashMap::new(),
        }
    }

    pub fn add_template(&mut self, template: PromptTemplate) -> Result<(), PromptError> {
        self.env
            .add_template_owned(template.name.clone(), template.content.clone())
            .map_err(|e: minijinja::Error| PromptError::InvalidTemplate(e.to_string()))?;
        self.templates.insert(template.name.clone(), template);
        Ok(())
    }

    pub fn render<T: Serialize>(&self, name: &str, ctx: T) -> Result<String, PromptError> {
        let tmpl = self
            .env
            .get_template(name)
            .map_err(|_| PromptError::TemplateNotFound(name.to_string()))?;

        tmpl.render(ctx)
            .map_err(|e| PromptError::RenderError(e.to_string()))
    }

    pub fn get_template(&self, name: &str) -> Option<&PromptTemplate> {
        self.templates.get(name)
    }
}

impl Default for PromptManager {
    fn default() -> Self {
        Self::new()
    }
}
