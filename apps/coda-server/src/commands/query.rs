//! `/coda help` command handler.
//!
//! Displays available commands and usage information.

use std::sync::Arc;

use crate::error::ServerError;
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

/// Help text listing all available commands.
const HELP_TEXT: &str = "\
*CODA — AI-Driven Development Agent*

*Channel Setup*
  `/coda bind <path>` — Bind this channel to a local repository
  `/coda unbind` — Remove the channel binding

*Workflow Commands*
  `/coda init` — Initialize the bound repo as a CODA project
  `/coda plan <slug>` — Start an interactive planning session in a thread
  `/coda run <slug>` — Execute a feature development run with live progress

*Query Commands*
  `/coda list` — List all features in the bound repository
  `/coda status <slug>` — Show detailed status of a feature
  `/coda clean` — Clean up merged worktrees

*Other*
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_contain_all_commands_in_help_text() {
        assert!(HELP_TEXT.contains("/coda bind"));
        assert!(HELP_TEXT.contains("/coda unbind"));
        assert!(HELP_TEXT.contains("/coda init"));
        assert!(HELP_TEXT.contains("/coda plan"));
        assert!(HELP_TEXT.contains("/coda run"));
        assert!(HELP_TEXT.contains("/coda list"));
        assert!(HELP_TEXT.contains("/coda status"));
        assert!(HELP_TEXT.contains("/coda clean"));
        assert!(HELP_TEXT.contains("/coda help"));
    }
}
