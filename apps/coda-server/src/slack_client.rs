//! Thin async client for Slack Web API methods used by coda-server.
//!
//! Wraps `reqwest::Client` with the bot token for authorization and provides
//! typed methods for the Slack endpoints needed by this application.
//!
//! The [`SlackClient`] struct implements a manual [`Debug`] that redacts the
//! bot token to prevent accidental credential leakage in logs.

use std::fmt;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tracing::{debug, instrument, warn};

use crate::error::ServerError;

/// Total HTTP request timeout for Slack API calls.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// TCP connection timeout for Slack API calls.
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Thin async client for Slack Web API methods used by coda-server.
///
/// All methods authenticate with the bot token (`xoxb-...`) except
/// [`connections_open`](Self::connections_open) which uses the app-level
/// token (`xapp-...`) passed as a parameter.
///
/// The `Debug` implementation redacts the `bot_token` field to prevent
/// accidental credential exposure in log output.
///
/// # Examples
///
/// ```
/// use coda_server::slack_client::SlackClient;
///
/// let client = SlackClient::new("xoxb-test-token".into()).unwrap();
/// assert!(format!("{client:?}").contains("[REDACTED]"));
/// ```
#[derive(Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
}

impl fmt::Debug for SlackClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackClient")
            .field("bot_token", &"[REDACTED]")
            .finish()
    }
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
    ///
    /// Configures the underlying HTTP client with [`HTTP_CONNECT_TIMEOUT`]
    /// and [`HTTP_REQUEST_TIMEOUT`] to prevent indefinite hangs on Slack
    /// API calls.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::Config` if the HTTP client cannot be built.
    pub fn new(bot_token: String) -> Result<Self, ServerError> {
        let http = reqwest::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ServerError::Config(format!("Failed to build HTTP client: {e}")))?;
        Ok(Self { http, bot_token })
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
    #[instrument(skip(self, blocks), fields(channel = %channel))]
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
        debug!("Posting message");
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
    #[instrument(skip(self, blocks), fields(channel = %channel, ts = %ts))]
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
        debug!("Updating message");
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
    #[instrument(skip(self, text), fields(channel = %channel, thread_ts = %thread_ts))]
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
        debug!("Posting thread reply");
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
    #[instrument(skip(self), fields(channel = %channel, ts = %ts, name = %name))]
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
        debug!("Adding reaction");
        self.call_bot_api("reactions.add", &body).await?;
        Ok(())
    }

    /// Removes a reaction emoji from a message.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::SlackApi` if the API call fails or returns an error.
    #[instrument(skip(self), fields(channel = %channel, ts = %ts, name = %name))]
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
        debug!("Removing reaction");
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
    #[instrument(skip(self, text), fields(channel = %channel, ts = %ts))]
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
        debug!("Updating message text");
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
    #[instrument(skip(self, app_token))]
    pub async fn connections_open(&self, app_token: &str) -> Result<String, ServerError> {
        debug!("Opening Socket Mode connection");
        let start = Instant::now();

        let resp = match self
            .http
            .post(format!("{SLACK_API_BASE}/apps.connections.open"))
            .bearer_auth(app_token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                debug!(
                    duration_ms,
                    error = %e,
                    "apps.connections.open request failed",
                );
                return Err(ServerError::SlackApi(format!(
                    "apps.connections.open request failed: {e}"
                )));
            }
        };

        let api_resp: SlackApiResponse = match resp.json().await {
            Ok(parsed) => parsed,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                debug!(
                    duration_ms,
                    error = %e,
                    "apps.connections.open response parse failed",
                );
                return Err(ServerError::SlackApi(format!(
                    "apps.connections.open response parse failed: {e}"
                )));
            }
        };

        let duration_ms = start.elapsed().as_millis();
        debug!(
            duration_ms,
            ok = api_resp.ok,
            error = ?api_resp.error,
            "Slack API response for apps.connections.open",
        );

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
    #[instrument(skip(self, body), fields(slack_method = method))]
    async fn call_bot_api(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<SlackApiResponse, ServerError> {
        let start = Instant::now();

        let resp = match self
            .http
            .post(format!("{SLACK_API_BASE}/{method}"))
            .bearer_auth(&self.bot_token)
            .json(body)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                debug!(duration_ms, error = %e, "Slack API request failed");
                return Err(ServerError::SlackApi(format!(
                    "{method} request failed: {e}"
                )));
            }
        };

        let api_resp: SlackApiResponse = match resp.json().await {
            Ok(parsed) => parsed,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                debug!(duration_ms, error = %e, "Slack API response parse failed");
                return Err(ServerError::SlackApi(format!(
                    "{method} response parse failed: {e}"
                )));
            }
        };

        let duration_ms = start.elapsed().as_millis();
        debug!(
            duration_ms,
            ok = api_resp.ok,
            error = ?api_resp.error,
            "Slack API response",
        );

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
        let client = SlackClient::new("xoxb-test".into()).unwrap();
        assert!(format!("{client:?}").contains("SlackClient"));
    }

    #[test]
    fn test_should_redact_bot_token_in_debug_output() {
        let client = SlackClient::new("xoxb-secret-token-value".into()).unwrap();
        let debug_output = format!("{client:?}");
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output should contain [REDACTED]"
        );
        assert!(
            !debug_output.contains("xoxb-secret-token-value"),
            "Debug output must not contain the actual token"
        );
    }

    #[test]
    fn test_should_clone_client() {
        let client = SlackClient::new("xoxb-test".into()).unwrap();
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
