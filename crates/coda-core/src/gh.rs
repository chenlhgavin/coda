//! GitHub CLI operations abstraction.
//!
//! Defines the [`GhOps`] trait for GitHub CLI interactions and provides
//! [`DefaultGhOps`], the production implementation that shells out to `gh`.
//! This abstraction enables unit-testing modules that query PR state without
//! requiring a real GitHub repository.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::CoreError;

/// State of a GitHub pull request.
///
/// Used by [`PrStatus`] and [`CleanedWorktree`](crate::CleanedWorktree) to
/// represent PR lifecycle state with compile-time exhaustiveness checking
/// instead of string comparisons.
///
/// Deserialization handles the UPPERCASE format returned by the GitHub CLI
/// (`"OPEN"`, `"MERGED"`, `"CLOSED"`, `"DRAFT"`).
///
/// # Examples
///
/// ```
/// use coda_core::gh::PrState;
///
/// let state: PrState = serde_json::from_str("\"MERGED\"").unwrap();
/// assert_eq!(state, PrState::Merged);
/// assert_eq!(state.to_string(), "MERGED");
///
/// let parsed: PrState = "OPEN".parse().unwrap();
/// assert_eq!(parsed, PrState::Open);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrState {
    /// PR is open and accepting reviews/changes.
    Open,
    /// PR has been merged into the base branch.
    Merged,
    /// PR has been closed without merging.
    Closed,
    /// PR is in draft mode (not yet ready for review).
    Draft,
}

impl PrState {
    /// Returns `true` if the PR has been merged or closed (i.e., it is
    /// no longer active and the worktree can be cleaned up).
    ///
    /// # Examples
    ///
    /// ```
    /// use coda_core::gh::PrState;
    ///
    /// assert!(PrState::Merged.is_terminal());
    /// assert!(PrState::Closed.is_terminal());
    /// assert!(!PrState::Open.is_terminal());
    /// assert!(!PrState::Draft.is_terminal());
    /// ```
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }
}

impl std::fmt::Display for PrState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "OPEN"),
            Self::Merged => write!(f, "MERGED"),
            Self::Closed => write!(f, "CLOSED"),
            Self::Draft => write!(f, "DRAFT"),
        }
    }
}

impl FromStr for PrState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "OPEN" => Ok(Self::Open),
            "MERGED" => Ok(Self::Merged),
            "CLOSED" => Ok(Self::Closed),
            "DRAFT" => Ok(Self::Draft),
            other => Err(format!("unknown PR state: {other}")),
        }
    }
}

/// PR status as returned by the GitHub CLI.
#[derive(Debug, Clone, Deserialize)]
pub struct PrStatus {
    /// PR lifecycle state.
    pub state: PrState,

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

    /// Queries the PR title by its number.
    ///
    /// Used to populate `PrInfo.title` with the actual title from GitHub
    /// rather than a generated fallback.
    /// Returns `None` when the command fails or the PR doesn't exist.
    fn pr_view_title(&self, pr_number: u32, cwd: &Path) -> Option<String>;
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

    fn pr_view_title(&self, pr_number: u32, cwd: &Path) -> Option<String> {
        let number_str = pr_number.to_string();
        let result = run_gh(cwd, &["pr", "view", &number_str, "--json", "title"]);

        match result {
            Ok(output) => {
                let parsed: serde_json::Value = serde_json::from_str(output.trim()).ok()?;
                parsed
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            }
            Err(e) => {
                debug!(pr_number, error = %e, "gh pr view title failed");
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── PrState ─────────────────────────────────────────────────────

    #[test]
    fn test_should_serialize_pr_state_to_uppercase() {
        assert_eq!(serde_json::to_string(&PrState::Open).unwrap(), "\"OPEN\"");
        assert_eq!(
            serde_json::to_string(&PrState::Merged).unwrap(),
            "\"MERGED\""
        );
        assert_eq!(
            serde_json::to_string(&PrState::Closed).unwrap(),
            "\"CLOSED\""
        );
        assert_eq!(serde_json::to_string(&PrState::Draft).unwrap(), "\"DRAFT\"");
    }

    #[test]
    fn test_should_deserialize_pr_state_from_uppercase() {
        assert_eq!(
            serde_json::from_str::<PrState>("\"OPEN\"").unwrap(),
            PrState::Open
        );
        assert_eq!(
            serde_json::from_str::<PrState>("\"MERGED\"").unwrap(),
            PrState::Merged
        );
        assert_eq!(
            serde_json::from_str::<PrState>("\"CLOSED\"").unwrap(),
            PrState::Closed
        );
        assert_eq!(
            serde_json::from_str::<PrState>("\"DRAFT\"").unwrap(),
            PrState::Draft
        );
    }

    #[test]
    fn test_should_round_trip_pr_state_serde() {
        for state in [
            PrState::Open,
            PrState::Merged,
            PrState::Closed,
            PrState::Draft,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: PrState = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn test_should_display_pr_state_uppercase() {
        assert_eq!(PrState::Open.to_string(), "OPEN");
        assert_eq!(PrState::Merged.to_string(), "MERGED");
        assert_eq!(PrState::Closed.to_string(), "CLOSED");
        assert_eq!(PrState::Draft.to_string(), "DRAFT");
    }

    #[test]
    fn test_should_parse_pr_state_from_str_case_insensitive() {
        assert_eq!("open".parse::<PrState>().unwrap(), PrState::Open);
        assert_eq!("MERGED".parse::<PrState>().unwrap(), PrState::Merged);
        assert_eq!("Closed".parse::<PrState>().unwrap(), PrState::Closed);
        assert_eq!("draft".parse::<PrState>().unwrap(), PrState::Draft);
    }

    #[test]
    fn test_should_reject_invalid_pr_state() {
        assert!("invalid".parse::<PrState>().is_err());
    }

    #[test]
    fn test_should_display_and_from_str_roundtrip_for_pr_state() {
        for state in [
            PrState::Open,
            PrState::Merged,
            PrState::Closed,
            PrState::Draft,
        ] {
            let displayed = state.to_string();
            let parsed: PrState = displayed.parse().unwrap();
            assert_eq!(parsed, state);
        }
    }

    #[test]
    fn test_should_identify_terminal_pr_states() {
        assert!(PrState::Merged.is_terminal());
        assert!(PrState::Closed.is_terminal());
        assert!(!PrState::Open.is_terminal());
        assert!(!PrState::Draft.is_terminal());
    }

    #[test]
    fn test_should_deserialize_pr_status_with_typed_state() {
        let json = r#"{"state":"OPEN","number":42,"url":"https://github.com/org/repo/pull/42"}"#;
        let status: PrStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.state, PrState::Open);
        assert_eq!(status.number, 42);
    }
}
