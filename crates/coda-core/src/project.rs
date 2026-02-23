//! Project root discovery utilities.
//!
//! Provides functions to locate the CODA project root by walking up
//! the directory tree looking for `.coda/` or `.git/` markers.

use std::path::PathBuf;

use crate::CoreError;

/// Finds the project root by walking up from the current directory.
///
/// Looks for either a `.coda/` directory (indicating a CODA project)
/// or a `.git/` directory (indicating a git repository root).
///
/// # Errors
///
/// Returns `CoreError::IoError` if the current directory cannot be determined,
/// or `CoreError::ConfigError` if no project root is found (reached filesystem root).
pub fn find_project_root() -> Result<PathBuf, CoreError> {
    let cwd = std::env::current_dir()?;
    let mut current = cwd.as_path();
    loop {
        if current.join(".coda").exists() || current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        current = current.parent().ok_or_else(|| {
            CoreError::ConfigError("Not in a git repository or CODA project".into())
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_find_project_root_from_cwd() {
        // This test runs inside the CODA workspace which has a .git directory,
        // so it should find a project root.
        let root = find_project_root();
        assert!(root.is_ok());
        let root = root.unwrap();
        assert!(root.join(".git").exists() || root.join(".coda").exists());
    }
}
