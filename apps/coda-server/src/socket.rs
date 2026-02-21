//! Socket Mode WebSocket connection management.
//!
//! [`SocketClient`] manages the outbound WebSocket connection to Slack's
//! Socket Mode endpoint. It handles connection lifecycle, envelope
//! acknowledgment, and automatic reconnection with exponential backoff.

use std::future::Future;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{info, warn};

use crate::dispatch::{self, Envelope, ParsedMessage};
use crate::error::ServerError;
use crate::slack_client::SlackClient;

/// Initial backoff delay for reconnection.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// Maximum backoff delay cap for reconnection.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Type alias for the WebSocket stream with optional TLS.
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Manages the WebSocket connection to Slack Socket Mode.
///
/// Connects using the app-level token (`xapp-...`) via `apps.connections.open`,
/// then maintains the WebSocket connection. When the connection drops, it
/// automatically reconnects with exponential backoff (1s, 2s, 4s, ..., 30s cap).
///
/// # Examples
///
/// ```no_run
/// use coda_server::socket::SocketClient;
/// use coda_server::slack_client::SlackClient;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let slack = SlackClient::new("xoxb-token".into());
/// let socket = SocketClient::new("xapp-token".into(), slack);
/// let (_tx, rx) = tokio::sync::watch::channel(false);
/// socket.run(|_envelope| async {}, rx).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct SocketClient {
    app_token: String,
    slack: SlackClient,
}

impl SocketClient {
    /// Creates a new Socket Mode client.
    ///
    /// The `app_token` is used to call `apps.connections.open` for obtaining
    /// the WebSocket URL. The `slack` client is used for the API call itself.
    pub fn new(app_token: String, slack: SlackClient) -> Self {
        Self { app_token, slack }
    }

    /// Connects and runs the event loop until shutdown.
    ///
    /// For each received envelope:
    /// 1. Acknowledges within 3s by sending back `{ "envelope_id": "..." }`
    /// 2. Spawns the handler as a background task
    ///
    /// Auto-reconnects on disconnect with exponential backoff (1s to 30s cap).
    /// Exits cleanly when the shutdown signal is received.
    ///
    /// # Errors
    ///
    /// Returns `ServerError` if the initial connection setup fails and
    /// shutdown is not requested.
    pub async fn run<F, Fut>(
        &self,
        handler: F,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), ServerError>
    where
        F: Fn(Envelope) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut backoff = INITIAL_BACKOFF;
        let mut tasks = tokio::task::JoinSet::new();

        loop {
            if *shutdown.borrow() {
                info!("Shutdown requested, exiting socket loop");
                break;
            }

            match self
                .connect_and_run(&handler, &mut shutdown, &mut tasks)
                .await
            {
                Ok(ConnectionExit::Shutdown) => {
                    info!("Shutdown signal received, closing connection");
                    break;
                }
                Ok(ConnectionExit::Disconnect) => {
                    info!(
                        backoff_secs = backoff.as_secs(),
                        "Disconnected, reconnecting after backoff"
                    );
                    // Reset backoff on clean disconnect (Slack requested it)
                    backoff = INITIAL_BACKOFF;
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        backoff_secs = backoff.as_secs(),
                        "Connection error, reconnecting after backoff"
                    );
                }
            }

            // Wait for backoff duration or shutdown signal
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("Shutdown during backoff, exiting");
                        break;
                    }
                }
            }

            // Exponential backoff: double up to MAX_BACKOFF
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }

        // Abort all in-flight handler tasks so the runtime can exit
        let task_count = tasks.len();
        if task_count > 0 {
            info!(task_count, "Aborting in-flight handler tasks");
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }

        Ok(())
    }

    /// Connects to Slack and runs the event loop for a single connection.
    async fn connect_and_run<F, Fut>(
        &self,
        handler: &F,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
        tasks: &mut tokio::task::JoinSet<()>,
    ) -> Result<ConnectionExit, ServerError>
    where
        F: Fn(Envelope) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // 1. Get WebSocket URL via apps.connections.open
        let wss_url = self.slack.connections_open(&self.app_token).await?;
        info!("Obtained WebSocket URL, connecting...");

        // 2. Connect via WebSocket (explicit type annotation for TLS stream)
        let (ws_stream, _response): (WsStream, _) = connect_async(wss_url.as_str())
            .await
            .map_err(|e| ServerError::WebSocket(format!("WebSocket connect failed: {e}")))?;
        info!("WebSocket connected to Slack Socket Mode");

        // 3. Split into reader and writer
        let (mut write, mut read) = ws_stream.split();

        // 4. Event loop
        loop {
            tokio::select! {
                msg = read.next() => {
                    let Some(msg_result) = msg else {
                        info!("WebSocket stream ended");
                        return Ok(ConnectionExit::Disconnect);
                    };

                    let ws_msg = msg_result.map_err(|e| {
                        ServerError::WebSocket(format!("WebSocket read error: {e}"))
                    })?;

                    match ws_msg {
                        WsMessage::Text(ref text) => {
                            let text_ref: &str = text;
                            match dispatch::parse_message(text_ref)? {
                                Some(ParsedMessage::Hello) => {
                                    // Already logged in parse_message
                                }
                                Some(ParsedMessage::Disconnect) => {
                                    return Ok(ConnectionExit::Disconnect);
                                }
                                Some(ParsedMessage::Envelope(envelope)) => {
                                    // Ack immediately
                                    let ack = serde_json::json!({
                                        "envelope_id": &envelope.envelope_id
                                    });
                                    let ack_msg = WsMessage::Text(ack.to_string());
                                    write.send(ack_msg).await.map_err(|e| {
                                        ServerError::WebSocket(format!("Ack send failed: {e}"))
                                    })?;

                                    // Spawn handler as tracked task
                                    let fut = handler(envelope);
                                    tasks.spawn(fut);
                                }
                                None => {
                                    // Unknown message type, already logged
                                }
                            }
                        }
                        WsMessage::Ping(data) => {
                            write.send(WsMessage::Pong(data)).await.map_err(|e| {
                                ServerError::WebSocket(format!("Pong send failed: {e}"))
                            })?;
                        }
                        WsMessage::Close(_) => {
                            info!("Received WebSocket close frame");
                            return Ok(ConnectionExit::Disconnect);
                        }
                        _ => {
                            // Binary, Pong, Frame — ignore
                        }
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return Ok(ConnectionExit::Shutdown);
                    }
                }
            }
        }
    }
}

/// Reason the event loop exited a single connection.
enum ConnectionExit {
    /// Clean shutdown requested by the application.
    Shutdown,
    /// Slack requested a disconnect or connection was lost.
    Disconnect,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_create_socket_client() {
        let slack = SlackClient::new("xoxb-test".into());
        let socket = SocketClient::new("xapp-1-test".into(), slack);
        let debug = format!("{socket:?}");
        assert!(debug.contains("SocketClient"));
    }

    #[test]
    fn test_should_verify_backoff_constants() {
        assert_eq!(INITIAL_BACKOFF, Duration::from_secs(1));
        assert_eq!(MAX_BACKOFF, Duration::from_secs(30));
    }

    #[test]
    fn test_should_cap_backoff_at_max() {
        let mut backoff = INITIAL_BACKOFF;
        for _ in 0..10 {
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
        assert_eq!(backoff, MAX_BACKOFF);
    }

    #[test]
    fn test_should_follow_exponential_backoff_sequence() {
        let mut backoff = INITIAL_BACKOFF;
        let mut sequence = vec![backoff];
        for _ in 0..6 {
            backoff = (backoff * 2).min(MAX_BACKOFF);
            sequence.push(backoff);
        }
        // 1s, 2s, 4s, 8s, 16s, 30s (capped), 30s (capped)
        assert_eq!(sequence[0], Duration::from_secs(1));
        assert_eq!(sequence[1], Duration::from_secs(2));
        assert_eq!(sequence[2], Duration::from_secs(4));
        assert_eq!(sequence[3], Duration::from_secs(8));
        assert_eq!(sequence[4], Duration::from_secs(16));
        assert_eq!(sequence[5], Duration::from_secs(30));
        assert_eq!(sequence[6], Duration::from_secs(30));
    }
}
