//! Template loader for loading `.j2` template files from a directory.
//!
//! Recursively walks a directory, loading all `.j2` files and deriving
//! template names from their relative paths (e.g., `init/system` from
//! `init/system.j2`).

use std::path::Path;

use crate::{PromptError, PromptTemplate};

/// Recursively loads all `.j2` template files from the given directory.
///
/// Template names are derived from relative paths with `/` separator,
/// without the `.j2` extension.
///
/// # Examples
///
/// Given a directory structure:
/// ```text
/// templates/
/// ├── init/
/// │   ├── system.j2
/// │   └── analyze_repo.j2
/// └── run/
///     └── setup.j2
/// ```
///
/// This produces templates named `init/system`, `init/analyze_repo`, and `run/setup`.
///
/// # Errors
///
/// Returns `PromptError::IoError` if the directory cannot be read or a file
/// cannot be opened.
pub fn load_templates_from_dir(dir: &Path) -> Result<Vec<PromptTemplate>, PromptError> {
    let mut templates = Vec::new();
    load_templates_recursive(dir, dir, &mut templates)?;
    Ok(templates)
}

/// Recursively walks directory entries, collecting `.j2` templates.
fn load_templates_recursive(
    base: &Path,
    current: &Path,
    templates: &mut Vec<PromptTemplate>,
) -> Result<(), PromptError> {
    let entries = std::fs::read_dir(current)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            load_templates_recursive(base, &path, templates)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("j2") {
            let relative = path
                .strip_prefix(base)
                .map_err(|e| PromptError::IoError(std::io::Error::other(e)))?;

            // Build template name: relative path without .j2 extension, using `/` separator
            let name = relative
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");

            let content = std::fs::read_to_string(&path)?;
            templates.push(PromptTemplate::new(name, content));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn test_should_load_templates_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let init_dir = dir.path().join("init");
        fs::create_dir_all(&init_dir).unwrap();
        fs::write(init_dir.join("system.j2"), "Hello {{ name }}!").unwrap();
        fs::write(init_dir.join("setup.j2"), "Setup {{ project }}").unwrap();

        let templates = load_templates_from_dir(dir.path()).unwrap();
        assert_eq!(templates.len(), 2);

        let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"init/system"));
        assert!(names.contains(&"init/setup"));
    }

    #[test]
    fn test_should_ignore_non_j2_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.md"), "# README").unwrap();
        fs::write(dir.path().join("template.j2"), "Hello").unwrap();

        let templates = load_templates_from_dir(dir.path()).unwrap();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "template");
    }

    #[test]
    fn test_should_return_empty_for_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let templates = load_templates_from_dir(dir.path()).unwrap();
        assert!(templates.is_empty());
    }

    #[test]
    fn test_should_handle_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("deep.j2"), "Deep template").unwrap();

        let templates = load_templates_from_dir(dir.path()).unwrap();
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "a/b/deep");
    }
}
