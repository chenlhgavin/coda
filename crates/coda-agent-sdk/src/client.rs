//! `AgentSdkClient` - bidirectional streaming client with multi-backend support.

use crate::backend::{Backend, BackendKind, Session, create_backend};
use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::transport::Transport;
use crate::types::{Message, Prompt};
use futures::Stream;
use std::pin::Pin;

/// Client for bidirectional, interactive conversations with code agents.
///
/// Supports Claude Code and Codex backends. Methods that
/// require backend-specific features perform capability checks and return
/// [`Error::UnsupportedFeature`] when unsupported.
pub struct AgentSdkClient {
    options: AgentOptions,
    custom_transport: Option<Box<dyn Transport + Send>>,
    backend: Box<dyn Backend>,
    session: Option<Box<dyn Session + Send>>,
}

impl AgentSdkClient {
    /// Create a new client with optional configuration and custom transport.
    ///
    /// # Arguments
    /// * `options` - Configuration options (uses defaults if `None`).
    /// * `custom_transport` - Optional custom transport (uses subprocess CLI if `None`).
    ///   Only applicable to the Claude backend.
    pub fn new(
        options: Option<AgentOptions>,
        custom_transport: Option<Box<dyn Transport + Send>>,
    ) -> Self {
        let options = options.unwrap_or_default();
        let kind = options.backend.unwrap_or(BackendKind::Claude);
        let backend = create_backend(kind);

        Self {
            options,
            custom_transport,
            backend,
            session: None,
        }
    }

    /// Connect to the agent backend.
    ///
    /// Matching Python SDK behavior:
    /// - `None` prompt: connects without sending a message (interactive mode).
    /// - `Prompt::Text`: NOT auto-sent. Use `query()` after connecting.
    /// - `Prompt::Stream`: starts background stream input task (Claude only).
    pub async fn connect(&mut self, prompt: Option<Prompt>) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }

        // Validate: can_use_tool requires Stream prompt, not Text
        if self.options.can_use_tool.is_some()
            && let Some(Prompt::Text(_)) = &prompt
        {
            return Err(Error::Other(
                "can_use_tool callback requires a Stream prompt, not a string prompt. \
                 Use Prompt::Stream for bidirectional communication."
                    .to_string(),
            ));
        }

        // For Claude backend with custom transport, use the legacy Query path
        if self.custom_transport.is_some()
            && self.options.backend.unwrap_or(BackendKind::Claude) == BackendKind::Claude
        {
            return self.connect_claude_legacy(prompt).await;
        }

        let session = self.backend.create_session(&self.options, prompt).await?;
        self.session = Some(session);
        Ok(())
    }

    /// Legacy connect path for Claude with custom transport.
    async fn connect_claude_legacy(&mut self, prompt: Option<Prompt>) -> Result<()> {
        use crate::internal::query::Query;

        let mut options = self.options.clone();
        if options.can_use_tool.is_some() {
            if options.permission_prompt_tool_name.is_some() {
                return Err(Error::Other(
                    "can_use_tool callback cannot be used with permission_prompt_tool_name. \
                     Please use one or the other."
                        .to_string(),
                ));
            }
            options.permission_prompt_tool_name = Some("stdio".to_string());
        }

        let transport = self
            .custom_transport
            .take()
            .ok_or_else(|| Error::Other("Custom transport already consumed".to_string()))?;

        let mut query = Query::new(transport, &options);
        query.initialize(&options).await?;

        if let Some(Prompt::Stream(input_stream)) = prompt {
            query.stream_input(input_stream).await?;
        }

        self.session = Some(Box::new(LegacyQuerySession { query }));
        Ok(())
    }

    /// Send a query to the agent.
    ///
    /// For `Prompt::Text`: writes a user message with the given session_id.
    /// For `Prompt::Stream`: iterates and writes each message.
    pub async fn query(&mut self, prompt: impl Into<Prompt>, session_id: &str) -> Result<()> {
        let session = self.session.as_mut().ok_or(Error::NotConnected)?;
        session.send_message(prompt.into(), session_id).await
    }

    /// Receive all messages (for debugging/monitoring).
    pub fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        if let Some(ref s) = self.session {
            s.receive_messages()
        } else {
            Box::pin(futures::stream::once(async { Err(Error::NotConnected) }))
        }
    }

    /// Receive messages until the next [`ResultMessage`](crate::types::ResultMessage).
    pub fn receive_response(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        if let Some(ref s) = self.session {
            s.receive_response()
        } else {
            Box::pin(futures::stream::once(async { Err(Error::NotConnected) }))
        }
    }

    /// Interrupt the current turn.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFeature`] if the backend does not support interruption.
    pub async fn interrupt(&mut self) -> Result<()> {
        self.require_capability("interrupt", |c| c.interrupt)?;
        let session = self.session.as_mut().ok_or(Error::NotConnected)?;
        session
            .send_control_request(serde_json::json!({"subtype": "interrupt"}))
            .await?;
        Ok(())
    }

    /// Change the permission mode at runtime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFeature`] for non-Claude backends.
    pub async fn set_permission_mode(&mut self, mode: &str) -> Result<()> {
        self.require_capability("set_permission_mode", |c| c.runtime_config_changes)?;
        let session = self.session.as_mut().ok_or(Error::NotConnected)?;
        session
            .send_control_request(serde_json::json!({
                "subtype": "set_permission_mode",
                "mode": mode
            }))
            .await?;
        Ok(())
    }

    /// Change the model at runtime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFeature`] for non-Claude backends.
    pub async fn set_model(&mut self, model: Option<&str>) -> Result<()> {
        self.require_capability("set_model", |c| c.runtime_config_changes)?;
        let session = self.session.as_mut().ok_or(Error::NotConnected)?;
        session
            .send_control_request(serde_json::json!({
                "subtype": "set_model",
                "model": model
            }))
            .await?;
        Ok(())
    }

    /// Rewind files to a previous state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFeature`] for non-Claude backends.
    pub async fn rewind_files(&mut self, user_message_id: &str) -> Result<()> {
        self.require_capability("rewind_files", |c| c.control_protocol)?;
        let session = self.session.as_mut().ok_or(Error::NotConnected)?;
        session
            .send_control_request(serde_json::json!({
                "subtype": "rewind_files",
                "user_message_id": user_message_id
            }))
            .await?;
        Ok(())
    }

    /// Get MCP server status.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedFeature`] for non-Claude backends.
    pub async fn get_mcp_status(&mut self) -> Result<serde_json::Value> {
        self.require_capability("get_mcp_status", |c| c.control_protocol)?;
        let session = self.session.as_mut().ok_or(Error::NotConnected)?;
        session
            .send_control_request(serde_json::json!({"subtype": "mcp_status"}))
            .await
    }

    /// Get server info from the initialization handshake.
    pub async fn get_server_info(&self) -> Result<Option<serde_json::Value>> {
        if let Some(ref s) = self.session {
            Ok(s.get_server_info().await)
        } else {
            Err(Error::NotConnected)
        }
    }

    /// Disconnect from the agent.
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut s) = self.session.take() {
            s.close().await?;
        }
        Ok(())
    }

    /// Check that the backend supports a required capability.
    fn require_capability(
        &self,
        feature: &str,
        check: impl FnOnce(&crate::backend::Capabilities) -> bool,
    ) -> Result<()> {
        let caps = self.backend.capabilities();
        if check(caps) {
            Ok(())
        } else {
            Err(Error::UnsupportedFeature {
                feature: feature.to_string(),
                backend: self.backend.name().to_string(),
            })
        }
    }
}

/// Session wrapper for the legacy Query-based path (custom transport).
struct LegacyQuerySession {
    query: crate::internal::query::Query,
}

#[async_trait::async_trait]
impl Session for LegacyQuerySession {
    async fn send_message(&mut self, prompt: Prompt, session_id: &str) -> Result<()> {
        match prompt {
            Prompt::Text(text) => self.query.write_user_message(&text, session_id).await,
            Prompt::Stream(input_stream) => self.query.stream_input(input_stream).await,
        }
    }

    fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        self.query.receive_messages()
    }

    fn receive_response(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        self.query.receive_response()
    }

    async fn send_control_request(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.query.send_control_request(request).await
    }

    async fn get_server_info(&self) -> Option<serde_json::Value> {
        self.query.get_server_info().await
    }

    async fn close(&mut self) -> Result<()> {
        self.query.close().await
    }
}
