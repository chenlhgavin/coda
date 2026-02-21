//! GitHub repository integration and branch switching.
//!
//! - [`handle_repos`] — `/coda repos`: lists GitHub repos via `gh` CLI
//!   and posts a Block Kit select menu. When a repo is selected, the
//!   interaction handler clones it and auto-binds the channel.
//! - [`handle_switch`] — `/coda switch <branch>`: switches the bound
//!   repository's checked-out branch with proper locking.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Local;
use serde::Deserialize;
use tracing::{info, warn};

use crate::error::ServerError;
use crate::formatter::{self, RepoListEntry};
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

/// Handles `/coda repos`.
///
/// Runs `gh repo list` to fetch repositories owned by the authenticated
/// user and posts a Block Kit select menu to the channel. Requires the
/// `workspace` config to be set.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails or the `gh` CLI
/// returns an error.
pub async fn handle_repos(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some(workspace) = state.workspace() else {
        let blocks = formatter::error(
            "No workspace directory configured. Set `workspace` in `~/.coda-server/config.yml`.",
        );
        state.slack().post_message(channel, blocks).await?;
        return Ok(());
    };

    info!(channel, "Listing GitHub repos");

    let repos = match list_github_repos().await {
        Ok(repos) => repos,
        Err(e) => {
            let blocks = formatter::error(&format!("Failed to list repos: {e}"));
            state.slack().post_message(channel, blocks).await?;
            return Ok(());
        }
    };

    if repos.is_empty() {
        let blocks = formatter::error("No repositories found. Check your `gh` authentication.");
        state.slack().post_message(channel, blocks).await?;
        return Ok(());
    }

    let entries: Vec<RepoListEntry> = repos
        .into_iter()
        .map(|r| RepoListEntry {
            name_with_owner: r.name_with_owner,
            description: r.description,
        })
        .collect();

    let blocks = formatter::repo_select_menu(&entries);
    state.slack().post_message(channel, blocks).await?;

    info!(
        channel,
        workspace = %workspace.display(),
        "Posted repo select menu"
    );
    Ok(())
}

/// Handles `/coda switch <branch>`.
///
/// Acquires a repository lock, validates the working directory is clean,
/// fetches the latest refs, checks out the target branch, and attempts
/// a fast-forward pull.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails.
pub async fn handle_switch(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    branch: &str,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some(repo_path) = state.bindings().get(channel) else {
        let blocks = formatter::error(
            "No repository bound to this channel. Use `/coda repos` to clone and bind a repository first.",
        );
        state.slack().post_message(channel, blocks).await?;
        return Ok(());
    };

    // Acquire repo lock
    let lock_desc = format!("switch {branch}");
    if let Err(holder) = state.repo_locks().try_lock(&repo_path, &lock_desc) {
        let blocks = formatter::error(&format!(
            "Repository is busy (`{holder}`). Try again later."
        ));
        state.slack().post_message(channel, blocks).await?;
        return Ok(());
    }

    info!(channel, branch, repo = %repo_path.display(), "Switching branch");

    let result = switch_branch(&repo_path, branch).await;

    // Release lock
    state.repo_locks().unlock(&repo_path);

    let blocks = match result {
        Ok(msg) => {
            info!(channel, branch, "Branch switch completed");
            vec![serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(":white_check_mark: {msg}")
                }
            })]
        }
        Err(e) => {
            warn!(channel, branch, error = %e, "Branch switch failed");
            formatter::error(&e)
        }
    };

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}

/// Handles the repo clone action triggered from the select menu.
///
/// Clones the selected repository into the workspace directory and
/// auto-binds the channel.
pub async fn handle_repo_clone(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    repo_name: &str,
) {
    let Some(workspace) = state.workspace() else {
        let blocks = formatter::error("Workspace directory is not configured.");
        let _ = state
            .slack()
            .update_message(channel_id, message_ts, blocks)
            .await;
        return;
    };

    let clone_path = build_clone_path(workspace, repo_name);

    info!(
        channel_id,
        repo_name,
        clone_path = %clone_path.display(),
        "Cloning repository"
    );

    // Ensure parent directory exists
    if let Some(parent) = clone_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        let blocks = formatter::error(&format!("Failed to create directory: {e}"));
        let _ = state
            .slack()
            .update_message(channel_id, message_ts, blocks)
            .await;
        return;
    }

    match clone_repo(repo_name, &clone_path).await {
        Ok(()) => {
            info!(
                channel_id,
                repo_name,
                path = %clone_path.display(),
                "Repository cloned"
            );
        }
        Err(e) => {
            warn!(
                channel_id,
                repo_name,
                error = %e,
                "Failed to clone repository"
            );
            let blocks = formatter::error(&format!("Clone failed: {e}"));
            let _ = state
                .slack()
                .update_message(channel_id, message_ts, blocks)
                .await;
            return;
        }
    }

    // Auto-bind the channel
    match state.bindings().set(channel_id, clone_path.clone()) {
        Ok(()) => {
            let blocks = vec![serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(
                        ":white_check_mark: Cloned and bound `{repo_name}` to `{}`",
                        clone_path.display()
                    )
                }
            })];
            let _ = state
                .slack()
                .update_message(channel_id, message_ts, blocks)
                .await;
        }
        Err(e) => {
            let blocks = formatter::error(&format!("Binding failed: {e}"));
            let _ = state
                .slack()
                .update_message(channel_id, message_ts, blocks)
                .await;
        }
    }
}

/// JSON structure returned by `gh repo list --json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoInfo {
    name_with_owner: String,
    #[serde(default)]
    description: Option<String>,
}

/// Lists GitHub repositories owned by the authenticated `gh` CLI user.
async fn list_github_repos() -> Result<Vec<RepoInfo>, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["repo", "list"]);
    cmd.args(["--json", "nameWithOwner,description", "--limit", "30"]);

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run `gh`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`gh repo list` failed: {stderr}"));
    }

    let repos: Vec<RepoInfo> = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse `gh` output: {e}"))?;

    Ok(repos)
}

/// Builds the clone path: `<workspace>/<repo>-<YYYYMMDD-HHMMSS>`.
///
/// Extracts the repo name from `owner/repo` format and appends a
/// timestamp to ensure unique clone directories.
fn build_clone_path(workspace: &Path, repo_name: &str) -> PathBuf {
    let short_name = repo_name
        .rsplit_once('/')
        .map_or(repo_name, |(_, repo)| repo);
    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    workspace.join(format!("{short_name}-{timestamp}"))
}

/// Clones a GitHub repository using the `gh` CLI.
async fn clone_repo(repo_name: &str, clone_path: &Path) -> Result<(), String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "repo",
            "clone",
            repo_name,
            &clone_path.display().to_string(),
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to run `gh repo clone`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`gh repo clone` failed: {stderr}"));
    }

    Ok(())
}

/// Switches a repository to a different branch.
///
/// Uses `spawn_blocking` to run synchronous git operations.
async fn switch_branch(repo_path: &Path, branch: &str) -> Result<String, String> {
    let repo_path = repo_path.to_path_buf();
    let branch = branch.to_string();

    tokio::task::spawn_blocking(move || switch_branch_sync(&repo_path, &branch))
        .await
        .map_err(|e| format!("Branch switch task panicked: {e}"))?
}

/// Synchronous git branch switch operations.
fn switch_branch_sync(repo_path: &Path, branch: &str) -> Result<String, String> {
    // 1. Check for dirty working directory
    let status_output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git status`: {e}"))?;

    if !status_output.status.success() {
        let stderr = String::from_utf8_lossy(&status_output.stderr);
        return Err(format!("`git status` failed: {stderr}"));
    }

    let status_text = String::from_utf8_lossy(&status_output.stdout);
    if !status_text.trim().is_empty() {
        return Err(
            "Working directory has uncommitted changes. Commit or stash them first.".to_string(),
        );
    }

    // 2. Fetch latest refs
    let fetch_output = std::process::Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git fetch`: {e}"))?;

    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        return Err(format!("`git fetch origin` failed: {stderr}"));
    }

    // 3. Checkout branch
    let checkout_output = std::process::Command::new("git")
        .args(["checkout", branch])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git checkout`: {e}"))?;

    if !checkout_output.status.success() {
        let stderr = String::from_utf8_lossy(&checkout_output.stderr);
        return Err(format!("`git checkout {branch}` failed: {stderr}"));
    }

    // 4. Fast-forward pull (non-fatal if fails)
    let pull_output = std::process::Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git pull`: {e}"))?;

    if pull_output.status.success() {
        Ok(format!(
            "Switched to branch `{branch}` and pulled latest changes."
        ))
    } else {
        Ok(format!(
            "Switched to branch `{branch}`. Fast-forward pull skipped (branch may have diverged)."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_build_clone_path_with_timestamp() {
        let workspace = Path::new("/home/user/workspace");
        let path = build_clone_path(workspace, "org/my-repo");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("my-repo-"), "got: {name}");
        // Verify timestamp format: YYYYMMDD-HHMMSS (15 chars)
        let suffix = &name["my-repo-".len()..];
        assert_eq!(suffix.len(), 15, "timestamp should be YYYYMMDD-HHMMSS");
        assert_eq!(path.parent().unwrap(), Path::new("/home/user/workspace"));
    }

    #[test]
    fn test_should_build_clone_path_without_owner_prefix() {
        let workspace = Path::new("/ws");
        let path = build_clone_path(workspace, "deep-org/sub-repo");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("sub-repo-"),
            "should strip owner prefix, got: {name}"
        );
    }

    #[test]
    fn test_should_build_clone_path_plain_name() {
        let workspace = Path::new("/ws");
        let path = build_clone_path(workspace, "my-repo");
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("my-repo-"),
            "should handle plain repo names, got: {name}"
        );
    }

    #[test]
    fn test_should_deserialize_repo_info() {
        let json = r#"[
            {"nameWithOwner": "org/repo", "description": "A repo"},
            {"nameWithOwner": "org/other"}
        ]"#;
        let repos: Vec<RepoInfo> = serde_json::from_str(json).expect("deserialize");
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name_with_owner, "org/repo");
        assert_eq!(repos[0].description.as_deref(), Some("A repo"));
        assert!(repos[1].description.is_none());
    }
}
