//! Query command handlers: `/coda help`, `/coda list`, `/coda status`, `/coda clean`.
//!
//! These commands read state from `coda-core` and post formatted results
//! back to Slack. None of them perform mutations beyond `clean`, which
//! delegates to the interaction handler after user confirmation.

use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, instrument};

use crate::error::ServerError;
use crate::formatter;
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

use super::resolve_engine;

/// Help text listing all available commands.
const HELP_TEXT: &str = "\
*CODA — AI-Driven Development Agent*

*Channel Setup*
  `/coda repos` — List your GitHub repos, select one to clone and bind
  `/coda switch <branch>` — Switch the bound repository to a different branch

*Workflow Commands*
  `/coda init [--force]` — Initialize (or reinitialize) the bound repo as a CODA project
  `/coda plan <slug>` — Start an interactive planning session in a thread
  `/coda run <slug>` — Execute a feature development run with live progress

*Query Commands*
  `/coda list` — List all features in the bound repository
  `/coda status <slug>` — Show detailed status of a feature
  `/coda clean` — Clean up merged worktrees

*Configuration*
  `/coda config` — Show resolved agent configuration for all operations
  `/coda config get <key>` — Get a config value (e.g., `agents.run.model`)
  `/coda config set <key> <value>` — Set a config value

*Other*
  `/coda cancel [slug]` — Cancel a running init or run task
  `/coda help` — Show this help message";

/// Handles `/coda help`.
///
/// Posts the help text listing all available commands.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails.
pub async fn handle_help(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
) -> Result<(), ServerError> {
    let blocks = vec![serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": HELP_TEXT
        }
    })];

    state
        .slack()
        .post_message(&payload.channel_id, blocks)
        .await?;
    Ok(())
}

/// Handles `/coda list`.
///
/// Resolves the channel binding, creates an Engine, lists all features,
/// and posts a formatted feature list to the channel.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails or the Engine
/// cannot be created.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id))]
pub async fn handle_list(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some((repo_path, engine)) = resolve_engine(state.as_ref(), channel).await? else {
        return Ok(());
    };

    info!(channel, "Listing features");

    let start = Instant::now();
    let result = engine.list_features();
    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        success = result.is_ok(),
        "engine.list_features() completed"
    );

    let blocks = match result {
        Ok(features) if features.is_empty() => formatter::empty_feature_list(&repo_path),
        Ok(features) => formatter::feature_list(&repo_path, &features),
        Err(e) => formatter::error(&e.to_string()),
    };

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}

/// Handles `/coda status <feature_slug>`.
///
/// Resolves the channel binding, creates an Engine, queries feature
/// status, and posts a detailed status view to the channel.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails or the Engine
/// cannot be created.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id, slug = %feature_slug))]
pub async fn handle_status(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    feature_slug: &str,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some((_repo_path, engine)) = resolve_engine(state.as_ref(), channel).await? else {
        return Ok(());
    };

    info!(channel, feature_slug, "Querying feature status");

    let start = Instant::now();
    let result = engine.feature_status(feature_slug);
    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        feature_slug,
        success = result.is_ok(),
        "engine.feature_status() completed"
    );

    let blocks = match result {
        Ok(feature_state) => formatter::feature_status(&feature_state),
        Err(e) => formatter::error(&e.to_string()),
    };

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}

/// Handles `/coda clean`.
///
/// Resolves the channel binding, creates an Engine, scans for cleanable
/// worktrees (merged/closed PRs), and posts candidates with a confirm
/// button. The actual removal is handled by the interaction handler
/// when the user clicks the button.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails or the Engine
/// cannot be created.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id))]
pub async fn handle_clean(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some((_repo_path, engine)) = resolve_engine(state.as_ref(), channel).await? else {
        return Ok(());
    };

    info!(channel, "Scanning for cleanable worktrees");

    let start = Instant::now();
    let result = engine.scan_cleanable_worktrees();
    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        success = result.is_ok(),
        "engine.scan_cleanable_worktrees() completed"
    );

    let blocks = match result {
        Ok(candidates) if candidates.is_empty() => formatter::no_cleanable_worktrees(),
        Ok(candidates) => {
            info!(
                channel,
                count = candidates.len(),
                "Found cleanable worktrees"
            );
            formatter::clean_candidates(&candidates)
        }
        Err(e) => formatter::error(&e.to_string()),
    };

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}

/// Handles `/coda cancel [target]`.
///
/// Resolves the channel binding, searches for running tasks that match
/// the bound repository (and optional target slug), and cancels them.
///
/// If `target` is empty, cancels all running tasks for the bound repo.
/// Otherwise, looks for a matching `run:{repo}:{target}` or
/// `init:{repo}` task key.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id))]
pub async fn handle_cancel(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    target: &str,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some(repo_path) = state.bindings().get(channel) else {
        let blocks = formatter::error(
            "No repository bound to this channel. Use `/coda repos` to bind a repository first.",
        );
        state.slack().post_message(channel, blocks).await?;
        return Ok(());
    };

    let repo_display = repo_path.display().to_string();
    let running = state.running_tasks().running_keys();

    let mut cancelled = Vec::new();
    for key in &running {
        // Match tasks belonging to this repo
        let belongs_to_repo = key.ends_with(&format!(":{repo_display}"))
            || key.contains(&format!(":{repo_display}:"));

        if !belongs_to_repo {
            continue;
        }

        // If a specific target is given, only cancel matching tasks
        if !target.is_empty() && !key.ends_with(&format!(":{target}")) {
            continue;
        }

        if state.running_tasks().cancel(key) {
            cancelled.push(key.clone());
        }
    }

    let blocks = if cancelled.is_empty() {
        if target.is_empty() {
            formatter::error("No running tasks found for this repository.")
        } else {
            formatter::error(&format!("No running task found matching `{target}`."))
        }
    } else {
        let names: Vec<&str> = cancelled.iter().map(String::as_str).collect();
        info!(channel, tasks = ?names, "Cancelled tasks");
        let msg = if cancelled.len() == 1 {
            format!(":octagonal_sign: Cancelled task: `{}`", cancelled[0])
        } else {
            let list = cancelled
                .iter()
                .map(|k| format!("  \u{2022} `{k}`"))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                ":octagonal_sign: Cancelled {} tasks:\n{list}",
                cancelled.len()
            )
        };
        vec![serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": msg }
        })]
    };

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_contain_all_commands_in_help_text() {
        assert!(HELP_TEXT.contains("/coda init"));
        assert!(HELP_TEXT.contains("/coda plan"));
        assert!(HELP_TEXT.contains("/coda run"));
        assert!(HELP_TEXT.contains("/coda repos"));
        assert!(HELP_TEXT.contains("/coda switch"));
        assert!(HELP_TEXT.contains("/coda list"));
        assert!(HELP_TEXT.contains("/coda status"));
        assert!(HELP_TEXT.contains("/coda clean"));
        assert!(HELP_TEXT.contains("/coda cancel"));
        assert!(HELP_TEXT.contains("/coda config"));
        assert!(HELP_TEXT.contains("/coda help"));
    }
}
