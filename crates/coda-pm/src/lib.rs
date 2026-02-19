//! CODA Prompt Manager
//!
//! A template-based prompt management system using minijinja. Supports
//! loading `.j2` template files from directories and rendering them with
//! structured context data.
//!
//! # Usage
//!
//! ```
//! use coda_pm::{PromptManager, PromptTemplate};
//!
//! let mut pm = PromptManager::new();
//! pm.add_template(PromptTemplate::new("greeting", "Hello, {{ name }}!")).unwrap();
//! let rendered = pm.render("greeting", minijinja::context!(name => "World")).unwrap();
//! assert_eq!(rendered, "Hello, World!");
//! ```

pub mod builtin;
mod error;
pub mod loader;
mod manager;
mod template;

pub use error::PromptError;
pub use manager::PromptManager;
pub use template::PromptTemplate;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_manager_render() {
        let mut pm = PromptManager::new();
        let template = PromptTemplate::new("greeting", "Hello, {{ name }}!");
        pm.add_template(template).unwrap();

        let result = pm
            .render("greeting", minijinja::context!(name => "World"))
            .unwrap();
        assert_eq!(result, "Hello, World!");
    }

    #[test]
    fn test_should_load_all_builtin_templates_via_with_builtin_templates() {
        let pm = PromptManager::with_builtin_templates().unwrap();
        assert_eq!(pm.template_count(), builtin::BUILTIN_TEMPLATE_COUNT);

        // Verify a sample of templates are accessible by name
        assert!(pm.get_template("init/system").is_some());
        assert!(pm.get_template("run/dev_phase").is_some());
        assert!(pm.get_template("plan/approve").is_some());
    }

    #[test]
    fn test_should_render_builtin_template() {
        let pm = PromptManager::with_builtin_templates().unwrap();

        // init/system takes no variables, so we can render it with empty context
        let result = pm.render("init/system", minijinja::context!());
        assert!(result.is_ok(), "Failed to render init/system: {result:?}");
        assert!(!result.unwrap().is_empty());
    }
}
