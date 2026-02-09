//! CODA Prompt Manager
//!
//! A template-based prompt management system using minijinja.

mod error;
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
}
