//! GitHub repository integration and branch switching.
//!
//! - [`handle_repos`] — `/coda repos`: lists GitHub repos via `gh` CLI
//!   and posts a Block Kit select menu. When a repo is selected, the
//!   interaction handler clones it and auto-binds the channel.
//! - [`handle_switch`] — `/coda switch <branch>`: switches the bound
//!   repository's checked-out branch with proper locking.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info, warn};

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

/// Handles the repo clone-or-update action triggered from the select menu.
///
/// If the target path already contains a `.git` directory, the existing
/// clone is updated (fetch + checkout default branch + pull). Otherwise
/// a fresh clone is performed. In both cases the channel is auto-bound.
///
/// Acquires a repository lock for the duration of clone/update + bind
/// to prevent concurrent operations on the same path.
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

    let clone_path = match build_clone_path(workspace, repo_name) {
        Ok(path) => path,
        Err(e) => {
            warn!(channel_id, repo_name, error = %e, "Invalid repo name");
            let blocks = formatter::error(&e);
            let _ = state
                .slack()
                .update_message(channel_id, message_ts, blocks)
                .await;
            return;
        }
    };

    // Acquire repo lock to prevent concurrent clone/update on the same path
    let lock_desc = format!("clone/update {repo_name}");
    if let Err(holder) = state.repo_locks().try_lock(&clone_path, &lock_desc) {
        let blocks = formatter::error(&format!(
            "Repository is busy (`{holder}`). Try again later."
        ));
        let _ = state
            .slack()
            .update_message(channel_id, message_ts, blocks)
            .await;
        return;
    }

    let result =
        clone_or_update_locked(state, channel_id, message_ts, repo_name, &clone_path).await;

    // Always release the lock
    state.repo_locks().unlock(&clone_path);

    if let Err(msg) = result {
        let blocks = formatter::error(&msg);
        let _ = state
            .slack()
            .update_message(channel_id, message_ts, blocks)
            .await;
    }
}

/// Performs clone-or-update and binding while the repo lock is held.
async fn clone_or_update_locked(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    repo_name: &str,
    clone_path: &Path,
) -> Result<(), String> {
    let is_existing = clone_path.join(".git").exists();

    let bind_action = if is_existing {
        clone_or_update_existing(channel_id, repo_name, clone_path).await?
    } else {
        clone_fresh(channel_id, repo_name, clone_path).await?
    };

    // Auto-bind the channel
    match state.bindings().set(channel_id, clone_path.to_path_buf()) {
        Ok(()) => {
            let blocks = vec![serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(
                        ":white_check_mark: {bind_action} `{repo_name}` to `{}`",
                        clone_path.display()
                    )
                }
            })];
            let _ = state
                .slack()
                .update_message(channel_id, message_ts, blocks)
                .await;
            Ok(())
        }
        Err(e) => Err(format!("Binding failed: {e}")),
    }
}

/// Updates an existing clone and returns the bind action label.
async fn clone_or_update_existing(
    channel_id: &str,
    repo_name: &str,
    clone_path: &Path,
) -> Result<String, String> {
    info!(
        channel_id,
        repo_name,
        clone_path = %clone_path.display(),
        "Updating existing repository clone"
    );

    match update_repo(clone_path).await {
        Ok(branch) => {
            info!(
                channel_id,
                repo_name,
                branch,
                path = %clone_path.display(),
                "Repository updated"
            );
            Ok(format!("Updated and bound (branch `{branch}`)"))
        }
        Err(e) => {
            warn!(
                channel_id,
                repo_name,
                error = %e,
                "Failed to update repository"
            );
            Err(format!("Update failed: {e}"))
        }
    }
}

/// Clones a fresh repo and returns the bind action label.
async fn clone_fresh(
    channel_id: &str,
    repo_name: &str,
    clone_path: &Path,
) -> Result<String, String> {
    info!(
        channel_id,
        repo_name,
        clone_path = %clone_path.display(),
        "Cloning repository"
    );

    // Ensure parent directory exists
    if let Some(parent) = clone_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    match clone_repo(repo_name, clone_path).await {
        Ok(()) => {
            info!(
                channel_id,
                repo_name,
                path = %clone_path.display(),
                "Repository cloned"
            );
            Ok("Cloned and bound".to_string())
        }
        Err(e) => {
            warn!(
                channel_id,
                repo_name,
                error = %e,
                "Failed to clone repository"
            );
            Err(format!("Clone failed: {e}"))
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

/// Validates a repository name and builds a stable clone path.
///
/// Accepts either `owner/repo` or plain `repo` names. Rejects names that
/// contain path traversal segments (`..`), absolute paths, or characters
/// outside the set `[a-zA-Z0-9._-/]`.
///
/// Returns `<workspace>/<owner>/<repo>` or `<workspace>/<name>`, and
/// verifies the result is a child of `workspace`.
///
/// # Errors
///
/// Returns an error string if the repo name is invalid or the resolved
/// path escapes the workspace.
fn build_clone_path(workspace: &Path, repo_name: &str) -> Result<PathBuf, String> {
    if repo_name.is_empty() {
        return Err("Repository name is empty.".to_string());
    }

    // Reject absolute paths
    if repo_name.starts_with('/') || repo_name.starts_with('\\') {
        return Err(format!(
            "Invalid repository name `{repo_name}`: absolute paths are not allowed."
        ));
    }

    // Reject path traversal segments
    for segment in repo_name.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!(
                "Invalid repository name `{repo_name}`: path traversal is not allowed."
            ));
        }
    }

    // Allow only safe characters: alphanumeric, hyphen, underscore, dot, slash
    if !repo_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(format!(
            "Invalid repository name `{repo_name}`: contains disallowed characters."
        ));
    }

    // Expect at most one slash (owner/repo)
    if repo_name.matches('/').count() > 1 {
        return Err(format!(
            "Invalid repository name `{repo_name}`: expected `owner/repo` or `repo` format."
        ));
    }

    let candidate = workspace.join(repo_name);

    // Final containment check: the candidate must start with the workspace prefix
    if !candidate.starts_with(workspace) {
        return Err(format!(
            "Invalid repository name `{repo_name}`: resolved path escapes workspace."
        ));
    }

    Ok(candidate)
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

/// Updates an existing repository clone to the latest default branch.
///
/// Performs the following sequence:
/// 1. `git fetch origin`
/// 2. Detect default branch via `git symbolic-ref refs/remotes/origin/HEAD`
///    (falls back to `main` if the symbolic ref is not set)
/// 3. `git checkout --force <branch>` (force handles dirty working trees)
/// 4. `git pull --ff-only`; falls back to `git pull --rebase` on failure
///
/// Returns the default branch name on success.
async fn update_repo(repo_path: &Path) -> Result<String, String> {
    let repo_path = repo_path.to_path_buf();
    tokio::task::spawn_blocking(move || update_repo_sync(&repo_path))
        .await
        .map_err(|e| format!("Repository update task panicked: {e}"))?
}

/// Synchronous repository update operations.
fn update_repo_sync(repo_path: &Path) -> Result<String, String> {
    // 1. Fetch latest refs from origin
    let fetch_output = std::process::Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git fetch`: {e}"))?;

    if !fetch_output.status.success() {
        let stderr = String::from_utf8_lossy(&fetch_output.stderr);
        return Err(format!("`git fetch origin` failed: {stderr}"));
    }

    // 2. Detect default branch
    let branch = detect_default_branch(repo_path);
    debug!(branch, path = %repo_path.display(), "Detected default branch");

    // 3. Force checkout the default branch (handles dirty working trees)
    let checkout_output = std::process::Command::new("git")
        .args(["checkout", "--force", &branch])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git checkout`: {e}"))?;

    if !checkout_output.status.success() {
        let stderr = String::from_utf8_lossy(&checkout_output.stderr);
        return Err(format!("`git checkout --force {branch}` failed: {stderr}"));
    }

    // 4. Pull latest: try ff-only first, fall back to rebase
    let pull_ff = std::process::Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run `git pull --ff-only`: {e}"))?;

    if !pull_ff.status.success() {
        debug!(
            path = %repo_path.display(),
            "Fast-forward pull failed, falling back to rebase"
        );
        let pull_rebase = std::process::Command::new("git")
            .args(["pull", "--rebase"])
            .current_dir(repo_path)
            .output()
            .map_err(|e| format!("Failed to run `git pull --rebase`: {e}"))?;

        if !pull_rebase.status.success() {
            let stderr = String::from_utf8_lossy(&pull_rebase.stderr);
            return Err(format!("`git pull --rebase` failed: {stderr}"));
        }
    }

    Ok(branch)
}

/// Detects the default branch by reading `refs/remotes/origin/HEAD`.
///
/// Returns the branch name (e.g. `main`) with the `origin/` prefix stripped.
/// Falls back to `"main"` when the symbolic ref is not set.
fn detect_default_branch(repo_path: &Path) -> String {
    let output = std::process::Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
        .current_dir(repo_path)
        .output();

    let Ok(output) = output else {
        return "main".to_string();
    };

    if !output.status.success() {
        return "main".to_string();
    }

    let full_ref = String::from_utf8_lossy(&output.stdout);
    let trimmed = full_ref.trim();

    // Strip "origin/" prefix: "origin/main" → "main"
    trimmed
        .strip_prefix("origin/")
        .unwrap_or(trimmed)
        .to_string()
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
    fn test_should_build_clone_path_with_owner() {
        let workspace = Path::new("/home/user/workspace");
        let path = build_clone_path(workspace, "org/my-repo");
        assert_eq!(
            path.as_deref(),
            Ok(Path::new("/home/user/workspace/org/my-repo"))
        );
    }

    #[test]
    fn test_should_build_clone_path_with_nested_owner() {
        let workspace = Path::new("/ws");
        let path = build_clone_path(workspace, "deep-org/sub-repo");
        assert_eq!(path.as_deref(), Ok(Path::new("/ws/deep-org/sub-repo")));
    }

    #[test]
    fn test_should_build_clone_path_plain_name() {
        let workspace = Path::new("/ws");
        let path = build_clone_path(workspace, "my-repo");
        assert_eq!(path.as_deref(), Ok(Path::new("/ws/my-repo")));
    }

    #[test]
    fn test_should_reject_empty_repo_name() {
        let workspace = Path::new("/ws");
        let result = build_clone_path(workspace, "");
        assert!(result.is_err());
        assert!(
            result.as_ref().expect_err("should fail").contains("empty"),
            "got: {result:?}"
        );
    }

    #[test]
    fn test_should_reject_absolute_path_in_repo_name() {
        let workspace = Path::new("/ws");
        let result = build_clone_path(workspace, "/etc/passwd");
        assert!(result.is_err());
        assert!(
            result
                .as_ref()
                .expect_err("should fail")
                .contains("absolute"),
            "got: {result:?}"
        );
    }

    #[test]
    fn test_should_reject_path_traversal_in_repo_name() {
        let workspace = Path::new("/ws");
        let result = build_clone_path(workspace, "../escape/repo");
        assert!(result.is_err());
        assert!(
            result
                .as_ref()
                .expect_err("should fail")
                .contains("traversal"),
            "got: {result:?}"
        );
    }

    #[test]
    fn test_should_reject_dot_dot_segment_in_repo_name() {
        let workspace = Path::new("/ws");
        let result = build_clone_path(workspace, "owner/../escape");
        assert!(result.is_err());
    }

    #[test]
    fn test_should_reject_disallowed_characters_in_repo_name() {
        let workspace = Path::new("/ws");
        let result = build_clone_path(workspace, "owner/repo;rm -rf");
        assert!(result.is_err());
        assert!(
            result
                .as_ref()
                .expect_err("should fail")
                .contains("disallowed"),
            "got: {result:?}"
        );
    }

    #[test]
    fn test_should_reject_deeply_nested_repo_name() {
        let workspace = Path::new("/ws");
        let result = build_clone_path(workspace, "a/b/c");
        assert!(result.is_err());
        assert!(
            result
                .as_ref()
                .expect_err("should fail")
                .contains("owner/repo"),
            "got: {result:?}"
        );
    }

    #[test]
    fn test_should_accept_repo_name_with_dots_and_underscores() {
        let workspace = Path::new("/ws");
        let path = build_clone_path(workspace, "owner/my_repo.rs");
        assert_eq!(path.as_deref(), Ok(Path::new("/ws/owner/my_repo.rs")));
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

    #[test]
    fn test_should_fallback_to_main_when_origin_head_not_set() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Initialize a bare git repo — no origin/HEAD exists
        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        assert!(init.status.success());

        let branch = detect_default_branch(dir.path());
        assert_eq!(branch, "main");
    }

    #[test]
    fn test_should_return_fetch_error_when_no_remote_configured() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let init = std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        assert!(init.status.success());

        let result = update_repo_sync(dir.path());
        assert!(result.is_err());
        let err = result.expect_err("should fail");
        assert!(
            err.contains("git fetch origin"),
            "expected fetch error, got: {err}"
        );
    }

    #[test]
    fn test_should_detect_default_branch_from_origin_head() {
        let bare_dir = tempfile::tempdir().expect("create bare dir");
        let clone_dir = tempfile::tempdir().expect("create clone dir");

        // Create a bare repo with an initial commit
        let init = std::process::Command::new("git")
            .args(["init", "--bare"])
            .current_dir(bare_dir.path())
            .output()
            .expect("git init --bare");
        assert!(init.status.success());

        // Clone bare repo (creates origin/HEAD pointing to default branch)
        let clone_path = clone_dir.path().join("repo");
        let clone = std::process::Command::new("git")
            .args([
                "clone",
                &bare_dir.path().display().to_string(),
                &clone_path.display().to_string(),
            ])
            .output()
            .expect("git clone");
        assert!(
            clone.status.success(),
            "clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        );

        let branch = detect_default_branch(&clone_path);
        // The default branch for a bare init is typically "main" or "master"
        // depending on git config; either is acceptable here.
        assert!(
            branch == "main" || branch == "master",
            "expected main or master, got: {branch}"
        );
    }

    #[test]
    fn test_should_fallback_to_main_when_path_is_not_a_repo() {
        let dir = tempfile::tempdir().expect("create temp dir");
        // Not a git repo at all
        let branch = detect_default_branch(dir.path());
        assert_eq!(branch, "main");
    }
}
