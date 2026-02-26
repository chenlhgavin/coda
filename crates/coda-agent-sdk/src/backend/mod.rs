//! Backend abstraction layer for multi-CLI support.
//!
//! This module defines the [`Backend`] and [`Session`] traits that abstract
//! over different CLI backends (Claude, Codex). Each backend provides
//! one-shot query and multi-turn session capabilities with varying feature
//! sets described by [`Capabilities`].

pub mod claude;
pub mod codex;

use crate::error::Result;
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use async_trait::async_trait;
use futures::Stream;
use std::fmt;
use std::pin::Pin;

/// Selects which CLI backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum BackendKind {
    /// Claude Code CLI (`claude`).
    #[default]
    Claude,
    /// OpenAI Codex CLI (`codex`).
    Codex,
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Claude => write!(f, "Claude"),
            Self::Codex => write!(f, "Codex"),
        }
    }
}

/// Describes what features a backend supports.
///
/// Used by [`AgentSdkClient`](crate::client::AgentSdkClient) for capability
/// gating: methods that require unsupported features return
/// [`Error::UnsupportedFeature`](crate::error::Error::UnsupportedFeature).
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Claude-style control protocol (control_request / control_response).
    pub control_protocol: bool,
    /// Tool approval callback (`can_use_tool`).
    pub tool_approval: bool,
    /// SDK hook callbacks.
    pub hooks: bool,
    /// In-process SDK MCP server routing.
    pub sdk_mcp_routing: bool,
    /// Long-lived session (stdin/stdout streaming or app-server).
    pub persistent_session: bool,
    /// Turn interruption support.
    pub interrupt: bool,
    /// Runtime configuration changes (`set_model`, `set_permission_mode`).
    pub runtime_config_changes: bool,
}

/// A backend that can drive a CLI tool for agent queries.
///
/// Implementors handle CLI discovery, command building, and protocol translation
/// for a specific CLI backend.
///
/// # Object Safety
///
/// This trait uses `async_trait` for object safety since it is used with
/// `Arc<dyn Backend>` for runtime backend selection.
#[async_trait]
pub trait Backend: Send + Sync + fmt::Debug {
    /// Feature capabilities of this backend.
    fn capabilities(&self) -> &Capabilities;

    /// Human-readable backend name.
    fn name(&self) -> &str;

    /// Validate that the given options are compatible with this backend.
    ///
    /// Returns [`Error::UnsupportedOptions`](crate::error::Error::UnsupportedOptions)
    /// if options require features this backend does not support.
    fn validate_options(&self, options: &AgentOptions) -> Result<()>;

    /// Execute a one-shot query, returning a stream of messages.
    ///
    /// The returned stream yields messages until a [`ResultMessage`](crate::types::ResultMessage)
    /// is received, then terminates.
    fn one_shot_query(
        &self,
        prompt: Prompt,
        options: &AgentOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Message>> + Send>>>;

    /// Create a multi-turn interactive session.
    ///
    /// If `prompt` is provided, the session begins with that initial message.
    async fn create_session(
        &self,
        options: &AgentOptions,
        prompt: Option<Prompt>,
    ) -> Result<Box<dyn Session + Send>>;
}

/// A multi-turn interactive session with a backend.
///
/// Sessions maintain state across multiple exchanges. The lifetime and
/// mechanism vary by backend:
/// - Claude: single long-lived subprocess with stdin/stdout streaming
/// - Codex: `codex app-server` with JSON-RPC 2.0 protocol
#[async_trait]
pub trait Session: Send {
    /// Send a user message in the session.
    async fn send_message(&mut self, prompt: Prompt, session_id: &str) -> Result<()>;

    /// Receive all messages (for monitoring/debugging).
    fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>>;

    /// Receive messages until the next [`ResultMessage`](crate::types::ResultMessage).
    fn receive_response(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>>;

    /// Send a control request and wait for the response.
    ///
    /// For backends without a control protocol, returns
    /// [`Error::UnsupportedFeature`](crate::error::Error::UnsupportedFeature).
    async fn send_control_request(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value>;

    /// Get server info from the initialization handshake, if available.
    async fn get_server_info(&self) -> Option<serde_json::Value>;

    /// Returns whether the backend process is still alive.
    ///
    /// - `Some(true)` — process is alive (stdout pipe open, read task blocking).
    /// - `Some(false)` — process has exited (EOF observed on stdout pipe).
    /// - `None` — liveness unknown (backend does not track process state).
    ///
    /// The default implementation returns `None`.
    fn is_process_alive(&self) -> Option<bool> {
        None
    }

    /// Close the session and release resources.
    async fn close(&mut self) -> Result<()>;
}

/// Create a backend instance for the given kind.
pub fn create_backend(kind: BackendKind) -> Box<dyn Backend> {
    match kind {
        BackendKind::Claude => Box::new(claude::ClaudeBackend::new()),
        BackendKind::Codex => Box::new(codex::CodexBackend::new()),
    }
}
