//! Interactive component handler for Slack block actions.
//!
//! Routes button clicks and other interactive components to the appropriate
//! command handler. Currently handles the clean confirm button from
//! [`formatter::clean_candidates`](crate::formatter::clean_candidates),
//! config key/value dropdowns, and the init config modification flow
//! (operation → backend → model → effort selection).

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info, instrument, warn};

use crate::commands;
use crate::formatter::{
    self, CLEAN_CONFIRM_ACTION, CONFIG_KEY_SELECT_ACTION, CONFIG_VALUE_SELECT_ACTION, CleanTarget,
    INIT_BACKEND_SELECT_ACTION, INIT_EFFORT_SELECT_ACTION, INIT_MODEL_SELECT_ACTION,
    INIT_MODIFY_ACTION, INIT_OP_SELECT_ACTION, INIT_START_ACTION, REPO_SELECT_ACTION,
};
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
        } else if action.action_id == CONFIG_KEY_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                handle_config_key_select(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Config key select action received without selected_option");
            }
        } else if action.action_id == CONFIG_VALUE_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                handle_config_value_select(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Config value select action received without selected_option");
            }
        } else if action.action_id == INIT_START_ACTION {
            handle_init_start(
                Arc::clone(&state),
                &interaction.channel.id,
                &interaction.message.ts,
                action.value.as_deref(),
            )
            .await;
        } else if action.action_id == INIT_MODIFY_ACTION {
            handle_init_modify(
                &state,
                &interaction.channel.id,
                &interaction.message.ts,
                action.value.as_deref(),
            )
            .await;
        } else if action.action_id == INIT_OP_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                handle_init_op_select(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Init op select action received without selected_option");
            }
        } else if action.action_id == INIT_BACKEND_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                handle_init_backend_select(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Init backend select action received without selected_option");
            }
        } else if action.action_id == INIT_MODEL_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                handle_init_model_select(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Init model select action received without selected_option");
            }
        } else if action.action_id == INIT_EFFORT_SELECT_ACTION {
            if let Some(ref selected) = action.selected_option {
                handle_init_effort_select(
                    &state,
                    &interaction.channel.id,
                    &interaction.message.ts,
                    &selected.value,
                )
                .await;
            } else {
                debug!("Init effort select action received without selected_option");
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

/// Handles config key selection: finds the descriptor for the selected
/// key and updates the message with the value picker.
async fn handle_config_key_select(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    selected_key: &str,
) {
    let blocks = match resolve_config_descriptor(state, channel_id, selected_key).await {
        Ok(descriptor) => {
            info!(channel_id, key = selected_key, "Config key selected");
            formatter::config_value_select(&descriptor)
        }
        Err(msg) => {
            warn!(channel_id, error = %msg, "Config key select failed");
            formatter::error(&msg)
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update config key select message");
    }
}

/// Handles config value selection: parses `key=value` from the option,
/// applies the config change, and updates the message with the result.
async fn handle_config_value_select(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    encoded_value: &str,
) {
    let blocks = match execute_config_set(state, channel_id, encoded_value).await {
        Ok((key, value)) => {
            info!(channel_id, key = %key, value = %value, "Config value set via interaction");
            vec![serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(":white_check_mark: Updated `{key}` = `{value}`")
                }
            })]
        }
        Err(msg) => {
            warn!(channel_id, error = %msg, "Config value select failed");
            formatter::error(&msg)
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update config value select message");
    }
}

/// Resolves the config descriptor for a selected key by creating an
/// Engine from the channel binding.
async fn resolve_config_descriptor(
    state: &AppState,
    channel_id: &str,
    key: &str,
) -> Result<coda_core::ConfigKeyDescriptor, String> {
    let repo_path = state.bindings().get(channel_id).ok_or(
        "Channel binding was removed. Use `/coda repos` to clone and bind a repository first.",
    )?;

    let engine = coda_core::Engine::new(repo_path)
        .await
        .map_err(|e| format!("Failed to initialize engine: {e}"))?;

    let schema = engine.config_schema();
    schema
        .into_iter()
        .find(|d| d.key == key)
        .ok_or_else(|| format!("Unknown config key: `{key}`"))
}

/// Parses `key=value` from the encoded option, creates an Engine, and
/// applies the config change.
async fn execute_config_set(
    state: &AppState,
    channel_id: &str,
    encoded: &str,
) -> Result<(String, String), String> {
    let (key, value) = encoded
        .split_once('=')
        .ok_or_else(|| format!("Invalid config value encoding: `{encoded}`"))?;

    let repo_path = state.bindings().get(channel_id).ok_or(
        "Channel binding was removed. Use `/coda repos` to clone and bind a repository first.",
    )?;

    let engine = coda_core::Engine::new(repo_path)
        .await
        .map_err(|e| format!("Failed to initialize engine: {e}"))?;

    engine
        .config_set(key, value)
        .map_err(|e| format!("Failed to set config: {e}"))?;

    Ok((key.to_string(), value.to_string()))
}

// ---------------------------------------------------------------------------
// Init config modification flow handlers
// ---------------------------------------------------------------------------

/// Parses a `|`-delimited value string into its segments.
///
/// Returns `None` if the number of segments doesn't match `expected_count`.
fn parse_pipe_segments(value: &str, expected_count: usize) -> Option<Vec<&str>> {
    let segments: Vec<&str> = value.split('|').collect();
    if segments.len() == expected_count {
        Some(segments)
    } else {
        None
    }
}

/// Parses the `force` flag from a string segment (`"true"` / `"false"`).
fn parse_force(s: &str) -> bool {
    s == "true"
}

/// Creates an Engine from the channel binding, returning a user-friendly error.
async fn resolve_engine_for_channel(
    state: &AppState,
    channel_id: &str,
) -> Result<coda_core::Engine, String> {
    let repo_path = state.bindings().get(channel_id).ok_or(
        "Channel binding was removed. Use `/coda repos` to clone and bind a repository first.",
    )?;

    coda_core::Engine::new(repo_path)
        .await
        .map_err(|e| format!("Failed to initialize engine: {e}"))
}

/// Handles the "Start Init" button click.
///
/// Parses the `force` flag from the button value and delegates to
/// [`commands::init::start_init`] to launch the init pipeline.
async fn handle_init_start(
    state: Arc<AppState>,
    channel_id: &str,
    message_ts: &str,
    value: Option<&str>,
) {
    let force = value.is_some_and(parse_force);

    if let Err(e) = commands::init::start_init(state, channel_id, message_ts, force).await {
        warn!(channel_id, error = %e, "Init start failed");
    }
}

/// Handles the "Modify Settings" button click.
///
/// Shows the operation select dropdown so the user can pick which
/// operation to configure.
async fn handle_init_modify(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    value: Option<&str>,
) {
    let force = value.is_some_and(parse_force);

    let blocks = match resolve_engine_for_channel(state, channel_id).await {
        Ok(engine) => {
            let summaries = engine.config().operation_summaries();
            info!(channel_id, "Showing init operation select");
            formatter::init_config_op_select(&summaries, force)
        }
        Err(msg) => {
            warn!(channel_id, error = %msg, "Init modify failed");
            formatter::error(&msg)
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update init modify message");
    }
}

/// Handles operation selection in the init config flow.
///
/// Parses `"{op}|{force}"` from the dropdown value and shows the
/// backend select dropdown for the chosen operation.
async fn handle_init_op_select(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    selected_value: &str,
) {
    let blocks = match parse_pipe_segments(selected_value, 2) {
        Some(segments) => {
            let op = segments[0];
            let force = parse_force(segments[1]);

            match resolve_engine_for_channel(state, channel_id).await {
                Ok(engine) => {
                    let summaries = engine.config().operation_summaries();
                    if let Some(op_summary) = summaries.iter().find(|s| s.name == op) {
                        info!(channel_id, op, "Showing init backend select");
                        formatter::init_config_backend_select(
                            op,
                            &op_summary.backend_options,
                            &op_summary.backend,
                            force,
                        )
                    } else {
                        formatter::error(&format!("Unknown operation: `{op}`"))
                    }
                }
                Err(msg) => formatter::error(&msg),
            }
        }
        None => formatter::error(&format!(
            "Invalid operation select value: `{selected_value}`"
        )),
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update init op select message");
    }
}

/// Handles backend selection in the init config flow.
///
/// Parses `"{op}|{backend}|{force}"` from the dropdown value and shows
/// the model select dropdown with suggestions for the chosen backend.
async fn handle_init_backend_select(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    selected_value: &str,
) {
    let blocks = match parse_pipe_segments(selected_value, 3) {
        Some(segments) => {
            let op = segments[0];
            let backend = segments[1];
            let force = parse_force(segments[2]);

            let suggestions = coda_core::config::model_suggestions_for_backend(backend);

            match resolve_engine_for_channel(state, channel_id).await {
                Ok(engine) => {
                    let summaries = engine.config().operation_summaries();
                    let current_model = summaries
                        .iter()
                        .find(|s| s.name == op)
                        .map_or("", |s| s.model.as_str());

                    info!(channel_id, op, backend, "Showing init model select");
                    formatter::init_config_model_select(
                        op,
                        backend,
                        &suggestions,
                        current_model,
                        force,
                    )
                }
                Err(msg) => formatter::error(&msg),
            }
        }
        None => formatter::error(&format!("Invalid backend select value: `{selected_value}`")),
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update init backend select message");
    }
}

/// Handles model selection in the init config flow.
///
/// Parses `"{op}|{backend}|{model}|{force}"` from the dropdown value
/// and shows the effort select dropdown.
async fn handle_init_model_select(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    selected_value: &str,
) {
    let blocks = match parse_pipe_segments(selected_value, 4) {
        Some(segments) => {
            let op = segments[0];
            let backend = segments[1];
            let model = segments[2];
            let force = parse_force(segments[3]);

            match resolve_engine_for_channel(state, channel_id).await {
                Ok(engine) => {
                    let summaries = engine.config().operation_summaries();
                    let op_summary = summaries.iter().find(|s| s.name == op);

                    let effort_options = op_summary
                        .map(|s| s.effort_options.as_slice())
                        .unwrap_or(&[]);
                    let current_effort = op_summary.map_or("", |s| s.effort.as_str());

                    info!(channel_id, op, backend, model, "Showing init effort select");
                    formatter::init_config_effort_select(
                        op,
                        backend,
                        model,
                        effort_options,
                        current_effort,
                        force,
                    )
                }
                Err(msg) => formatter::error(&msg),
            }
        }
        None => formatter::error(&format!("Invalid model select value: `{selected_value}`")),
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update init model select message");
    }
}

/// Handles effort selection in the init config flow — the final step.
///
/// Parses `"{op}|{backend}|{model}|{effort}|{force}"` from the dropdown
/// value, applies all three config changes atomically, reloads the
/// engine config, and shows the updated config preview.
async fn handle_init_effort_select(
    state: &AppState,
    channel_id: &str,
    message_ts: &str,
    selected_value: &str,
) {
    let blocks = match parse_pipe_segments(selected_value, 5) {
        Some(segments) => {
            let op = segments[0];
            let backend = segments[1];
            let model = segments[2];
            let effort = segments[3];
            let force = parse_force(segments[4]);

            match apply_init_config(state, channel_id, op, backend, model, effort).await {
                Ok(summary) => {
                    info!(
                        channel_id,
                        op, backend, model, effort, "Init config applied, showing updated preview"
                    );
                    formatter::init_config_preview(&summary, force)
                }
                Err(msg) => formatter::error(&msg),
            }
        }
        None => formatter::error(&format!("Invalid effort select value: `{selected_value}`")),
    };

    if let Err(e) = state
        .slack()
        .update_message(channel_id, message_ts, blocks)
        .await
    {
        warn!(error = %e, channel_id, "Failed to update init effort select message");
    }
}

/// Applies backend, model, and effort config for a single operation,
/// then reloads the config and returns the updated summary.
async fn apply_init_config(
    state: &AppState,
    channel_id: &str,
    op: &str,
    backend: &str,
    model: &str,
    effort: &str,
) -> Result<coda_core::ResolvedConfigSummary, String> {
    let repo_path = state.bindings().get(channel_id).ok_or(
        "Channel binding was removed. Use `/coda repos` to clone and bind a repository first.",
    )?;

    let mut engine = coda_core::Engine::new(repo_path)
        .await
        .map_err(|e| format!("Failed to initialize engine: {e}"))?;

    // Apply all three config keys atomically (on disk)
    let pairs = [
        (format!("agents.{op}.backend"), backend),
        (format!("agents.{op}.model"), model),
        (format!("agents.{op}.effort"), effort),
    ];

    for (key, value) in &pairs {
        engine
            .config_set(key, value)
            .map_err(|e| format!("Failed to set `{key}`: {e}"))?;
    }

    // Reload in-memory config so the preview reflects the changes
    engine
        .reload_config()
        .map_err(|e| format!("Failed to reload config: {e}"))?;

    Ok(engine.config_show())
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
            pr_state: coda_core::PrState::Merged,
        }];
        let json = serde_json::to_string(&targets).unwrap();
        let parsed: Vec<CleanTarget> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].slug, "add-auth");
    }

    #[test]
    fn test_should_deserialize_config_key_select_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": CONFIG_KEY_SELECT_ACTION,
                "type": "static_select",
                "selected_option": {
                    "text": { "type": "plain_text", "text": "agents.run.backend" },
                    "value": "agents.run.backend"
                }
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.actions.len(), 1);
        assert_eq!(payload.actions[0].action_id, CONFIG_KEY_SELECT_ACTION);
        let selected = payload.actions[0]
            .selected_option
            .as_ref()
            .expect("selected_option");
        assert_eq!(selected.value, "agents.run.backend");
    }

    #[test]
    fn test_should_deserialize_config_value_select_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": CONFIG_VALUE_SELECT_ACTION,
                "type": "static_select",
                "selected_option": {
                    "text": { "type": "plain_text", "text": "codex" },
                    "value": "agents.run.backend=codex"
                }
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.actions.len(), 1);
        assert_eq!(payload.actions[0].action_id, CONFIG_VALUE_SELECT_ACTION);
        let selected = payload.actions[0]
            .selected_option
            .as_ref()
            .expect("selected_option");
        // Verify key=value encoding
        let (key, value) = selected.value.split_once('=').expect("key=value split");
        assert_eq!(key, "agents.run.backend");
        assert_eq!(value, "codex");
    }

    // -----------------------------------------------------------------------
    // Init config flow tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_should_parse_pipe_segments_with_correct_count() {
        let segments = parse_pipe_segments("init|true", 2).unwrap();
        assert_eq!(segments, vec!["init", "true"]);
    }

    #[test]
    fn test_should_reject_pipe_segments_with_wrong_count() {
        assert!(parse_pipe_segments("init|true", 3).is_none());
        assert!(parse_pipe_segments("a|b|c", 2).is_none());
    }

    #[test]
    fn test_should_parse_force_flag() {
        assert!(parse_force("true"));
        assert!(!parse_force("false"));
        assert!(!parse_force(""));
        assert!(!parse_force("anything"));
    }

    #[test]
    fn test_should_deserialize_init_start_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": INIT_START_ACTION,
                "type": "button",
                "value": "true"
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.actions[0].action_id, INIT_START_ACTION);
        assert_eq!(payload.actions[0].value.as_deref(), Some("true"));
    }

    #[test]
    fn test_should_deserialize_init_modify_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": INIT_MODIFY_ACTION,
                "type": "button",
                "value": "false"
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        assert_eq!(payload.actions[0].action_id, INIT_MODIFY_ACTION);
        assert_eq!(payload.actions[0].value.as_deref(), Some("false"));
    }

    #[test]
    fn test_should_deserialize_init_op_select_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": INIT_OP_SELECT_ACTION,
                "type": "static_select",
                "selected_option": {
                    "text": { "type": "plain_text", "text": "Run" },
                    "value": "run|false"
                }
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        let selected = payload.actions[0]
            .selected_option
            .as_ref()
            .expect("selected_option");

        let segments = parse_pipe_segments(&selected.value, 2).unwrap();
        assert_eq!(segments[0], "run");
        assert!(!parse_force(segments[1]));
    }

    #[test]
    fn test_should_deserialize_init_backend_select_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": INIT_BACKEND_SELECT_ACTION,
                "type": "static_select",
                "selected_option": {
                    "text": { "type": "plain_text", "text": "codex" },
                    "value": "run|codex|true"
                }
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        let selected = payload.actions[0]
            .selected_option
            .as_ref()
            .expect("selected_option");

        let segments = parse_pipe_segments(&selected.value, 3).unwrap();
        assert_eq!(segments[0], "run");
        assert_eq!(segments[1], "codex");
        assert!(parse_force(segments[2]));
    }

    #[test]
    fn test_should_deserialize_init_model_select_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": INIT_MODEL_SELECT_ACTION,
                "type": "static_select",
                "selected_option": {
                    "text": { "type": "plain_text", "text": "gpt-5.3-codex" },
                    "value": "run|codex|gpt-5.3-codex|false"
                }
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        let selected = payload.actions[0]
            .selected_option
            .as_ref()
            .expect("selected_option");

        let segments = parse_pipe_segments(&selected.value, 4).unwrap();
        assert_eq!(segments[0], "run");
        assert_eq!(segments[1], "codex");
        assert_eq!(segments[2], "gpt-5.3-codex");
        assert!(!parse_force(segments[3]));
    }

    #[test]
    fn test_should_deserialize_init_effort_select_action() {
        let json = serde_json::json!({
            "type": "block_actions",
            "actions": [{
                "action_id": INIT_EFFORT_SELECT_ACTION,
                "type": "static_select",
                "selected_option": {
                    "text": { "type": "plain_text", "text": "high" },
                    "value": "run|codex|gpt-5.3-codex|high|true"
                }
            }],
            "channel": { "id": "C123" },
            "message": { "ts": "123.456" }
        });

        let payload: InteractionPayload = serde_json::from_value(json).unwrap();
        let selected = payload.actions[0]
            .selected_option
            .as_ref()
            .expect("selected_option");

        let segments = parse_pipe_segments(&selected.value, 5).unwrap();
        assert_eq!(segments[0], "run");
        assert_eq!(segments[1], "codex");
        assert_eq!(segments[2], "gpt-5.3-codex");
        assert_eq!(segments[3], "high");
        assert!(parse_force(segments[4]));
    }
}
