//! Interactive component handler for Slack block actions.
//!
//! Routes button clicks and other interactive components to the appropriate
//! command handler. Currently handles the clean confirm button from
//! [`formatter::clean_candidates`](crate::formatter::clean_candidates).

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info, instrument, warn};

use crate::commands;
use crate::formatter::{self, CLEAN_CONFIRM_ACTION, CleanTarget, REPO_SELECT_ACTION};
use crate::state::AppState;

/// Channel reference from an interaction payload.
///
/// # Examples
///
/// ```
/// use serde_json::json;
///
/// let v = json!({"id": "C123", "name": "general"});
/// // Deserializes to ChannelRef { id: "C123" }
/// ```
#[derive(Debug, Deserialize)]
struct ChannelRef {
    id: String,
}

/// Message reference from an interaction payload.
#[derive(Debug, Deserialize)]
struct MessageRef {
    ts: String,
}

/// A selected option from a `static_select` element.
#[derive(Debug, Deserialize)]
struct SelectedOption {
    value: String,
}

/// A single action from a `block_actions` interaction.
#[derive(Debug, Deserialize)]
struct Action {
    action_id: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    selected_option: Option<SelectedOption>,
}

/// Top-level interactive payload from Slack Socket Mode.
///
/// Subset of fields needed for routing and handling clean confirmations.
#[derive(Debug, Deserialize)]
struct InteractionPayload {
    #[serde(default)]
    actions: Vec<Action>,
    channel: ChannelRef,
    message: MessageRef,
}

/// Handles an interactive component payload (e.g., button click).
///
/// Parses the payload, routes by `action_id`, and delegates to the
/// appropriate handler. Unknown actions are silently ignored.
#[instrument(skip(state, payload), fields(channel_id, action_ids))]
pub async fn handle_interaction(state: Arc<AppState>, payload: serde_json::Value) {
    let interaction: InteractionPayload = match serde_json::from_value(payload) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse interaction payload");
            return;
        }
    };

    // Record parsed context onto the current span
    let span = tracing::Span::current();
    span.record("channel_id", interaction.channel.id.as_str());
    let action_ids: String = interaction
        .actions
        .iter()
        .map(|a| a.action_id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    span.record("action_ids", action_ids.as_str());

    for action in &interaction.actions {
        if action.action_id == CLEAN_CONFIRM_ACTION {
            handle_clean_confirm(
                &state,
                &interaction.channel.id,
                &interaction.message.ts,
                action.value.as_deref(),
            )
            .await;
        } else if action.action_id == REPO_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                commands::repos::handle_repo_clone(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Repo select action received without selected_option");
            }
        } else {
            debug!(action_id = %action.action_id, "Unknown interaction action, ignoring");
        }
    }
}

/// Handles the clean confirm button click.
///
/// Deserializes the candidates from the button value, resolves the
/// channel binding, creates an Engine, and removes the worktrees.
/// Updates the original message with the result or error.
async fn handle_clean_confirm(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    value: Option<&str>,
) {
    let blocks = match execute_clean(state, channel_id, value).await {
        Ok(removed) => {
            info!(
                channel_id,
                count = removed.len(),
                "Clean confirm: removed worktrees"
            );
            formatter::clean_result(&removed)
        }
        Err(msg) => {
            warn!(channel_id, error = %msg, "Clean confirm failed");
            formatter::error(&msg)
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update clean confirmation message");
    }
}

/// Executes the clean operation: deserializes candidates, resolves
/// binding, acquires repo lock, creates Engine, removes worktrees.
///
/// Returns a user-friendly error message on failure.
async fn execute_clean(
    state: &AppState,
    channel_id: &str,
    value: Option<&str>,
) -> Result<Vec<coda_core::CleanedWorktree>, String> {
    let value = value.ok_or("No candidate data in button value")?;

    let targets: Vec<CleanTarget> =
        serde_json::from_str(value).map_err(|e| format!("Failed to parse clean targets: {e}"))?;

    if targets.is_empty() {
        return Err("No candidates to clean".to_string());
    }

    let repo_path = state.bindings().get(channel_id).ok_or(
        "Channel binding was removed. Use `/coda repos` to clone and bind a repository first.",
    )?;

    // Acquire repo lock for worktree removal
    state
        .repo_locks()
        .try_lock(&repo_path, "clean")
        .map_err(|holder| format!("Repository is busy (`{holder}`). Try again later."))?;

    let result = async {
        let engine = coda_core::Engine::new(repo_path.clone())
            .await
            .map_err(|e| format!("Failed to initialize engine: {e}"))?;

        let candidates: Vec<coda_core::CleanedWorktree> =
            targets.into_iter().map(Into::into).collect();

        engine
            .remove_worktrees(&candidates)
            .map_err(|e| format!("Failed to remove worktrees: {e}"))
    }
    .await;

    // Release repo lock
    state.repo_locks().unlock(&repo_path);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_deserialize_interaction_payload() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": "coda_clean_confirm",
                "value": "[{\"slug\":\"add-auth\",\"branch\":\"feature/add-auth\",\"pr_number\":42,\"pr_state\":\"MERGED\"}]",
                "type": "button"
            }],
            "channel": { "id": "C123", "name": "general" },
            "user": { "id": "U123", "username": "testuser" },
            "message": { "ts": "1234567890.123456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.channel.id, "C123");
        assert_eq!(payload.message.ts, "1234567890.123456");
        assert_eq!(payload.actions.len(), 1);
        assert_eq!(payload.actions[0].action_id, CLEAN_CONFIRM_ACTION);
    }

    #[test]
    fn test_should_deserialize_payload_with_no_actions() {
        let json = serde_json::json!({
            "type": "block_actions",
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        assert!(payload.actions.is_empty());
    }

    #[test]
    fn test_should_deserialize_action_without_value() {
        let json = serde_json::json!({
            "action_id": "some_action",
            "type": "button"
        });

        let action: Action = serde_json::from_value(json).unwrap();
        assert_eq!(action.action_id, "some_action");
        assert!(action.value.is_none());
        assert!(action.selected_option.is_none());
    }

    #[test]
    fn test_should_deserialize_action_with_selected_option() {
        let json = serde_json::json!({
            "action_id": REPO_SELECT_ACTION,
            "type": "static_select",
            "selected_option": {
                "text": { "type": "plain_text", "text": "org/repo" },
                "value": "org/repo"
            }
        });

        let action: Action = serde_json::from_value(json).unwrap();
        assert_eq!(action.action_id, REPO_SELECT_ACTION);
        let selected = action.selected_option.expect("selected_option");
        assert_eq!(selected.value, "org/repo");
    }

    #[test]
    fn test_should_parse_clean_targets_from_button_value() {
        let targets = vec![CleanTarget {
            slug: "add-auth".to_string(),
            branch: "feature/add-auth".to_string(),
            pr_number: Some(42),
            pr_state: "MERGED".to_string(),
        }];
        let json = serde_json::to_string(&targets).unwrap();
        let parsed: Vec<CleanTarget> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].slug, "add-auth");
    }
}
