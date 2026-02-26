//! Config command handler: `/coda config`.
//!
//! Supports `show`, `get <key>`, and `set <key> <value>` subactions,
//! delegating to the core [`Engine`](coda_core::Engine) config methods.

use std::sync::Arc;

use tracing::{info, instrument};

use crate::error::ServerError;
use crate::formatter;
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

use super::resolve_engine;

/// Handles `/coda config [args]`.
///
/// Parses the arguments to determine the subaction:
/// - `""` or `"show"` — display resolved config for all operations
/// - `"get <key>"` — read a specific dot-path key
/// - `"set <key> <value>"` — update a specific dot-path key
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails, the Engine
/// cannot be created, or the config operation fails.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id))]
pub async fn handle_config(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    args: &str,
) -> Result<(), ServerError> {
    let channel = &payload.channel_id;

    let Some((_repo_path, engine)) = resolve_engine(state.as_ref(), channel).await? else {
        return Ok(());
    };

    let mut parts = args.splitn(3, ' ');
    let subaction = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    let value = parts.next().unwrap_or("").trim();

    let blocks = match subaction {
        "" | "show" => {
            info!(channel, "Showing config");
            let summary = engine.config_show();
            let mut lines = vec![
                "*Agent Configuration (resolved)*".to_string(),
                String::new(),
                format!(
                    "`{:<10}` `{:<10}` `{:<26}` `{:<8}`",
                    "Operation", "Backend", "Model", "Effort"
                ),
            ];
            for (name, resolved) in [
                ("init", &summary.init),
                ("plan", &summary.plan),
                ("run", &summary.run),
                ("review", &summary.review),
                ("verify", &summary.verify),
            ] {
                lines.push(format!(
                    "`{:<10}` `{:<10}` `{:<26}` `{:<8}`",
                    name, resolved.backend, resolved.model, resolved.effort,
                ));
            }
            vec![serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": lines.join("\n") }
            })]
        }
        "get" => {
            if rest.is_empty() {
                formatter::error("Usage: `/coda config get <key>` — provide a dot-path key")
            } else {
                info!(channel, key = rest, "Getting config value");
                match engine.config_get(rest) {
                    Ok(val) => vec![serde_json::json!({
                        "type": "section",
                        "text": { "type": "mrkdwn", "text": format!("`{rest}` = `{val}`") }
                    })],
                    Err(e) => formatter::error(&e.to_string()),
                }
            }
        }
        "set" => {
            if rest.is_empty() {
                // No key provided — show key picker
                info!(channel, "Showing interactive config key select");
                let schema = engine.config_schema();
                formatter::config_key_select(&schema)
            } else if value.is_empty() {
                // Key provided but no value — show value picker
                info!(
                    channel,
                    key = rest,
                    "Showing interactive config value select"
                );
                let schema = engine.config_schema();
                match schema.iter().find(|d| d.key == rest) {
                    Some(descriptor) => formatter::config_value_select(descriptor),
                    None => formatter::error(&format!("Unknown config key: `{rest}`")),
                }
            } else {
                info!(channel, key = rest, value, "Setting config value");
                match engine.config_set(rest, value) {
                    Ok(()) => vec![serde_json::json!({
                        "type": "section",
                        "text": {
                            "type": "mrkdwn",
                            "text": format!(":white_check_mark: Updated `{rest}` = `{value}`")
                        }
                    })],
                    Err(e) => formatter::error(&e.to_string()),
                }
            }
        }
        other => formatter::error(&format!(
            "Unknown config subaction `{other}`. Use `show`, `get <key>`, or `set <key> <value>`."
        )),
    };

    state.slack().post_message(channel, blocks).await?;
    Ok(())
}
