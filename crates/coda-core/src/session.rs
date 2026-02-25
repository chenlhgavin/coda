//! Shared agent session abstraction for streaming/timeout/reconnect logic.
//!
//! Provides [`AgentSession`] which wraps a [`ClaudeClient`] with idle timeout
//! detection, exponential backoff reconnection, response accumulation, and
//! API error detection. Both [`PlanSession`](crate::planner::PlanSession) and
//! [`Runner`](crate::runner::Runner) delegate their agent interactions here,
//! eliminating the duplicated streaming logic that previously existed in both.
//!
//! # Cancellation
//!
//! An optional [`CancellationToken`] can be set via [`AgentSession::set_cancellation_token`].
//! When triggered, the token causes the streaming loop in [`AgentSession::send`] to
//! return [`CoreError::Cancelled`] promptly, and the backoff sleep in
//! [`AgentSession::reconnect_and_resend`] is also interrupted.
//!
//! # Architecture
//!
//! `AgentSession` does not know about `RunEvent` or `PlanStreamUpdate`.
//! Instead, it emits [`SessionEvent`] variants through an optional channel,
//! letting callers map these events to their own domain-specific types.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), coda_core::CoreError> {
//! use coda_core::session::{AgentSession, SessionConfig};
//! use claude_agent_sdk_rs::ClaudeClient;
//!
//! let config = SessionConfig {
//!     idle_timeout_secs: 300,
//!     tool_execution_timeout_secs: 600,
//!     idle_retries: 3,
//!     max_budget_usd: 100.0,
//! };
//! // let client = ClaudeClient::new(options);
//! // let mut session = AgentSession::new(client, config);
//! // session.connect().await?;
//! // let resp = session.send("Hello", None).await?;
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use claude_agent_sdk_rs::{ClaudeClient, ContentBlock, Message, ResultMessage, ToolResultContent};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use crate::CoreError;

/// Maximum characters for tool input summaries in streaming events.
pub const TOOL_SUMMARY_MAX_LEN: usize = 60;

/// Known error patterns that API proxies may embed in assistant text
/// instead of signaling through the SDK error channel.
///
/// These patterns indicate that the agent session is non-functional and
/// any response text is an error message, not useful work output.
const API_ERROR_PATTERNS: &[&str] = &[
    "Failed to authenticate",
    "API Error: 401",
    "API Error: 403",
    "API Error: 429",
    "API Error: 500",
    "API Error: 502",
    "API Error: 503",
    "rate limit",
    "quota exceeded",
];

/// Streaming events emitted by [`AgentSession::send`].
///
/// Callers receive these events through an optional channel and map them
/// to their own domain-specific types (e.g., `RunEvent`, `PlanStreamUpdate`).
///
/// # Example
///
/// ```
/// use coda_core::session::SessionEvent;
///
/// let event = SessionEvent::TextDelta {
///     text: "Hello".to_string(),
/// };
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SessionEvent {
    /// Incremental text delta from the assistant.
    TextDelta {
        /// The text fragment to append to the streaming buffer.
        text: String,
    },
    /// A tool invocation observed during agent execution.
    ToolActivity {
        /// Tool name (e.g., `"Bash"`, `"Write"`, `"Read"`).
        tool_name: String,
        /// Brief summary of the tool input.
        summary: String,
    },
    /// An agent turn completed within the current interaction.
    TurnCompleted {
        /// Number of turns completed so far.
        current_turn: u32,
    },
    /// An idle timeout fired but retries remain.
    IdleWarning {
        /// Which retry attempt this is (1-based).
        attempt: u32,
        /// Maximum retries before aborting.
        max_retries: u32,
        /// How many seconds of silence elapsed.
        idle_secs: u64,
    },
    /// The agent subprocess is being reconnected after an idle timeout.
    Reconnecting {
        /// Which reconnection attempt this is (1-based).
        attempt: u32,
        /// Maximum reconnection attempts before aborting.
        max_retries: u32,
    },
}

/// Configuration for [`AgentSession`] timeout and budget behavior.
///
/// # Example
///
/// ```
/// use coda_core::session::SessionConfig;
///
/// let config = SessionConfig {
///     idle_timeout_secs: 300,
///     tool_execution_timeout_secs: 600,
///     idle_retries: 3,
///     max_budget_usd: 100.0,
/// };
/// assert_eq!(config.idle_retries, 3);
/// ```
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Maximum seconds of silence before counting as one idle timeout.
    pub idle_timeout_secs: u64,
    /// Maximum seconds of silence during tool execution.
    pub tool_execution_timeout_secs: u64,
    /// How many consecutive idle timeouts to tolerate before aborting.
    pub idle_retries: u32,
    /// Maximum budget in USD for the session.
    pub max_budget_usd: f64,
}

/// Collected output from a single agent interaction.
///
/// Separates assistant text from tool execution output so callers can
/// search both independently (e.g., extracting a PR URL from bash stdout).
///
/// # Example
///
/// ```
/// use coda_core::session::AgentResponse;
///
/// let resp = AgentResponse::default();
/// assert!(resp.text.is_empty());
/// assert!(resp.tool_output.is_empty());
/// assert_eq!(resp.all_text(), "");
/// ```
#[derive(Debug, Default)]
pub struct AgentResponse {
    /// Text content from assistant messages.
    pub text: String,
    /// Combined tool result output (bash stdout/stderr, etc.).
    pub tool_output: String,
    /// SDK result message with metrics.
    pub result: Option<ResultMessage>,
}

impl AgentResponse {
    /// Returns all collected text (assistant text + tool output) for searching.
    ///
    /// # Example
    ///
    /// ```
    /// use coda_core::session::AgentResponse;
    ///
    /// let resp = AgentResponse {
    ///     text: "assistant".to_string(),
    ///     tool_output: "tool".to_string(),
    ///     result: None,
    /// };
    /// assert!(resp.all_text().contains("assistant"));
    /// assert!(resp.all_text().contains("tool"));
    /// ```
    pub fn all_text(&self) -> String {
        if self.tool_output.is_empty() {
            self.text.clone()
        } else {
            format!("{}\n{}", self.text, self.tool_output)
        }
    }
}

/// Wraps a [`ClaudeClient`] with shared streaming, idle timeout, and
/// reconnection logic.
///
/// This struct is the single implementation of the send/collect pattern
/// that was previously duplicated between `Runner::send_and_collect` and
/// `PlanSession::send_inner`.
///
/// An optional [`CancellationToken`] can be set to enable graceful
/// cancellation of in-flight agent interactions.
pub struct AgentSession {
    client: ClaudeClient,
    config: SessionConfig,
    connected: bool,
    event_tx: Option<UnboundedSender<SessionEvent>>,
    /// Callback invoked before each reconnection attempt, allowing callers
    /// to persist state for crash safety.
    on_reconnect: Option<Box<dyn Fn() + Send>>,
    /// Optional cancellation token for graceful shutdown.
    cancel_token: Option<CancellationToken>,
}

impl AgentSession {
    /// Creates a new agent session wrapping the given client.
    ///
    /// # Arguments
    ///
    /// * `client` - The `ClaudeClient` to wrap.
    /// * `config` - Timeout and budget configuration.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use coda_core::session::{AgentSession, SessionConfig};
    /// use claude_agent_sdk_rs::ClaudeClient;
    ///
    /// # fn example(client: ClaudeClient) {
    /// let config = SessionConfig {
    ///     idle_timeout_secs: 300,
    ///     tool_execution_timeout_secs: 600,
    ///     idle_retries: 3,
    ///     max_budget_usd: 100.0,
    /// };
    /// let session = AgentSession::new(client, config);
    /// # }
    /// ```
    pub fn new(client: ClaudeClient, config: SessionConfig) -> Self {
        Self {
            client,
            config,
            connected: false,
            event_tx: None,
            on_reconnect: None,
            cancel_token: None,
        }
    }

    /// Sets the event channel for streaming session events.
    pub fn set_event_sender(&mut self, tx: UnboundedSender<SessionEvent>) {
        self.event_tx = Some(tx);
    }

    /// Clears the event channel, dropping the sender.
    ///
    /// This causes any spawned forwarder task that reads from the
    /// corresponding receiver to observe channel closure and terminate.
    /// Call this after [`send`](Self::send) returns to ensure temporary
    /// event bridges do not outlive the interaction.
    pub fn clear_event_sender(&mut self) {
        self.event_tx = None;
    }

    /// Sets a callback that is invoked before each reconnection attempt.
    ///
    /// This allows callers (e.g., `Runner`) to persist state for crash
    /// safety before the subprocess is killed and restarted.
    pub fn set_on_reconnect(&mut self, f: impl Fn() + Send + 'static) {
        self.on_reconnect = Some(Box::new(f));
    }

    /// Sets the cancellation token for graceful shutdown.
    ///
    /// When the token is cancelled, the streaming loop in [`send`](Self::send)
    /// will return [`CoreError::Cancelled`] at the next check point, and the
    /// backoff sleep in reconnection will be interrupted.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use coda_core::session::{AgentSession, SessionConfig};
    /// use claude_agent_sdk_rs::ClaudeClient;
    /// use tokio_util::sync::CancellationToken;
    ///
    /// # fn example(client: ClaudeClient) {
    /// let config = SessionConfig {
    ///     idle_timeout_secs: 300,
    ///     tool_execution_timeout_secs: 600,
    ///     idle_retries: 3,
    ///     max_budget_usd: 100.0,
    /// };
    /// let mut session = AgentSession::new(client, config);
    /// let token = CancellationToken::new();
    /// session.set_cancellation_token(token);
    /// # }
    /// ```
    pub fn set_cancellation_token(&mut self, token: CancellationToken) {
        self.cancel_token = Some(token);
    }

    /// Returns a mutable reference to the underlying `ClaudeClient`.
    ///
    /// Useful for callers that need to configure the client (e.g., setting
    /// `stderr_callback` or `include_partial_messages`).
    pub fn client_mut(&mut self) -> &mut ClaudeClient {
        &mut self.client
    }

    /// Returns whether the session is currently connected.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Connects the underlying `ClaudeClient` to the Claude process.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` if the connection fails.
    pub async fn connect(&mut self) -> Result<(), CoreError> {
        self.client
            .connect()
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;
        self.connected = true;
        debug!("AgentSession connected to Claude");
        Ok(())
    }

    /// Disconnects the underlying `ClaudeClient`.
    ///
    /// Safe to call multiple times or when not connected.
    pub async fn disconnect(&mut self) {
        if self.connected {
            let _ = self.client.disconnect().await;
            self.connected = false;
            debug!("AgentSession disconnected from Claude");
        }
    }

    /// Returns `Err(Cancelled)` if the cancellation token has been triggered.
    fn check_cancelled(&self) -> Result<(), CoreError> {
        if let Some(ref token) = self.cancel_token
            && token.is_cancelled()
        {
            return Err(CoreError::Cancelled);
        }

        Ok(())
    }

    /// Sends a prompt and collects the full response.
    ///
    /// Handles idle timeout detection, exponential backoff reconnection,
    /// response accumulation, and API error detection. Emits [`SessionEvent`]
    /// variants through the configured channel as the agent generates text
    /// and invokes tools.
    ///
    /// When a [`CancellationToken`] is set and triggered, returns
    /// [`CoreError::Cancelled`] at the next check point in the streaming loop.
    ///
    /// When `session_id` is `Some`, sends the prompt on an isolated session
    /// (via [`ClaudeClient::query_with_session`]) so the conversation history
    /// of the main session is not included.
    ///
    /// # Arguments
    ///
    /// * `prompt` - The user message to send to the agent.
    /// * `session_id` - Optional isolated session identifier.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::Cancelled` if the cancellation token is triggered.
    /// Returns `CoreError::IdleTimeout` if all reconnection retries are
    /// exhausted. Returns `CoreError::AgentError` if the agent returns an
    /// empty response or an error result. Returns `CoreError::BudgetExhausted`
    /// if the session budget is exhausted.
    pub async fn send(
        &mut self,
        prompt: &str,
        session_id: Option<&str>,
    ) -> Result<AgentResponse, CoreError> {
        self.check_cancelled()?;

        if !self.connected {
            self.connect().await?;
        }

        let base_timeout = Duration::from_secs(self.config.idle_timeout_secs);
        let tool_timeout = Duration::from_secs(self.config.tool_execution_timeout_secs);
        let max_idle_retries = self.config.idle_retries;
        let mut consecutive_timeouts: u32 = 0;

        // Clone the cancel token for use in `select!` blocks where we cannot
        // borrow `self` (the stream borrows `self.client`).
        let cancel_token = self.cancel_token.clone();

        // Initial query
        match session_id {
            Some(id) => self.client.query_with_session(prompt, id).await,
            None => self.client.query(prompt).await,
        }
        .map_err(|e| CoreError::AgentError(e.to_string()))?;

        let mut resp = AgentResponse::default();
        let mut turn_count: u32 = 0;
        let mut in_tool_execution = false;

        'outer: loop {
            // Check cancellation at the start of each reconnection cycle
            check_token_cancelled(&cancel_token)?;

            let mut stream = self.client.receive_response();
            loop {
                let timeout = if in_tool_execution {
                    tool_timeout
                } else {
                    base_timeout
                };

                // Race the stream against timeout and cancellation.
                // When `cancel_token` is `None`, the cancellation branch
                // uses `std::future::pending()` which never resolves.
                let timed_result = tokio::select! {
                    biased;
                    () = cancel_wait(&cancel_token) => {
                        return Err(CoreError::Cancelled);
                    }
                    timed = tokio::time::timeout(timeout, stream.next()) => timed,
                };

                let result = match timed_result {
                    Ok(Some(result)) => {
                        consecutive_timeouts = 0;
                        result
                    }
                    Ok(None) => break 'outer,
                    Err(_) => {
                        consecutive_timeouts += 1;
                        let idle_secs = if in_tool_execution {
                            self.config.tool_execution_timeout_secs
                        } else {
                            self.config.idle_timeout_secs
                        };

                        if consecutive_timeouts <= max_idle_retries {
                            warn!(
                                idle_secs,
                                attempt = consecutive_timeouts,
                                max_retries = max_idle_retries,
                                turns = turn_count,
                                in_tool_execution,
                                "Agent idle timeout — reconnecting subprocess",
                            );
                            self.emit_event(SessionEvent::IdleWarning {
                                attempt: consecutive_timeouts,
                                max_retries: max_idle_retries,
                                idle_secs,
                            });

                            // Drop stream before reconnecting (releases borrow)
                            drop(stream);

                            self.reconnect_and_resend(
                                prompt,
                                session_id,
                                consecutive_timeouts,
                                max_idle_retries,
                            )
                            .await?;

                            // Reset accumulator — partial data is unreliable
                            resp = AgentResponse::default();
                            turn_count = 0;
                            in_tool_execution = false;

                            continue 'outer;
                        }

                        let total_secs = idle_secs * u64::from(max_idle_retries + 1);
                        error!(
                            total_idle_secs = total_secs,
                            turns = turn_count,
                            "Agent idle timeout — all retries exhausted",
                        );
                        return Err(CoreError::IdleTimeout {
                            total_idle_secs: total_secs,
                            retries_exhausted: max_idle_retries,
                        });
                    }
                };
                let msg = result.map_err(|e| CoreError::AgentError(e.to_string()))?;
                match msg {
                    Message::Assistant(assistant) => {
                        turn_count += 1;
                        in_tool_execution = false;
                        self.emit_event(SessionEvent::TurnCompleted {
                            current_turn: turn_count,
                        });
                        for block in &assistant.message.content {
                            match block {
                                ContentBlock::Text(text) => {
                                    resp.text.push_str(&text.text);
                                }
                                ContentBlock::ToolUse(tu) => {
                                    in_tool_execution = true;
                                    let summary = summarize_tool_input(&tu.name, &tu.input);
                                    trace!(
                                        tool = %tu.name,
                                        summary = %summary,
                                        "Tool invocation observed",
                                    );
                                    self.emit_event(SessionEvent::ToolActivity {
                                        tool_name: tu.name.clone(),
                                        summary,
                                    });
                                }
                                ContentBlock::ToolResult(tr) => {
                                    in_tool_execution = false;
                                    collect_tool_result_text(
                                        tr.content.as_ref(),
                                        &mut resp.tool_output,
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                    Message::User(user) => {
                        in_tool_execution = false;
                        if let Some(blocks) = &user.content {
                            for block in blocks {
                                if let ContentBlock::ToolResult(tr) = block {
                                    collect_tool_result_text(
                                        tr.content.as_ref(),
                                        &mut resp.tool_output,
                                    );
                                }
                            }
                        }
                    }
                    Message::StreamEvent(event) => {
                        if let Some(text) = extract_text_delta(&event.event) {
                            self.emit_event(SessionEvent::TextDelta {
                                text: text.to_string(),
                            });
                        }
                    }
                    Message::Result(r) => {
                        resp.result = Some(r);
                        break 'outer;
                    }
                    _ => {}
                }
            }
        }

        // Post-response validation
        self.validate_response(&resp)?;

        Ok(resp)
    }

    /// Validates the agent response for errors, API error patterns, empty
    /// responses, and budget exhaustion.
    fn validate_response(&self, resp: &AgentResponse) -> Result<(), CoreError> {
        // 1. Check ResultMessage.is_error flag
        if let Some(ref result) = resp.result
            && result.is_error
        {
            let detail = resp.text.chars().take(200).collect::<String>();
            error!(
                is_error = result.is_error,
                turns = result.num_turns,
                text_preview = %detail,
                "Agent session returned error result",
            );
            return Err(CoreError::AgentError(format!(
                "Agent session returned error: {detail}",
            )));
        }

        // 2. Detect API errors in text content
        if resp.tool_output.is_empty() && contains_api_error_pattern(&resp.text) {
            let detail = resp.text.chars().take(200).collect::<String>();
            error!(
                text_preview = %detail,
                "Agent response contains API error pattern",
            );
            return Err(CoreError::AgentError(format!(
                "API error detected in agent response: {detail}",
            )));
        }

        // 3. Detect empty responses
        if resp.text.is_empty() && resp.tool_output.is_empty() {
            // Check budget exhaustion first
            let budget_limit = self.config.max_budget_usd;
            if let Some(spent) = resp.result.as_ref().and_then(|r| r.total_cost_usd)
                && spent >= budget_limit
            {
                error!(
                    spent = spent,
                    limit = budget_limit,
                    "Session budget exhausted",
                );
                return Err(CoreError::BudgetExhausted {
                    spent,
                    limit: budget_limit,
                });
            }

            let reason = resp
                .result
                .as_ref()
                .map(|r| {
                    format!(
                        "turns={}, cost={:?}, is_error={}",
                        r.num_turns, r.total_cost_usd, r.is_error,
                    )
                })
                .unwrap_or_else(|| "no ResultMessage received".to_string());

            error!(reason = %reason, "Agent returned empty response");
            return Err(CoreError::AgentError(format!(
                "Agent returned empty response (session may be disconnected): {reason}",
            )));
        }

        Ok(())
    }

    /// Reconnects the subprocess and re-sends the prompt after idle timeout.
    ///
    /// The backoff sleep is interruptible by the cancellation token.
    async fn reconnect_and_resend(
        &mut self,
        prompt: &str,
        session_id: Option<&str>,
        attempt: u32,
        max_retries: u32,
    ) -> Result<(), CoreError> {
        // Invoke the on_reconnect callback for crash safety
        if let Some(ref callback) = self.on_reconnect {
            callback();
        }

        self.emit_event(SessionEvent::Reconnecting {
            attempt,
            max_retries,
        });

        // Disconnect old subprocess
        let _ = self.client.disconnect().await;
        self.connected = false;

        // Exponential backoff: 5s, 15s, 45s, ...
        // Interruptible by cancellation token.
        let backoff_secs = 5u64 * 3u64.saturating_pow(attempt.saturating_sub(1));
        info!(
            backoff_secs,
            attempt, "Waiting before reconnecting subprocess",
        );
        let cancel_token = self.cancel_token.clone();
        tokio::select! {
            biased;
            () = cancel_wait(&cancel_token) => {
                return Err(CoreError::Cancelled);
            }
            () = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
        }

        // Reconnect
        self.client
            .connect()
            .await
            .map_err(|e| CoreError::AgentError(format!("Reconnect failed: {e}")))?;
        self.connected = true;

        // Re-send prompt
        match session_id {
            Some(id) => self.client.query_with_session(prompt, id).await,
            None => self.client.query(prompt).await,
        }
        .map_err(|e| CoreError::AgentError(format!("Re-query after reconnect failed: {e}")))?;

        info!(attempt, "Subprocess reconnected and prompt re-sent");
        Ok(())
    }

    /// Emits a session event, if a channel is configured.
    fn emit_event(&self, event: SessionEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}

impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("connected", &self.connected)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Awaits cancellation of the token, or pends forever if `None`.
///
/// This helper is used in `tokio::select!` branches to race the streaming
/// loop against cancellation. When `token` is `None`, the returned future
/// never resolves, effectively disabling the cancellation branch.
async fn cancel_wait(token: &Option<CancellationToken>) {
    match token {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Returns `Err(Cancelled)` if the optional token has been triggered.
fn check_token_cancelled(token: &Option<CancellationToken>) -> Result<(), CoreError> {
    if let Some(t) = token
        && t.is_cancelled()
    {
        return Err(CoreError::Cancelled);
    }
    Ok(())
}

// ── Shared Utility Functions ────────────────────────────────────────

/// Truncates a string to the given maximum length, appending `…` if truncated.
///
/// Uses character count (not byte length) to handle multi-byte UTF-8
/// sequences correctly.
///
/// # Examples
///
/// ```
/// use coda_core::session::truncate_str;
///
/// assert_eq!(truncate_str("hello", 10), "hello");
/// assert_eq!(truncate_str("hello world!", 5), "hell\u{2026}");
/// assert_eq!(truncate_str("", 10), "");
/// ```
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    }
}

/// Extracts text delta content from a raw `StreamEvent.event` JSON value.
///
/// Parses `content_block_delta` events with a `text_delta` delta type.
/// Returns `None` for all other event types.
///
/// # Examples
///
/// ```
/// use coda_core::session::extract_text_delta;
///
/// let event = serde_json::json!({
///     "type": "content_block_delta",
///     "delta": { "type": "text_delta", "text": "Hello" }
/// });
/// assert_eq!(extract_text_delta(&event), Some("Hello"));
///
/// let other = serde_json::json!({"type": "message_start"});
/// assert_eq!(extract_text_delta(&other), None);
/// ```
pub fn extract_text_delta(event: &serde_json::Value) -> Option<&str> {
    if event.get("type")?.as_str()? == "content_block_delta" {
        let delta = event.get("delta")?;
        if delta.get("type")?.as_str()? == "text_delta" {
            return delta.get("text")?.as_str();
        }
    }
    None
}

/// Extracts a brief summary from a tool use input for UI display.
///
/// Returns a human-readable string like the file path for Write/Read/Edit,
/// the command for Bash, or the pattern for Grep/Glob. Returns an empty
/// string for unknown tools.
///
/// # Examples
///
/// ```
/// use coda_core::session::summarize_tool_input;
///
/// let input = serde_json::json!({"command": "cargo build"});
/// assert_eq!(summarize_tool_input("Bash", &input), "cargo build");
///
/// let input = serde_json::json!({"file_path": "src/main.rs"});
/// assert_eq!(summarize_tool_input("Write", &input), "src/main.rs");
/// ```
pub fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| truncate_str(s, TOOL_SUMMARY_MAX_LEN))
            .unwrap_or_default(),
        "Write" | "Read" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "Grep" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|s| truncate_str(s, TOOL_SUMMARY_MAX_LEN))
            .unwrap_or_default(),
        "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// Extracts text content from a `ToolResultContent` and appends it to the buffer.
///
/// # Examples
///
/// ```
/// use claude_agent_sdk_rs::ToolResultContent;
/// use coda_core::session::collect_tool_result_text;
///
/// let mut buf = String::new();
/// let content = ToolResultContent::Text("hello".to_string());
/// collect_tool_result_text(Some(&content), &mut buf);
/// assert_eq!(buf, "hello");
/// ```
pub fn collect_tool_result_text(content: Option<&ToolResultContent>, buf: &mut String) {
    match content {
        Some(ToolResultContent::Text(text)) => {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
        Some(ToolResultContent::Blocks(blocks)) => {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
            }
        }
        None => {}
    }
}

/// Returns `true` if the response text contains a known API error pattern.
///
/// Used to detect situations where the API proxy returns authentication
/// or rate-limiting errors as assistant text rather than SDK-level errors.
///
/// # Examples
///
/// ```
/// use coda_core::session::contains_api_error_pattern;
///
/// assert!(contains_api_error_pattern("API Error: 429 Too Many Requests"));
/// assert!(!contains_api_error_pattern("All checks passed successfully"));
/// ```
pub fn contains_api_error_pattern(text: &str) -> bool {
    let lower = text.to_lowercase();
    API_ERROR_PATTERNS
        .iter()
        .any(|pattern| lower.contains(&pattern.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate_str tests ──────────────────────────────────────

    #[test]
    fn test_should_not_truncate_short_string() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_should_truncate_long_string_with_ellipsis() {
        let result = truncate_str("hello world!", 5);
        assert_eq!(result, "hell\u{2026}");
    }

    #[test]
    fn test_should_handle_empty_string_truncation() {
        assert_eq!(truncate_str("", 10), "");
    }

    #[test]
    fn test_should_handle_exact_length_truncation() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    // ── extract_text_delta tests ────────────────────────────────

    #[test]
    fn test_should_extract_text_delta_from_content_block_delta() {
        let event = serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "text_delta",
                "text": "Hello, world!"
            }
        });
        assert_eq!(extract_text_delta(&event), Some("Hello, world!"));
    }

    #[test]
    fn test_should_return_none_for_non_text_delta() {
        let event = serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "input_json_delta",
                "partial_json": "{\"key\":"
            }
        });
        assert_eq!(extract_text_delta(&event), None);
    }

    #[test]
    fn test_should_return_none_for_non_delta_event() {
        let event = serde_json::json!({
            "type": "message_start",
            "message": {}
        });
        assert_eq!(extract_text_delta(&event), None);
    }

    #[test]
    fn test_should_return_none_for_empty_event() {
        let event = serde_json::json!({});
        assert_eq!(extract_text_delta(&event), None);
    }

    #[test]
    fn test_should_return_none_for_missing_text_field() {
        let event = serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "text_delta"
            }
        });
        assert_eq!(extract_text_delta(&event), None);
    }

    // ── summarize_tool_input tests ──────────────────────────────

    #[test]
    fn test_should_summarize_bash_command() {
        let input = serde_json::json!({"command": "cargo build --release"});
        assert_eq!(
            summarize_tool_input("Bash", &input),
            "cargo build --release"
        );
    }

    #[test]
    fn test_should_truncate_long_bash_command() {
        let long_cmd = "a".repeat(100);
        let input = serde_json::json!({"command": long_cmd});
        let summary = summarize_tool_input("Bash", &input);
        assert!(summary.len() <= TOOL_SUMMARY_MAX_LEN + 3); // +3 for `…` UTF-8
        assert!(summary.ends_with('\u{2026}'));
    }

    #[test]
    fn test_should_summarize_write_file_path() {
        let input = serde_json::json!({"file_path": "/src/main.rs", "content": "fn main() {}"});
        assert_eq!(summarize_tool_input("Write", &input), "/src/main.rs");
    }

    #[test]
    fn test_should_summarize_read_file_path() {
        let input = serde_json::json!({"file_path": "/src/lib.rs"});
        assert_eq!(summarize_tool_input("Read", &input), "/src/lib.rs");
    }

    #[test]
    fn test_should_summarize_edit_file_path() {
        let input = serde_json::json!({"file_path": "/src/handler.rs"});
        assert_eq!(summarize_tool_input("Edit", &input), "/src/handler.rs");
    }

    #[test]
    fn test_should_summarize_grep_pattern() {
        let input = serde_json::json!({"pattern": "fn main", "path": "/src"});
        assert_eq!(summarize_tool_input("Grep", &input), "fn main");
    }

    #[test]
    fn test_should_summarize_glob_pattern() {
        let input = serde_json::json!({"pattern": "**/*.rs"});
        assert_eq!(summarize_tool_input("Glob", &input), "**/*.rs");
    }

    #[test]
    fn test_should_return_empty_for_unknown_tool() {
        let input = serde_json::json!({"something": "value"});
        assert_eq!(summarize_tool_input("UnknownTool", &input), "");
    }

    #[test]
    fn test_should_return_empty_when_expected_field_missing() {
        let input = serde_json::json!({"other": "value"});
        assert_eq!(summarize_tool_input("Bash", &input), "");
        assert_eq!(summarize_tool_input("Write", &input), "");
        assert_eq!(summarize_tool_input("Grep", &input), "");
        assert_eq!(summarize_tool_input("Glob", &input), "");
    }

    // ── contains_api_error_pattern tests ────────────────────────

    #[test]
    fn test_should_detect_api_error_403_in_text() {
        let text =
            "I'll start reading the files. Failed to authenticate. API Error: 403 rate limited";
        assert!(contains_api_error_pattern(text));
    }

    #[test]
    fn test_should_detect_rate_limit_in_text() {
        let text = "The API has hit a rate limit, please try again later.";
        assert!(contains_api_error_pattern(text));
    }

    #[test]
    fn test_should_detect_auth_failure_in_text() {
        let text = "Failed to authenticate. Please check your API key.";
        assert!(contains_api_error_pattern(text));
    }

    #[test]
    fn test_should_detect_api_error_429_in_text() {
        let text = "API Error: 429 Too Many Requests";
        assert!(contains_api_error_pattern(text));
    }

    #[test]
    fn test_should_not_flag_normal_response_text() {
        let text = "I've updated the error handling in the authentication module. \
                    The code now properly validates API responses.";
        assert!(!contains_api_error_pattern(text));
    }

    #[test]
    fn test_should_not_flag_empty_text() {
        assert!(!contains_api_error_pattern(""));
    }

    #[test]
    fn test_should_detect_case_insensitive_patterns() {
        assert!(contains_api_error_pattern("FAILED TO AUTHENTICATE"));
        assert!(contains_api_error_pattern("Rate Limit exceeded"));
        assert!(contains_api_error_pattern(
            "Quota Exceeded for this account"
        ));
    }

    // ── collect_tool_result_text tests ──────────────────────────

    #[test]
    fn test_should_collect_text_content() {
        let mut buf = String::new();
        let content = ToolResultContent::Text("hello world".to_string());
        collect_tool_result_text(Some(&content), &mut buf);
        assert_eq!(buf, "hello world");
    }

    #[test]
    fn test_should_collect_blocks_content() {
        let mut buf = String::new();
        let blocks = vec![
            serde_json::json!({"text": "block1"}),
            serde_json::json!({"text": "block2"}),
        ];
        let content = ToolResultContent::Blocks(blocks);
        collect_tool_result_text(Some(&content), &mut buf);
        assert_eq!(buf, "block1\nblock2");
    }

    #[test]
    fn test_should_handle_none_content() {
        let mut buf = String::new();
        collect_tool_result_text(None, &mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_should_append_with_newline_separator() {
        let mut buf = "existing".to_string();
        let content = ToolResultContent::Text("appended".to_string());
        collect_tool_result_text(Some(&content), &mut buf);
        assert_eq!(buf, "existing\nappended");
    }

    // ── AgentResponse tests ─────────────────────────────────────

    #[test]
    fn test_should_return_all_text_combined() {
        let resp = AgentResponse {
            text: "assistant text".to_string(),
            tool_output: "tool output".to_string(),
            result: None,
        };
        let all = resp.all_text();
        assert!(all.contains("assistant text"));
        assert!(all.contains("tool output"));
    }

    #[test]
    fn test_should_return_only_text_when_no_tool_output() {
        let resp = AgentResponse {
            text: "only text".to_string(),
            tool_output: String::new(),
            result: None,
        };
        assert_eq!(resp.all_text(), "only text");
    }

    #[test]
    fn test_should_return_empty_string_for_default_response() {
        let resp = AgentResponse::default();
        assert_eq!(resp.all_text(), "");
    }

    // ── SessionConfig tests ─────────────────────────────────────

    #[test]
    fn test_should_create_session_config() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 50.0,
        };
        assert_eq!(config.idle_timeout_secs, 300);
        assert_eq!(config.tool_execution_timeout_secs, 600);
        assert_eq!(config.idle_retries, 3);
        assert!((config.max_budget_usd - 50.0).abs() < f64::EPSILON);
    }

    // ── validate_response tests ─────────────────────────────────

    #[test]
    fn test_should_accept_valid_response() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 100.0,
        };
        let session = AgentSession::new(
            ClaudeClient::new(
                claude_agent_sdk_rs::ClaudeAgentOptions::builder()
                    .system_prompt("test")
                    .build(),
            ),
            config,
        );
        let resp = AgentResponse {
            text: "Some response".to_string(),
            tool_output: String::new(),
            result: None,
        };
        assert!(session.validate_response(&resp).is_ok());
    }

    #[test]
    fn test_should_reject_error_result() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 100.0,
        };
        let session = AgentSession::new(
            ClaudeClient::new(
                claude_agent_sdk_rs::ClaudeAgentOptions::builder()
                    .system_prompt("test")
                    .build(),
            ),
            config,
        );
        let resp = AgentResponse {
            text: "Error occurred".to_string(),
            tool_output: String::new(),
            result: Some(ResultMessage {
                subtype: "error".to_string(),
                duration_ms: 100,
                duration_api_ms: 80,
                is_error: true,
                num_turns: 1,
                session_id: "test".to_string(),
                total_cost_usd: None,
                usage: None,
                result: None,
                structured_output: None,
            }),
        };
        let err = session.validate_response(&resp).unwrap_err();
        assert!(matches!(err, CoreError::AgentError(_)));
    }

    #[test]
    fn test_should_reject_api_error_pattern() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 100.0,
        };
        let session = AgentSession::new(
            ClaudeClient::new(
                claude_agent_sdk_rs::ClaudeAgentOptions::builder()
                    .system_prompt("test")
                    .build(),
            ),
            config,
        );
        let resp = AgentResponse {
            text: "API Error: 429 Too Many Requests".to_string(),
            tool_output: String::new(),
            result: None,
        };
        let err = session.validate_response(&resp).unwrap_err();
        assert!(matches!(err, CoreError::AgentError(_)));
    }

    #[test]
    fn test_should_detect_budget_exhaustion_on_empty_response() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 10.0,
        };
        let session = AgentSession::new(
            ClaudeClient::new(
                claude_agent_sdk_rs::ClaudeAgentOptions::builder()
                    .system_prompt("test")
                    .build(),
            ),
            config,
        );
        let resp = AgentResponse {
            text: String::new(),
            tool_output: String::new(),
            result: Some(ResultMessage {
                subtype: "success".to_string(),
                duration_ms: 100,
                duration_api_ms: 80,
                is_error: false,
                num_turns: 1,
                session_id: "test".to_string(),
                total_cost_usd: Some(10.5),
                usage: None,
                result: None,
                structured_output: None,
            }),
        };
        let err = session.validate_response(&resp).unwrap_err();
        assert!(matches!(err, CoreError::BudgetExhausted { .. }));
    }

    #[test]
    fn test_should_reject_empty_response_without_budget() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 100.0,
        };
        let session = AgentSession::new(
            ClaudeClient::new(
                claude_agent_sdk_rs::ClaudeAgentOptions::builder()
                    .system_prompt("test")
                    .build(),
            ),
            config,
        );
        let resp = AgentResponse {
            text: String::new(),
            tool_output: String::new(),
            result: None,
        };
        let err = session.validate_response(&resp).unwrap_err();
        assert!(matches!(err, CoreError::AgentError(_)));
    }

    // ── Cancellation tests ──────────────────────────────────────

    #[test]
    fn test_should_detect_cancellation_via_check_cancelled() {
        let config = SessionConfig {
            idle_timeout_secs: 300,
            tool_execution_timeout_secs: 600,
            idle_retries: 3,
            max_budget_usd: 100.0,
        };
        let mut session = AgentSession::new(
            ClaudeClient::new(
                claude_agent_sdk_rs::ClaudeAgentOptions::builder()
                    .system_prompt("test")
                    .build(),
            ),
            config,
        );

        // No token set — should pass
        assert!(session.check_cancelled().is_ok());

        // Token set but not cancelled — should pass
        let token = CancellationToken::new();
        session.set_cancellation_token(token.clone());
        assert!(session.check_cancelled().is_ok());

        // Token cancelled — should return Cancelled
        token.cancel();
        let err = session.check_cancelled().unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));
    }

    #[test]
    fn test_should_return_ok_for_check_token_cancelled_without_token() {
        assert!(check_token_cancelled(&None).is_ok());
    }

    #[test]
    fn test_should_return_ok_for_uncancelled_token() {
        let token = CancellationToken::new();
        assert!(check_token_cancelled(&Some(token)).is_ok());
    }

    #[test]
    fn test_should_return_cancelled_for_triggered_token() {
        let token = CancellationToken::new();
        token.cancel();
        let err = check_token_cancelled(&Some(token)).unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));
    }

    #[tokio::test]
    async fn test_should_resolve_cancel_wait_when_cancelled() {
        let token = CancellationToken::new();
        let token_clone = token.clone();
        let opt = Some(token);

        // Spawn a task that cancels after a brief delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            token_clone.cancel();
        });

        // cancel_wait should resolve once the token is cancelled
        tokio::time::timeout(Duration::from_secs(1), cancel_wait(&opt))
            .await
            .expect("cancel_wait should resolve");
    }

    #[tokio::test]
    async fn test_should_pend_forever_when_cancel_wait_has_no_token() {
        let result = tokio::time::timeout(Duration::from_millis(50), cancel_wait(&None)).await;
        assert!(result.is_err(), "cancel_wait(None) should never resolve");
    }

    // ── send() cancellation tests ─────────────────────────────────

    /// Helper to build a test AgentSession with default config.
    fn test_session() -> AgentSession {
        let options = claude_agent_sdk_rs::ClaudeAgentOptions::builder()
            .system_prompt("test")
            .build();
        let config = SessionConfig {
            idle_timeout_secs: 1,
            tool_execution_timeout_secs: 2,
            idle_retries: 0,
            max_budget_usd: 10.0,
        };
        AgentSession::new(ClaudeClient::new(options), config)
    }

    #[tokio::test]
    async fn test_should_return_cancelled_when_token_is_precancelled_before_send() {
        let mut session = test_session();
        let token = CancellationToken::new();
        token.cancel();
        session.set_cancellation_token(token);

        let err = session.send("hello", None).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Cancelled),
            "send() should short-circuit with Cancelled when token is pre-cancelled"
        );
    }

    #[tokio::test]
    async fn test_should_return_cancelled_during_reconnect_backoff() {
        let mut session = test_session();
        let token = CancellationToken::new();
        session.set_cancellation_token(token.clone());

        // Cancel after a short delay — the backoff sleep (5s) should
        // be interrupted by the cancellation.
        let token_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        let result = session.reconnect_and_resend("hello", None, 1, 3).await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::Cancelled),
            "reconnect_and_resend should return Cancelled when token fires during backoff"
        );
    }

    #[tokio::test]
    async fn test_should_clear_event_sender() {
        let mut session = test_session();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        session.set_event_sender(tx);
        assert!(session.event_tx.is_some());

        session.clear_event_sender();
        assert!(session.event_tx.is_none());
    }

    #[tokio::test]
    async fn test_should_emit_events_only_when_sender_is_set() {
        let mut session = test_session();

        // No sender — should not panic
        session.emit_event(SessionEvent::TurnCompleted { current_turn: 1 });

        // With sender
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        session.set_event_sender(tx);
        session.emit_event(SessionEvent::TurnCompleted { current_turn: 1 });
        let event = rx.try_recv().expect("should receive event");
        assert!(matches!(
            event,
            SessionEvent::TurnCompleted { current_turn: 1 }
        ));

        // After clear
        session.clear_event_sender();
        session.emit_event(SessionEvent::TurnCompleted { current_turn: 2 });
        assert!(
            rx.try_recv().is_err(),
            "should not receive events after clear"
        );
    }
}
