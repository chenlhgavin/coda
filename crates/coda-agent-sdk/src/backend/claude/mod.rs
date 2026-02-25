//! Claude Code CLI backend.
//!
//! Implements the [`Backend`] and [`Session`] traits for the Claude Code CLI,
//! using a long-lived subprocess with stdin/stdout streaming and the Claude
//! control protocol (control_request/control_response).

pub mod cli_finder;
pub mod command_builder;
pub mod message_parser;
pub mod transport;

use crate::backend::{Backend, Capabilities, Session};
use crate::error::{Error, Result};
use crate::internal::query::Query;
use crate::options::AgentOptions;
use crate::transport::Transport;
use crate::types::{Message, Prompt};
use async_stream::stream;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Capabilities for the Claude backend.
fn claude_capabilities() -> Capabilities {
    Capabilities {
        control_protocol: true,
        tool_approval: true,
        hooks: true,
        sdk_mcp_routing: true,
        persistent_session: true,
        interrupt: true,
        runtime_config_changes: true,
    }
}

/// Backend implementation for the Claude Code CLI.
#[derive(Debug)]
pub struct ClaudeBackend {
    capabilities: Capabilities,
}

impl ClaudeBackend {
    /// Create a new Claude backend.
    pub fn new() -> Self {
        Self {
            capabilities: claude_capabilities(),
        }
    }
}

impl Default for ClaudeBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for ClaudeBackend {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        "Claude"
    }

    fn validate_options(&self, _options: &AgentOptions) -> Result<()> {
        // Claude supports all options; no validation needed.
        Ok(())
    }

    fn one_shot_query(
        &self,
        prompt: Prompt,
        options: &AgentOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Message>> + Send>>> {
        let options = options.clone();
        let stream = stream! {
            // Validate: can_use_tool requires Stream prompt, not Text
            if options.can_use_tool.is_some() && matches!(&prompt, Prompt::Text(_)) {
                yield Err(Error::Other(
                    "can_use_tool callback requires a Stream prompt, not a string prompt. \
                     Use Prompt::Stream for bidirectional communication."
                        .to_string(),
                ));
                return;
            }

            let mut configured_options = options.clone();
            if configured_options.can_use_tool.is_some() {
                if configured_options.permission_prompt_tool_name.is_some() {
                    yield Err(Error::Other(
                        "can_use_tool callback cannot be used with permission_prompt_tool_name. \
                         Please use one or the other."
                            .to_string(),
                    ));
                    return;
                }
                configured_options.permission_prompt_tool_name = Some("stdio".to_string());
            }

            let prompt_str = match &prompt {
                Prompt::Text(s) => s.clone(),
                Prompt::Stream(_) => String::new(),
            };
            let _ = prompt_str; // Used only for logging/debugging

            let mut transport = match transport::ClaudeCliTransport::new(configured_options.clone()) {
                Ok(t) => t,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            if let Err(e) = transport.connect().await {
                yield Err(e);
                return;
            }

            let transport: Box<dyn Transport + Send> = Box::new(transport);
            let mut query = Query::new(transport, &configured_options);

            if let Err(e) = query.initialize(&configured_options).await {
                yield Err(e);
                let _ = query.close().await;
                return;
            }

            match prompt {
                Prompt::Text(ref text) => {
                    if let Err(e) = query.write_user_message(text, "").await {
                        yield Err(e);
                        let _ = query.close().await;
                        return;
                    }
                    if let Err(e) = query.end_input().await {
                        yield Err(e);
                        let _ = query.close().await;
                        return;
                    }
                }
                Prompt::Stream(input_stream) => {
                    if let Err(e) = query.stream_input(input_stream).await {
                        yield Err(e);
                        let _ = query.close().await;
                        return;
                    }
                }
            }

            {
                use futures::StreamExt;
                let mut response_stream = query.receive_response();
                while let Some(item) = response_stream.next().await {
                    match item {
                        Ok(msg) => yield Ok(msg),
                        Err(e) => {
                            yield Err(e);
                            break;
                        }
                    }
                }
            }

            let _ = query.close().await;
        };

        Ok(Box::pin(stream))
    }

    async fn create_session(
        &self,
        options: &AgentOptions,
        prompt: Option<Prompt>,
    ) -> Result<Box<dyn Session + Send>> {
        let mut configured_options = options.clone();
        if configured_options.can_use_tool.is_some() {
            if configured_options.permission_prompt_tool_name.is_some() {
                return Err(Error::Other(
                    "can_use_tool callback cannot be used with permission_prompt_tool_name. \
                     Please use one or the other."
                        .to_string(),
                ));
            }
            configured_options.permission_prompt_tool_name = Some("stdio".to_string());
        }

        let mut transport = transport::ClaudeCliTransport::new(configured_options.clone())?;
        transport.connect().await?;

        let transport: Box<dyn Transport + Send> = Box::new(transport);
        let mut query = Query::new(transport, &configured_options);
        query.initialize(&configured_options).await?;

        if let Some(Prompt::Stream(input_stream)) = prompt {
            query.stream_input(input_stream).await?;
        }

        Ok(Box::new(ClaudeSession { query }))
    }
}

/// Multi-turn session for the Claude backend, wrapping [`Query`].
struct ClaudeSession {
    query: Query,
}

#[async_trait]
impl Session for ClaudeSession {
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
