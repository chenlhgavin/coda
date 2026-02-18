//! GitHub CLI operations abstraction.
//!
//! Defines the [`GhOps`] trait for GitHub CLI interactions and provides
//! [`DefaultGhOps`], the production implementation that shells out to `gh`.
//! This abstraction enables unit-testing modules that query PR state without
//! requiring a real GitHub repository.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::debug;

use crate::CoreError;

/// PR status as returned by the GitHub CLI.
#[derive(Debug, Clone, Deserialize)]
pub struct PrStatus {
    /// PR state string (e.g., `"OPEN"`, `"MERGED"`, `"CLOSED"`).
    pub state: String,

    /// PR number in the repository.
    #[serde(default)]
    pub number: u32,

    /// PR URL (only populated by some queries).
    #[serde(default)]
    pub url: Option<String>,
}

/// Abstraction over GitHub CLI (`gh`) operations.
///
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks.
pub trait GhOps: Send + Sync {
    /// Queries the state of a PR by its number.
    ///
    /// Returns `Ok(None)` when the `gh` command fails (e.g., not authenticated
    /// or PR doesn't exist).
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` only on JSON parse failures; command
    /// failures are mapped to `Ok(None)`.
    fn pr_view_state(&self, pr_number: u32) -> Result<Option<PrStatus>, CoreError>;

    /// Discovers a PR for the given branch head across all states.
    ///
    /// Returns `Ok(None)` when no PR is found or the command fails.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` only on JSON parse failures.
    fn pr_list_by_branch(&self, branch: &str) -> Result<Option<PrStatus>, CoreError>;

    /// Discovers the PR URL for a branch head (open PRs only).
    ///
    /// Used as a fallback when the agent doesn't return a PR URL.
    /// Returns `None` when no PR is found or the command fails.
    fn pr_url_for_branch(&self, branch: &str, cwd: &Path) -> Option<String>;
}

/// Production [`GhOps`] implementation that shells out to the `gh` binary.
#[derive(Debug)]
pub struct DefaultGhOps {
    project_root: PathBuf,
}

impl DefaultGhOps {
    /// Creates a new instance rooted at `project_root`.
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

impl GhOps for DefaultGhOps {
    fn pr_view_state(&self, pr_number: u32) -> Result<Option<PrStatus>, CoreError> {
        let number_str = pr_number.to_string();
        let result = run_gh(
            &self.project_root,
            &["pr", "view", &number_str, "--json", "state,number"],
        );

        match result {
            Ok(output) => {
                let status: PrStatus = serde_json::from_str(output.trim()).map_err(|e| {
                    CoreError::GitError(format!("Failed to parse gh pr view output: {e}"))
                })?;
                Ok(Some(status))
            }
            Err(e) => {
                debug!(pr_number, error = %e, "gh pr view failed");
                Ok(None)
            }
        }
    }

    fn pr_list_by_branch(&self, branch: &str) -> Result<Option<PrStatus>, CoreError> {
        let result = run_gh(
            &self.project_root,
            &[
                "pr",
                "list",
                "--head",
                branch,
                "--state",
                "all",
                "--json",
                "state,number,url",
                "--limit",
                "1",
            ],
        );

        match result {
            Ok(output) => {
                let statuses: Vec<PrStatus> = serde_json::from_str(output.trim()).map_err(|e| {
                    CoreError::GitError(format!("Failed to parse gh pr list output: {e}"))
                })?;
                Ok(statuses.into_iter().next())
            }
            Err(e) => {
                debug!(branch, error = %e, "gh pr list failed");
                Ok(None)
            }
        }
    }

    fn pr_url_for_branch(&self, branch: &str, cwd: &Path) -> Option<String> {
        let result = run_gh(
            cwd,
            &[
                "pr", "list", "--head", branch, "--json", "url", "--limit", "1",
            ],
        );

        match result {
            Ok(output) => {
                let trimmed = output.trim();
                if trimmed.is_empty() || trimmed == "[]" {
                    debug!(branch, "No PR found via gh pr list");
                    return None;
                }
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
                    arr.first()
                        .and_then(|v| v.get("url"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                } else {
                    crate::parser::extract_pr_url(trimmed)
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "gh pr list failed");
                None
            }
        }
    }
}

/// Runs a `gh` command and returns its stdout.
fn run_gh(cwd: &Path, args: &[&str]) -> Result<String, CoreError> {
    debug!(cwd = %cwd.display(), args = ?args, "gh");
    let output = std::process::Command::new("gh")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(CoreError::IoError)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoreError::GitError(format!(
            "gh {} failed: {stderr}",
            args.join(" "),
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
