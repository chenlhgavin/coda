//! GitHub repository integration and branch switching.
//!
//! - [`handle_repos`] — `/coda repos`: lists GitHub repos via `gh` CLI
//!   and posts a Block Kit select menu. When a repo is selected, the
//!   interaction handler clones it and auto-binds the channel.
//! - [`handle_switch`] — `/coda switch <branch>`: switches the bound
//!   repository's checked-out branch with proper locking.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

use crate::commands::streaming::STREAM_UPDATE_DEBOUNCE;
use crate::error::ServerError;
use crate::formatter::{self, PhaseDisplayStatus, RepoListEntry, RepoStepDisplay};
use crate::handlers::commands::SlashCommandPayload;
use crate::slack_client::SlackClient;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Event types for repo sync progress
// ---------------------------------------------------------------------------

/// Events emitted during a repo clone or update operation.
///
/// Consumed by [`consume_repo_sync_events`] to drive live Slack updates.
#[derive(Debug)]
enum RepoSyncEvent {
    /// The sync is starting with the given step labels.
    Starting { steps: Vec<String> },
    /// A step is about to begin.
    StepStarting { index: usize },
    /// A step completed successfully.
    StepCompleted { index: usize, duration: Duration },
    /// A step failed.
    StepFailed { index: usize, error: String },
    /// A git progress line (e.g. "Receiving objects: 45%").
    GitProgress { index: usize, line: String },
    /// The sync has finished (channel will close after this).
    Finished,
}

// ---------------------------------------------------------------------------
// /coda repos
// ---------------------------------------------------------------------------

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
#[instrument(skip(state, payload), fields(channel = %payload.channel_id))]
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

    let start = Instant::now();
    let repos = match list_github_repos().await {
        Ok(repos) => {
            let duration_ms = start.elapsed().as_millis();
            debug!(
                duration_ms,
                count = repos.len(),
                "list_github_repos() completed"
            );
            repos
        }
        Err(e) => {
            let duration_ms = start.elapsed().as_millis();
            debug!(duration_ms, error = %e, "list_github_repos() failed");
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

// ---------------------------------------------------------------------------
// /coda switch
// ---------------------------------------------------------------------------

/// Handles `/coda switch <branch>`.
///
/// Acquires a repository lock, validates the working directory is clean,
/// fetches the latest refs, checks out the target branch, and attempts
/// a fast-forward pull.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id, branch = %branch))]
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

    let start = Instant::now();
    let result = switch_branch(&repo_path, branch).await;
    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        branch,
        success = result.is_ok(),
        "switch_branch() completed"
    );

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

// ---------------------------------------------------------------------------
// Repo clone / update with live progress
// ---------------------------------------------------------------------------

/// Handles the repo clone-or-update action triggered from the select menu.
///
/// If the target path already contains a `.git` directory, the existing
/// clone is updated (fetch + checkout default branch + pull). Otherwise
/// a fresh clone is performed. In both cases the channel is auto-bound.
///
/// Posts an initial progress message immediately after acquiring the lock,
/// then streams step-by-step updates (including git transfer percentages)
/// via `chat.update`.
///
/// Acquires a repository lock for the duration of clone/update + bind
/// to prevent concurrent operations on the same path.
#[instrument(skip(state, message_ts), fields(channel = %channel_id, repo = %repo_name))]
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

    // Post initial progress message immediately
    let blocks = formatter::repo_sync_progress(repo_name, &[]);
    let _ = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await;

    // Create event channel
    let (tx, rx) = mpsc::unbounded_channel();
    let slack = state.slack().clone();
    let channel_owned = channel_id.to_string();
    let ts_owned = message_ts.to_string();
    let repo_name_owned = repo_name.to_string();

    // Spawn event consumer task
    let event_handle = tokio::spawn(consume_repo_sync_events(
        slack,
        channel_owned.clone(),
        ts_owned.clone(),
        repo_name_owned.clone(),
        rx,
    ));

    // Run clone or update with progress events
    let start = Instant::now();
    let is_existing = clone_path.join(".git").exists();
    let result = if is_existing {
        update_existing_with_progress(&clone_path, &tx).await
    } else {
        clone_fresh_with_progress(repo_name, &clone_path, &tx).await
    };

    // Signal finished and drop sender
    let _ = tx.send(RepoSyncEvent::Finished);
    drop(tx);

    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        repo_name,
        success = result.is_ok(),
        "repo sync completed"
    );

    // Wait for consumer to finish and get final step state
    let display_steps = match event_handle.await {
        Ok(steps) => steps,
        Err(e) => {
            warn!(error = %e, "Repo sync event consumer task panicked");
            Vec::new()
        }
    };

    // Build and post final message
    let final_blocks = match result {
        Ok(bind_label) => {
            // Auto-bind the channel
            match state.bindings().set(channel_id, clone_path.to_path_buf()) {
                Ok(()) => {
                    let label = format!("{bind_label} `{repo_name}` to `{}`", clone_path.display());
                    info!(channel_id, repo_name, "Repo sync succeeded, channel bound");
                    formatter::repo_sync_success(repo_name, &display_steps, &label)
                }
                Err(e) => {
                    let err = format!("Binding failed: {e}");
                    warn!(channel_id, repo_name, error = %err, "Channel binding failed");
                    formatter::repo_sync_failure(repo_name, &display_steps, &err)
                }
            }
        }
        Err(e) => {
            warn!(channel_id, repo_name, error = %e, "Repo sync failed");
            formatter::repo_sync_failure(repo_name, &display_steps, &e)
        }
    };

    let _ = state
        .slack()
        .update_message(channel_id, message_ts, final_blocks)
        .await;

    // Always release the lock
    state.repo_locks().unlock(&clone_path);
}

// ---------------------------------------------------------------------------
// Event consumer
// ---------------------------------------------------------------------------

/// Consumes repo sync events and updates the Slack progress message.
///
/// Step transitions (starting, completed, failed) trigger **immediate**
/// `chat.update` calls. Git progress lines are **debounced** using
/// [`STREAM_UPDATE_DEBOUNCE`] to avoid hitting Slack rate limits.
///
/// Returns the accumulated step display state for the final summary.
async fn consume_repo_sync_events(
    slack: SlackClient,
    channel: String,
    message_ts: String,
    repo_name: String,
    mut rx: mpsc::UnboundedReceiver<RepoSyncEvent>,
) -> Vec<RepoStepDisplay> {
    let mut display_steps: Vec<RepoStepDisplay> = Vec::new();
    let mut last_update = Instant::now() - STREAM_UPDATE_DEBOUNCE;
    let mut pending_update = false;

    while let Some(event) = rx.recv().await {
        let immediate = match event {
            RepoSyncEvent::Starting { steps } => {
                display_steps = steps
                    .into_iter()
                    .map(|label| RepoStepDisplay {
                        label,
                        status: PhaseDisplayStatus::Pending,
                        duration: None,
                        started_at: None,
                        detail: None,
                    })
                    .collect();
                true
            }
            RepoSyncEvent::StepStarting { index } => {
                if let Some(step) = display_steps.get_mut(index) {
                    step.status = PhaseDisplayStatus::Running;
                    step.started_at = Some(Instant::now());
                    step.detail = None;
                }
                true
            }
            RepoSyncEvent::StepCompleted { index, duration } => {
                if let Some(step) = display_steps.get_mut(index) {
                    step.status = PhaseDisplayStatus::Completed;
                    step.duration = Some(duration);
                    step.started_at = None;
                    step.detail = None;
                }
                true
            }
            RepoSyncEvent::StepFailed { index, error } => {
                if let Some(step) = display_steps.get_mut(index) {
                    if let Some(started) = step.started_at {
                        step.duration = Some(started.elapsed());
                    }
                    step.status = PhaseDisplayStatus::Failed;
                    step.started_at = None;
                    step.detail = Some(error);
                }
                true
            }
            RepoSyncEvent::GitProgress { index, line } => {
                if let Some(step) = display_steps.get_mut(index) {
                    step.detail = Some(line);
                }
                false // debounced
            }
            RepoSyncEvent::Finished => true,
        };

        if immediate {
            send_repo_update(&slack, &channel, &message_ts, &repo_name, &display_steps).await;
            last_update = Instant::now();
            pending_update = false;
        } else {
            pending_update = true;
            if last_update.elapsed() >= STREAM_UPDATE_DEBOUNCE {
                send_repo_update(&slack, &channel, &message_ts, &repo_name, &display_steps).await;
                last_update = Instant::now();
                pending_update = false;
            }
        }
    }

    // Flush any remaining pending update
    if pending_update {
        send_repo_update(&slack, &channel, &message_ts, &repo_name, &display_steps).await;
    }

    display_steps
}

/// Sends a progress update to Slack.
async fn send_repo_update(
    slack: &SlackClient,
    channel: &str,
    ts: &str,
    repo_name: &str,
    steps: &[RepoStepDisplay],
) {
    let blocks = formatter::repo_sync_progress(repo_name, steps);
    if let Err(e) = slack.update_message(channel, ts, blocks).await {
        warn!(error = %e, channel, "Failed to update repo sync progress message");
    }
}

// ---------------------------------------------------------------------------
// Clone with progress
// ---------------------------------------------------------------------------

/// Clones a fresh repository with step-by-step progress events.
///
/// Emits `Starting`, `StepStarting`, `GitProgress` lines from stderr,
/// and `StepCompleted` or `StepFailed`.
///
/// Returns the bind action label on success.
async fn clone_fresh_with_progress(
    repo_name: &str,
    clone_path: &Path,
    tx: &mpsc::UnboundedSender<RepoSyncEvent>,
) -> Result<String, String> {
    info!(
        repo_name,
        clone_path = %clone_path.display(),
        "Cloning repository with progress"
    );

    // Ensure parent directory exists
    if let Some(parent) = clone_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    let _ = tx.send(RepoSyncEvent::Starting {
        steps: vec!["Cloning".to_string()],
    });
    let _ = tx.send(RepoSyncEvent::StepStarting { index: 0 });

    let start = Instant::now();

    let mut child = tokio::process::Command::new("gh")
        .args([
            "repo",
            "clone",
            repo_name,
            &clone_path.display().to_string(),
            "--",
            "--progress",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run `gh repo clone`: {e}"))?;

    // Read progress from stderr
    if let Some(stderr) = child.stderr.take() {
        read_progress_lines(stderr, 0, tx).await;
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for `gh repo clone`: {e}"))?;

    let duration = start.elapsed();

    if status.success() {
        let _ = tx.send(RepoSyncEvent::StepCompleted { index: 0, duration });
        info!(
            repo_name,
            path = %clone_path.display(),
            "Repository cloned"
        );
        Ok("Cloned and bound".to_string())
    } else {
        let err = format!("`gh repo clone` exited with status {status}");
        let _ = tx.send(RepoSyncEvent::StepFailed {
            index: 0,
            error: err.clone(),
        });
        warn!(
            repo_name,
            error = %err,
            "Failed to clone repository"
        );
        Err(format!("Clone failed: {err}"))
    }
}

// ---------------------------------------------------------------------------
// Update with progress
// ---------------------------------------------------------------------------

/// Updates an existing repository clone with step-by-step progress events.
///
/// Steps: Fetching, Checking out, Pulling.
///
/// Returns the bind action label on success.
async fn update_existing_with_progress(
    clone_path: &Path,
    tx: &mpsc::UnboundedSender<RepoSyncEvent>,
) -> Result<String, String> {
    info!(
        clone_path = %clone_path.display(),
        "Updating existing repository clone with progress"
    );

    let _ = tx.send(RepoSyncEvent::Starting {
        steps: vec![
            "Fetching".to_string(),
            "Checking out".to_string(),
            "Pulling".to_string(),
        ],
    });

    // Step 0: Fetch
    let _ = tx.send(RepoSyncEvent::StepStarting { index: 0 });
    let fetch_start = Instant::now();

    let mut fetch_child = tokio::process::Command::new("git")
        .args(["fetch", "origin", "--progress"])
        .current_dir(clone_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run `git fetch`: {e}"))?;

    if let Some(stderr) = fetch_child.stderr.take() {
        read_progress_lines(stderr, 0, tx).await;
    }

    let fetch_status = fetch_child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for `git fetch`: {e}"))?;

    if !fetch_status.success() {
        let err = format!("`git fetch origin` exited with status {fetch_status}");
        let _ = tx.send(RepoSyncEvent::StepFailed {
            index: 0,
            error: err.clone(),
        });
        return Err(err);
    }
    let _ = tx.send(RepoSyncEvent::StepCompleted {
        index: 0,
        duration: fetch_start.elapsed(),
    });

    // Detect default branch (fast, local-only)
    let branch = detect_default_branch(clone_path);
    debug!(branch, path = %clone_path.display(), "Detected default branch");

    // Step 1: Checkout
    let _ = tx.send(RepoSyncEvent::StepStarting { index: 1 });
    let checkout_start = Instant::now();

    let checkout_output = tokio::process::Command::new("git")
        .args(["checkout", "--force", &branch])
        .current_dir(clone_path)
        .output()
        .await
        .map_err(|e| format!("Failed to run `git checkout`: {e}"))?;

    if !checkout_output.status.success() {
        let stderr = String::from_utf8_lossy(&checkout_output.stderr);
        let err = format!("`git checkout --force {branch}` failed: {stderr}");
        let _ = tx.send(RepoSyncEvent::StepFailed {
            index: 1,
            error: err.clone(),
        });
        return Err(err);
    }
    let _ = tx.send(RepoSyncEvent::StepCompleted {
        index: 1,
        duration: checkout_start.elapsed(),
    });

    // Step 2: Pull
    let _ = tx.send(RepoSyncEvent::StepStarting { index: 2 });
    let pull_start = Instant::now();

    let pull_ff = tokio::process::Command::new("git")
        .args(["pull", "--ff-only"])
        .current_dir(clone_path)
        .output()
        .await
        .map_err(|e| format!("Failed to run `git pull --ff-only`: {e}"))?;

    if !pull_ff.status.success() {
        debug!(
            path = %clone_path.display(),
            "Fast-forward pull failed, falling back to rebase"
        );
        let pull_rebase = tokio::process::Command::new("git")
            .args(["pull", "--rebase"])
            .current_dir(clone_path)
            .output()
            .await
            .map_err(|e| format!("Failed to run `git pull --rebase`: {e}"))?;

        if !pull_rebase.status.success() {
            let stderr = String::from_utf8_lossy(&pull_rebase.stderr);
            let err = format!("`git pull --rebase` failed: {stderr}");
            let _ = tx.send(RepoSyncEvent::StepFailed {
                index: 2,
                error: err.clone(),
            });
            return Err(err);
        }
    }
    let _ = tx.send(RepoSyncEvent::StepCompleted {
        index: 2,
        duration: pull_start.elapsed(),
    });

    info!(
        clone_path = %clone_path.display(),
        branch,
        "Repository updated"
    );
    Ok(format!("Updated and bound (branch `{branch}`)"))
}

// ---------------------------------------------------------------------------
// Git progress line parsing
// ---------------------------------------------------------------------------

/// Parses a raw git progress line into a human-readable detail string.
///
/// Returns `Some(detail)` for lines containing a percentage or `done.`,
/// and `None` for unrecognized lines (e.g. "Cloning into '...'").
fn parse_git_progress_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip "remote: " prefix that git prepends to server-side messages
    let cleaned = trimmed.strip_prefix("remote: ").unwrap_or(trimmed);

    if cleaned.contains('%') || cleaned.ends_with("done.") {
        Some(cleaned.to_string())
    } else {
        None
    }
}

/// Reads git progress lines from a stderr stream and sends them as events.
///
/// Git writes progress updates using `\r` for in-place overwriting and
/// `\n` for completed lines. This reader handles both delimiters.
async fn read_progress_lines(
    stderr: tokio::process::ChildStderr,
    step_index: usize,
    tx: &mpsc::UnboundedSender<RepoSyncEvent>,
) {
    let reader = BufReader::new(stderr);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        // A single line may contain \r-separated progress updates
        for segment in line.split('\r') {
            if let Some(detail) = parse_git_progress_line(segment) {
                let _ = tx.send(RepoSyncEvent::GitProgress {
                    index: step_index,
                    line: detail,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // build_clone_path tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // detect_default_branch tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // parse_git_progress_line tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_should_parse_percentage_line() {
        let result = parse_git_progress_line("Receiving objects:  45% (556/1234)");
        assert_eq!(
            result.as_deref(),
            Some("Receiving objects:  45% (556/1234)")
        );
    }

    #[test]
    fn test_should_strip_remote_prefix() {
        let result = parse_git_progress_line("remote: Compressing objects: 100% (42/42), done.");
        assert_eq!(
            result.as_deref(),
            Some("Compressing objects: 100% (42/42), done.")
        );
    }

    #[test]
    fn test_should_parse_done_line() {
        let result = parse_git_progress_line("remote: Counting objects: 1234, done.");
        assert_eq!(result.as_deref(), Some("Counting objects: 1234, done."));
    }

    #[test]
    fn test_should_return_none_for_cloning_into() {
        let result = parse_git_progress_line("Cloning into '/path/to/repo'...");
        assert!(result.is_none());
    }

    #[test]
    fn test_should_return_none_for_empty_string() {
        let result = parse_git_progress_line("");
        assert!(result.is_none());
    }

    #[test]
    fn test_should_return_none_for_whitespace_only() {
        let result = parse_git_progress_line("   ");
        assert!(result.is_none());
    }

    #[test]
    fn test_should_parse_resolving_deltas() {
        let result = parse_git_progress_line("Resolving deltas:  78% (123/158)");
        assert_eq!(result.as_deref(), Some("Resolving deltas:  78% (123/158)"));
    }
}
