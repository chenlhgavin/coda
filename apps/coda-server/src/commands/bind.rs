//! `/coda bind` and `/coda unbind` command handlers.
//!
//! Manages channel-to-repository bindings. `bind` associates the current
//! Slack channel with a local repository path. `unbind` removes the
//! association.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::info;

use crate::error::ServerError;
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

/// Handles `/coda bind <repo_path>`.
///
/// Validates the path and creates a channel-repo binding. Posts a
/// confirmation or error message to the channel.
///
/// # Errors
///
/// Returns `ServerError` if the binding fails or the Slack API call fails.
pub async fn handle_bind(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    repo_path: &str,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;
    let path = PathBuf::from(repo_path);

    state.bindings().set(channel, path)?;

    info!(
        channel,
        repo_path,
        user = payload.user_name,
        "Channel bound to repository"
    );

    let blocks = vec![serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": format!(
                ":white_check_mark: Channel bound to `{repo_path}`\nYou can now use `/coda list`, `/coda plan`, `/coda run`, and other commands."
            )
        }
    })];

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}

/// Handles `/coda unbind`.
///
/// Removes the channel-repo binding if one exists. Posts a confirmation
/// or informational message.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails.
pub async fn handle_unbind(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;
    let removed = state.bindings().remove(channel)?;

    let text = if removed {
        info!(
            channel,
            user = payload.user_name,
            "Channel unbound from repository"
        );
        ":white_check_mark: Channel unbound. Use `/coda bind <path>` to bind a new repository."
            .to_string()
    } else {
        "This channel is not bound to any repository. Use `/coda bind <path>` to bind one."
            .to_string()
    };

    let blocks = vec![serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": text
        }
    })];

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}
