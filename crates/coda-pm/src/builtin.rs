//! Built-in prompt templates embedded at compile time.
//!
//! All `.j2` template files under `crates/coda-pm/templates/` are compiled
//! into the binary via [`include_str!`], ensuring they are always available
//! regardless of the runtime filesystem layout (e.g., after `cargo install`).
//!
//! When adding or removing `.j2` template files, update the
//! [`builtin_templates`] function accordingly.

use crate::PromptTemplate;

/// The total number of built-in prompt templates.
///
/// Used for verification in tests. Update this constant when adding or
/// removing template files.
pub const BUILTIN_TEMPLATE_COUNT: usize = 13;

/// Returns all built-in prompt templates, compiled into the binary.
///
/// Each template is loaded via [`include_str!`] at compile time from the
/// `templates/` directory relative to this crate's source root.
///
/// # Examples
///
/// ```
/// use coda_pm::builtin::builtin_templates;
///
/// let templates = builtin_templates();
/// assert_eq!(templates.len(), 13);
/// assert!(templates.iter().any(|t| t.name == "init/system"));
/// ```
pub fn builtin_templates() -> Vec<PromptTemplate> {
    vec![
        PromptTemplate::new(
            "init/analyze_repo",
            include_str!("../templates/init/analyze_repo.j2"),
        ),
        PromptTemplate::new("init/system", include_str!("../templates/init/system.j2")),
        PromptTemplate::new(
            "init/setup_project",
            include_str!("../templates/init/setup_project.j2"),
        ),
        PromptTemplate::new("plan/system", include_str!("../templates/plan/system.j2")),
        PromptTemplate::new("plan/approve", include_str!("../templates/plan/approve.j2")),
        PromptTemplate::new(
            "plan/verification",
            include_str!("../templates/plan/verification.j2"),
        ),
        PromptTemplate::new("run/system", include_str!("../templates/run/system.j2")),
        PromptTemplate::new(
            "run/dev_phase",
            include_str!("../templates/run/dev_phase.j2"),
        ),
        PromptTemplate::new("run/review", include_str!("../templates/run/review.j2")),
        PromptTemplate::new("run/verify", include_str!("../templates/run/verify.j2")),
        PromptTemplate::new("run/resume", include_str!("../templates/run/resume.j2")),
        PromptTemplate::new(
            "run/create_pr",
            include_str!("../templates/run/create_pr.j2"),
        ),
        PromptTemplate::new(
            "commit/fix_hook_errors",
            include_str!("../templates/commit/fix_hook_errors.j2"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_return_all_builtin_templates() {
        let templates = builtin_templates();
        assert_eq!(templates.len(), BUILTIN_TEMPLATE_COUNT);
    }

    #[test]
    fn test_should_include_all_expected_template_names() {
        let templates = builtin_templates();
        let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();

        let expected = [
            "init/analyze_repo",
            "init/system",
            "init/setup_project",
            "plan/system",
            "plan/approve",
            "plan/verification",
            "run/system",
            "run/dev_phase",
            "run/review",
            "run/verify",
            "run/resume",
            "run/create_pr",
            "commit/fix_hook_errors",
        ];

        for name in &expected {
            assert!(names.contains(name), "Missing template: {name}");
        }
    }

    #[test]
    fn test_should_have_non_empty_content_for_all_templates() {
        let templates = builtin_templates();
        for template in &templates {
            assert!(
                !template.content.is_empty(),
                "Template '{}' has empty content",
                template.name,
            );
        }
    }
}
