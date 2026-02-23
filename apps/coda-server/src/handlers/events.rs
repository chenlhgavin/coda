//! Events API handler for Slack Socket Mode messages.
//!
//! Routes incoming message events to active plan sessions. Messages in
//! threads with an active [`PlanSession`](coda_core::PlanSession) are
//! forwarded to the plan command handler. Bot messages are skipped to
//! avoid feedback loops.

use std::sync::Arc;

use futures::FutureExt;
use serde::Deserialize;
use tracing::{debug, instrument, warn};

use crate::commands;
use crate::state::AppState;

/// Events API wrapper envelope containing the inner event.
///
/// Slack wraps the actual event inside an `event` field of the
/// `events_api` envelope payload.
#[derive(Debug, Deserialize)]
struct EventsApiPayload {
    event: EventPayload,
}

/// The inner event payload from the Events API.
///
/// Currently only `message` events are handled. Other event types
/// are silently ignored.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum EventPayload {
    /// A message posted in a channel, group, or DM.
    #[serde(rename = "message")]
    Message(MessageEvent),

    /// Any other event type — silently ignored.
    #[serde(other)]
    Other,
}

/// A message event from Slack's Events API.
///
/// # Examples
///
/// ```
/// let json = serde_json::json!({
///     "type": "message",
///     "channel": "C123",
///     "user": "U456",
///     "text": "hello",
///     "ts": "1234567890.123456",
///     "thread_ts": "1234567890.000001"
/// });
/// ```
#[derive(Debug, Deserialize)]
pub struct MessageEvent {
    /// Channel where the message was posted.
    pub channel: String,

    /// User ID of the message author (absent for bot messages).
    #[serde(default)]
    pub user: Option<String>,

    /// Message text content.
    #[serde(default)]
    pub text: String,

    /// Timestamp of this message.
    pub ts: String,

    /// Thread parent timestamp (present only for thread replies).
    #[serde(default)]
    pub thread_ts: Option<String>,

    /// Bot ID if the message was sent by a bot.
    #[serde(default)]
    pub bot_id: Option<String>,

    /// Message subtype (e.g., `"bot_message"`, `"channel_join"`).
    /// Regular user messages have no subtype.
    #[serde(default)]
    pub subtype: Option<String>,
}

/// Handles an Events API envelope payload.
///
/// Parses the event, skips bot messages and non-thread messages,
/// and routes thread replies to active plan sessions.
#[instrument(skip(state, payload))]
pub async fn handle_event(state: Arc<AppState>, payload: serde_json::Value) {
    let events_payload: EventsApiPayload = match serde_json::from_value(payload) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse events_api payload");
            return;
        }
    };

    match events_payload.event {
        EventPayload::Message(msg) => handle_message_event(state, msg).await,
        EventPayload::Other => {
            debug!("Ignoring non-message event");
        }
    }
}

/// Processes a single message event.
///
/// Skips bot messages (to avoid feedback loops), messages without a
/// thread_ts (not thread replies), and messages in threads without an
/// active plan session. Eligible messages are forwarded to the plan
/// thread handler.
#[instrument(skip(state, msg), fields(channel = %msg.channel, thread_ts = ?msg.thread_ts))]
async fn handle_message_event(state: Arc<AppState>, msg: MessageEvent) {
    // Skip bot messages to prevent feedback loops
    if msg.bot_id.is_some() {
        debug!(channel = msg.channel, ts = msg.ts, "Skipping bot message");
        return;
    }

    // Skip messages with subtypes (bot_message, channel_join, etc.)
    if msg.subtype.is_some() {
        debug!(
            channel = msg.channel,
            subtype = ?msg.subtype,
            "Skipping message with subtype"
        );
        return;
    }

    // Only process thread replies
    let Some(ref thread_ts) = msg.thread_ts else {
        return;
    };

    // Check if this thread has an active plan session
    if !state.sessions().contains(&msg.channel, thread_ts) {
        // If this thread previously had an expired session, send a one-time hint
        if state.sessions().was_expired(&msg.channel, thread_ts) {
            state.sessions().clear_expired(&msg.channel, thread_ts);
            let _ = state
                .slack()
                .post_thread_reply(
                    &msg.channel,
                    thread_ts,
                    ":information_source: This planning session has expired due to inactivity. \
                     Use `/coda plan` to start a new session.",
                )
                .await;
        }
        return;
    }

    debug!(
        channel = msg.channel,
        thread_ts,
        user = ?msg.user,
        "Routing thread message to plan session"
    );

    // Run inline with catch_unwind as a panic boundary. Unlike
    // tokio::spawn, this keeps the future in the caller's JoinSet task
    // so that abort_all() on shutdown can cancel it.
    let state_clone = state.clone();
    let channel = msg.channel.clone();
    let thread_ts = thread_ts.clone();
    let user_ts = msg.ts.clone();
    let text = msg.text.clone();

    let result = std::panic::AssertUnwindSafe(commands::plan::handle_thread_message(
        state_clone,
        &channel,
        &thread_ts,
        &user_ts,
        &text,
    ))
    .catch_unwind()
    .await;

    if let Err(panic_info) = result {
        let panic_msg = panic_info
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic_info.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");

        warn!(
            error = panic_msg,
            channel = msg.channel,
            thread_ts = ?msg.thread_ts,
            "Thread message handler panicked"
        );

        let thread_ts = msg.thread_ts.as_deref().unwrap_or(&msg.ts);

        // Clean up hourglass reaction on the user's message (best-effort)
        let _ = state
            .slack()
            .remove_reaction(&msg.channel, &msg.ts, "hourglass_flowing_sand")
            .await;

        // Disconnect and remove the leaked session to free the CLI subprocess
        if let Some(session_arc) = state.sessions().remove(&msg.channel, thread_ts) {
            let mut session = session_arc.lock().await;
            session.disconnect().await;
        }

        // Notify the user in the thread
        let _ = state
            .slack()
            .post_thread_reply(
                &msg.channel,
                thread_ts,
                ":x: An unexpected error occurred. Please try again.",
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_deserialize_message_event() {
        let json = serde_json::json!({
            "event": {
                "type": "message",
                "channel": "C123",
                "user": "U456",
                "text": "hello world",
                "ts": "1234567890.123456",
                "thread_ts": "1234567890.000001"
            }
        });
        let payload: EventsApiPayload = serde_json::from_value(json).expect("deserialize");
        match payload.event {
            EventPayload::Message(msg) => {
                assert_eq!(msg.channel, "C123");
                assert_eq!(msg.user, Some("U456".to_string()));
                assert_eq!(msg.text, "hello world");
                assert_eq!(msg.ts, "1234567890.123456");
                assert_eq!(msg.thread_ts, Some("1234567890.000001".to_string()));
                assert!(msg.bot_id.is_none());
            }
            _ => panic!("Expected Message event"),
        }
    }

    #[test]
    fn test_should_deserialize_bot_message_event() {
        let json = serde_json::json!({
            "event": {
                "type": "message",
                "channel": "C123",
                "text": "bot response",
                "ts": "1234567890.123456",
                "bot_id": "B789"
            }
        });
        let payload: EventsApiPayload = serde_json::from_value(json).expect("deserialize");
        match payload.event {
            EventPayload::Message(msg) => {
                assert_eq!(msg.bot_id, Some("B789".to_string()));
                assert!(msg.user.is_none());
            }
            _ => panic!("Expected Message event"),
        }
    }

    #[test]
    fn test_should_deserialize_message_without_thread() {
        let json = serde_json::json!({
            "event": {
                "type": "message",
                "channel": "C123",
                "user": "U456",
                "text": "top-level message",
                "ts": "1234567890.123456"
            }
        });
        let payload: EventsApiPayload = serde_json::from_value(json).expect("deserialize");
        match payload.event {
            EventPayload::Message(msg) => {
                assert!(msg.thread_ts.is_none());
            }
            _ => panic!("Expected Message event"),
        }
    }

    #[test]
    fn test_should_deserialize_unknown_event_type() {
        let json = serde_json::json!({
            "event": {
                "type": "reaction_added",
                "user": "U456",
                "reaction": "thumbsup"
            }
        });
        let payload: EventsApiPayload = serde_json::from_value(json).expect("deserialize");
        assert!(matches!(payload.event, EventPayload::Other));
    }

    #[test]
    fn test_should_deserialize_message_with_subtype() {
        let json = serde_json::json!({
            "event": {
                "type": "message",
                "channel": "C123",
                "text": "joined the channel",
                "ts": "1234567890.123456",
                "subtype": "channel_join"
            }
        });
        let payload: EventsApiPayload = serde_json::from_value(json).expect("deserialize");
        match payload.event {
            EventPayload::Message(msg) => {
                assert_eq!(msg.subtype, Some("channel_join".to_string()));
            }
            _ => panic!("Expected Message event"),
        }
    }
}
