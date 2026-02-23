//! Thin async client for Slack Web API methods used by coda-server.
//!
//! Wraps `reqwest::Client` with the bot token for authorization and provides
//! typed methods for the Slack endpoints needed by this application.

use serde::Deserialize;
use tracing::{debug, warn};

use crate::error::ServerError;

/// Thin async client for Slack Web API methods used by coda-server.
///
/// All methods authenticate with the bot token (`xoxb-...`) except
/// [`connections_open`](Self::connections_open) which uses the app-level
/// token (`xapp-...`) passed as a parameter.
///
/// # Examples
///
/// ```
/// use coda_server::slack_client::SlackClient;
///
/// let client = SlackClient::new("xoxb-test-token".into());
/// // client.post_message("C123", vec![]).await?;
/// ```
#[derive(Debug, Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
}

/// Successful response from `chat.postMessage`.
#[derive(Debug, Clone)]
pub struct PostMessageResponse {
    /// Message timestamp (used as message ID for updates and thread parent).
    pub ts: String,
}

/// Base URL for Slack Web API.
const SLACK_API_BASE: &str = "https://slack.com/api";

/// Generic Slack API response envelope for deserialization.
#[derive(Debug, Deserialize)]
struct SlackApiResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// Extracts a plain-text fallback from Block Kit blocks.
///
/// Iterates over `section` and `header` blocks and concatenates their text
/// content separated by newlines. Returns `"\u{00a0}"` (non-breaking space)
/// if no renderable text is found, so callers always have a non-empty value
/// suitable for Slack's `text` parameter.
fn extract_blocks_fallback(blocks: &[serde_json::Value]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match block_type {
            "header" => {
                if let Some(text) = block
                    .get("text")
                    .and_then(|t| t.get("text"))
                    .and_then(|v| v.as_str())
                {
                    parts.push(text.to_string());
                }
            }
            "section" => {
                if let Some(text) = block
                    .get("text")
                    .and_then(|t| t.get("text"))
                    .and_then(|v| v.as_str())
                {
                    parts.push(text.to_string());
                }
            }
            "context" => {
                if let Some(elems) = block.get("elements").and_then(|e| e.as_array()) {
                    for elem in elems {
                        if let Some(text) = elem.get("text").and_then(|v| v.as_str()) {
                            parts.push(text.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        "\u{00a0}".to_string()
    } else {
        parts.join("\n")
    }
}

impl SlackClient {
    /// Creates a new Slack Web API client with the given bot token.
    pub fn new(bot_token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            bot_token,
        }
    }

    /// Posts a new message to a channel with Block Kit blocks.
    ///
    /// A plain-text fallback is extracted from the blocks for Slack
    /// notifications and accessibility. See Slack's guidance on
    /// [secondary content](https://api.slack.com/reference/surfaces/formatting#secondary-attachments).
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn post_message(
        &self,
        channel: &str,
        blocks: Vec<serde_json::Value>,
    ) -> Result<PostMessageResponse, ServerError> {
        let fallback = extract_blocks_fallback(&blocks);
        let body = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
            "text": fallback,
        });
        debug!(channel, "Posting message");
        let resp = self.call_bot_api("chat.postMessage", &body).await?;
        Ok(PostMessageResponse {
            ts: resp.ts.ok_or_else(|| {
                ServerError::SlackApi("chat.postMessage response missing 'ts'".into())
            })?,
        })
    }

    /// Updates an existing message identified by its timestamp.
    ///
    /// A plain-text fallback is included alongside the blocks to satisfy
    /// Slack's `chat.update` requirement (the API returns `no_text` when
    /// neither `text` nor renderable `blocks` are present).
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        blocks: Vec<serde_json::Value>,
    ) -> Result<(), ServerError> {
        let fallback = extract_blocks_fallback(&blocks);
        let body = serde_json::json!({
            "channel": channel,
            "ts": ts,
            "blocks": blocks,
            "text": fallback,
        });
        debug!(channel, ts, "Updating message");
        self.call_bot_api("chat.update", &body).await?;
        Ok(())
    }

    /// Posts a reply in a thread.
    ///
    /// Returns the timestamp of the reply message.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn post_thread_reply(
        &self,
        channel: &str,
        thread_ts: &str,
        text: &str,
    ) -> Result<String, ServerError> {
        let body = serde_json::json!({
            "channel": channel,
            "thread_ts": thread_ts,
            "text": text,
        });
        debug!(channel, thread_ts, "Posting thread reply");
        let resp = self.call_bot_api("chat.postMessage", &body).await?;
        resp.ts.ok_or_else(|| {
            ServerError::SlackApi("chat.postMessage thread reply missing 'ts'".into())
        })
    }

    /// Adds a reaction emoji to a message.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn add_reaction(
        &self,
        channel: &str,
        ts: &str,
        name: &str,
    ) -> Result<(), ServerError> {
        let body = serde_json::json!({
            "channel": channel,
            "timestamp": ts,
            "name": name,
        });
        debug!(channel, ts, name, "Adding reaction");
        self.call_bot_api("reactions.add", &body).await?;
        Ok(())
    }

    /// Removes a reaction emoji from a message.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn remove_reaction(
        &self,
        channel: &str,
        ts: &str,
        name: &str,
    ) -> Result<(), ServerError> {
        let body = serde_json::json!({
            "channel": channel,
            "timestamp": ts,
            "name": name,
        });
        debug!(channel, ts, name, "Removing reaction");
        self.call_bot_api("reactions.remove", &body).await?;
        Ok(())
    }

    /// Updates an existing message's plain-text content.
    ///
    /// Unlike [`update_message`] which replaces Block Kit blocks, this method
    /// updates a text-only message identified by its timestamp.
    ///
    /// Empty or whitespace-only text is replaced with a non-breaking space
    /// to prevent Slack's `no_text` error.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn update_message_text(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<(), ServerError> {
        let safe_text = if text.trim().is_empty() {
            "\u{00a0}" // non-breaking space
        } else {
            text
        };
        let body = serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": safe_text,
        });
        debug!(channel, ts, "Updating message text");
        self.call_bot_api("chat.update", &body).await?;
        Ok(())
    }

    /// Opens a Socket Mode connection and returns the WebSocket URL.
    ///
    /// Uses the app-level token (`xapp-...`) rather than the bot token.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    pub async fn connections_open(&self, app_token: &str) -> Result<String, ServerError> {
        debug!("Opening Socket Mode connection");
        let resp = self
            .http
            .post(format!("{SLACK_API_BASE}/apps.connections.open"))
            .bearer_auth(app_token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| {
                ServerError::SlackApi(format!("apps.connections.open request failed: {e}"))
            })?;

        let api_resp: SlackApiResponse = resp.json().await.map_err(|e| {
            ServerError::SlackApi(format!("apps.connections.open response parse failed: {e}"))
        })?;

        if !api_resp.ok {
            return Err(ServerError::SlackApi(format!(
                "apps.connections.open error: {}",
                api_resp.error.unwrap_or_default()
            )));
        }

        api_resp.url.ok_or_else(|| {
            ServerError::SlackApi("apps.connections.open response missing 'url'".into())
        })
    }

    /// Sends a JSON POST request to a Slack Web API method using the bot token.
    async fn call_bot_api(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<SlackApiResponse, ServerError> {
        let resp = self
            .http
            .post(format!("{SLACK_API_BASE}/{method}"))
            .bearer_auth(&self.bot_token)
            .json(body)
            .send()
            .await
            .map_err(|e| ServerError::SlackApi(format!("{method} request failed: {e}")))?;

        let api_resp: SlackApiResponse = resp
            .json()
            .await
            .map_err(|e| ServerError::SlackApi(format!("{method} response parse failed: {e}")))?;

        if !api_resp.ok {
            let error_msg = api_resp.error.as_deref().unwrap_or("unknown");
            warn!(method, error = error_msg, "Slack API error");
            return Err(ServerError::SlackApi(format!(
                "{method} error: {error_msg}"
            )));
        }

        Ok(api_resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_create_client() {
        let client = SlackClient::new("xoxb-test".into());
        assert!(format!("{client:?}").contains("SlackClient"));
    }

    #[test]
    fn test_should_clone_client() {
        let client = SlackClient::new("xoxb-test".into());
        let cloned = client.clone();
        assert!(format!("{cloned:?}").contains("SlackClient"));
    }

    #[test]
    fn test_should_extract_fallback_from_header_and_section() {
        let blocks = vec![
            serde_json::json!({
                "type": "header",
                "text": { "type": "plain_text", "text": "Plan: `add-auth`" }
            }),
            serde_json::json!({
                "type": "section",
                "text": { "type": "mrkdwn", "text": ":speech_balloon: *Status:* Discussing" }
            }),
        ];
        let fallback = extract_blocks_fallback(&blocks);
        assert!(fallback.contains("Plan: `add-auth`"));
        assert!(fallback.contains("Discussing"));
    }

    #[test]
    fn test_should_extract_fallback_from_context_blocks() {
        let blocks = vec![serde_json::json!({
            "type": "context",
            "elements": [{ "type": "mrkdwn", "text": "_2 features_" }]
        })];
        let fallback = extract_blocks_fallback(&blocks);
        assert!(fallback.contains("2 features"));
    }

    #[test]
    fn test_should_return_nbsp_for_empty_blocks() {
        let blocks: Vec<serde_json::Value> = vec![];
        let fallback = extract_blocks_fallback(&blocks);
        assert_eq!(fallback, "\u{00a0}");
    }

    #[test]
    fn test_should_return_nbsp_for_non_text_blocks() {
        let blocks = vec![serde_json::json!({ "type": "divider" })];
        let fallback = extract_blocks_fallback(&blocks);
        assert_eq!(fallback, "\u{00a0}");
    }
}
