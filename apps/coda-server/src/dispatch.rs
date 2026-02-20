//! Envelope parsing and type-based dispatch for Socket Mode messages.
//!
//! Slack Socket Mode delivers envelopes over WebSocket. Each envelope has a
//! `type` field indicating the kind of payload (slash command, event, or
//! interactive action). This module parses raw JSON into [`Envelope`] structs
//! and routes them to the appropriate handler.

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::error::ServerError;
use crate::handlers;
use crate::state::AppState;

/// A Socket Mode envelope received from Slack.
///
/// Every envelope must be acknowledged within 3 seconds by sending back
/// its `envelope_id`. The `payload` contains type-specific data.
///
/// # Examples
///
/// ```
/// use coda_server::dispatch::{Envelope, EnvelopeType};
///
/// let json = r#"{
///     "envelope_id": "abc123",
///     "type": "slash_commands",
///     "payload": {"command": "/coda", "text": "help"}
/// }"#;
///
/// let envelope: Envelope = serde_json::from_str(json).unwrap();
/// assert_eq!(envelope.envelope_id, "abc123");
/// assert_eq!(envelope.envelope_type, EnvelopeType::SlashCommands);
/// ```
#[derive(Debug, Clone)]
pub struct Envelope {
    /// Unique identifier for this envelope, used in acknowledgment.
    pub envelope_id: String,

    /// The type of payload contained in this envelope.
    pub envelope_type: EnvelopeType,

    /// Type-specific payload data.
    pub payload: serde_json::Value,
}

/// The type of a Socket Mode envelope payload.
///
/// # Examples
///
/// ```
/// use coda_server::dispatch::EnvelopeType;
///
/// let et = EnvelopeType::SlashCommands;
/// assert_eq!(format!("{et:?}"), "SlashCommands");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeType {
    /// A slash command invocation (e.g., `/coda help`).
    SlashCommands,

    /// An Events API event (e.g., a message in a channel).
    EventsApi,

    /// An interactive component action (e.g., button click).
    Interactive,
}

/// Raw Socket Mode message for initial deserialization.
///
/// Both system messages (`hello`, `disconnect`) and envelopes share this
/// structure; they differ in which fields are present.
#[derive(Debug, Deserialize)]
struct RawSocketMessage {
    #[serde(rename = "type")]
    msg_type: String,

    #[serde(default)]
    envelope_id: Option<String>,

    #[serde(default)]
    payload: Option<serde_json::Value>,
}

/// Result of parsing a raw Socket Mode message.
#[derive(Debug)]
pub enum ParsedMessage {
    /// A `hello` message confirming connection.
    Hello,

    /// A `disconnect` message requesting reconnection.
    Disconnect,

    /// A business envelope that needs acknowledgment and handling.
    Envelope(Envelope),
}

/// Parses a raw JSON string from the WebSocket into a [`ParsedMessage`].
///
/// Returns `None` for unknown message types (logged as a warning).
///
/// # Errors
///
/// Returns `ServerError::Dispatch` if JSON parsing fails.
pub fn parse_message(text: &str) -> Result<Option<ParsedMessage>, ServerError> {
    let raw: RawSocketMessage =
        serde_json::from_str(text).map_err(|e| ServerError::Dispatch(format!("Bad JSON: {e}")))?;

    match raw.msg_type.as_str() {
        "hello" => {
            info!("Received hello from Slack — connection established");
            Ok(Some(ParsedMessage::Hello))
        }
        "disconnect" => {
            info!("Received disconnect from Slack — will reconnect");
            Ok(Some(ParsedMessage::Disconnect))
        }
        "slash_commands" | "events_api" | "interactive" => {
            let Some(envelope_id) = raw.envelope_id else {
                warn!(
                    msg_type = raw.msg_type,
                    "Envelope missing envelope_id, skipping"
                );
                return Ok(None);
            };

            let envelope_type = match raw.msg_type.as_str() {
                "slash_commands" => EnvelopeType::SlashCommands,
                "events_api" => EnvelopeType::EventsApi,
                "interactive" => EnvelopeType::Interactive,
                _ => unreachable!(),
            };

            debug!(
                envelope_id,
                envelope_type = ?envelope_type,
                "Parsed envelope"
            );

            Ok(Some(ParsedMessage::Envelope(Envelope {
                envelope_id,
                envelope_type,
                payload: raw.payload.unwrap_or(serde_json::Value::Null),
            })))
        }
        other => {
            warn!(
                msg_type = other,
                "Unknown Socket Mode message type, ignoring"
            );
            Ok(None)
        }
    }
}

/// Dispatches an envelope to the appropriate handler based on its type.
///
/// This is the main routing function called after acknowledging the envelope.
/// Each handler runs independently and posts results back to Slack.
pub async fn dispatch(state: Arc<AppState>, envelope: Envelope) {
    debug!(
        envelope_id = envelope.envelope_id,
        envelope_type = ?envelope.envelope_type,
        "Dispatching envelope"
    );

    match envelope.envelope_type {
        EnvelopeType::SlashCommands => {
            handlers::commands::handle_slash_command(state, envelope.payload).await;
        }
        EnvelopeType::EventsApi => {
            debug!("Events API handler not yet implemented");
        }
        EnvelopeType::Interactive => {
            debug!("Interactive handler not yet implemented");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_parse_hello_message() {
        let json = r#"{"type":"hello","num_connections":1}"#;
        let parsed = parse_message(json).expect("parse").expect("some");
        assert!(matches!(parsed, ParsedMessage::Hello));
    }

    #[test]
    fn test_should_parse_disconnect_message() {
        let json = r#"{"type":"disconnect","reason":"warning"}"#;
        let parsed = parse_message(json).expect("parse").expect("some");
        assert!(matches!(parsed, ParsedMessage::Disconnect));
    }

    #[test]
    fn test_should_parse_slash_command_envelope() {
        let json = r#"{
            "envelope_id": "env-123",
            "type": "slash_commands",
            "payload": {"command": "/coda", "text": "help"}
        }"#;
        let parsed = parse_message(json).expect("parse").expect("some");
        match parsed {
            ParsedMessage::Envelope(env) => {
                assert_eq!(env.envelope_id, "env-123");
                assert_eq!(env.envelope_type, EnvelopeType::SlashCommands);
                assert_eq!(env.payload["command"], "/coda");
            }
            _ => panic!("Expected Envelope"),
        }
    }

    #[test]
    fn test_should_parse_events_api_envelope() {
        let json = r#"{
            "envelope_id": "env-456",
            "type": "events_api",
            "payload": {"event": {"type": "message"}}
        }"#;
        let parsed = parse_message(json).expect("parse").expect("some");
        match parsed {
            ParsedMessage::Envelope(env) => {
                assert_eq!(env.envelope_type, EnvelopeType::EventsApi);
            }
            _ => panic!("Expected Envelope"),
        }
    }

    #[test]
    fn test_should_parse_interactive_envelope() {
        let json = r#"{
            "envelope_id": "env-789",
            "type": "interactive",
            "payload": {"actions": []}
        }"#;
        let parsed = parse_message(json).expect("parse").expect("some");
        match parsed {
            ParsedMessage::Envelope(env) => {
                assert_eq!(env.envelope_type, EnvelopeType::Interactive);
            }
            _ => panic!("Expected Envelope"),
        }
    }

    #[test]
    fn test_should_return_none_for_unknown_type() {
        let json = r#"{"type":"unknown_type"}"#;
        let parsed = parse_message(json).expect("parse");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_should_skip_envelope_without_id() {
        let json = r#"{"type":"slash_commands","payload":{}}"#;
        let parsed = parse_message(json).expect("parse");
        assert!(parsed.is_none());
    }

    #[test]
    fn test_should_error_on_invalid_json() {
        let result = parse_message("not json");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Bad JSON"));
    }

    #[test]
    fn test_should_handle_null_payload() {
        let json = r#"{"envelope_id":"e1","type":"slash_commands"}"#;
        let parsed = parse_message(json).expect("parse").expect("some");
        match parsed {
            ParsedMessage::Envelope(env) => {
                assert!(env.payload.is_null());
            }
            _ => panic!("Expected Envelope"),
        }
    }
}
