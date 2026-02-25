//! Cursor Agent CLI backend.
//!
//! Supports two modes:
//! - One-shot: `agent --print --output-format stream-json <prompt>`
//! - Multi-turn: spawn-per-turn with `agent --print --resume <chatId>`

pub mod message_parser;
pub mod session;
pub mod transport;

use crate::backend::{Backend, Capabilities, Session};
use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

fn cursor_capabilities() -> Capabilities {
    Capabilities {
        control_protocol: false,
        tool_approval: false,
        hooks: false,
        sdk_mcp_routing: false,
        persistent_session: false,
        interrupt: false,
        runtime_config_changes: false,
    }
}

/// Backend implementation for the Cursor Agent CLI.
#[derive(Debug)]
pub struct CursorBackend {
    capabilities: Capabilities,
}

impl CursorBackend {
    /// Create a new Cursor backend.
    pub fn new() -> Self {
        Self {
            capabilities: cursor_capabilities(),
        }
    }
}

impl Default for CursorBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for CursorBackend {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        "Cursor"
    }

    fn validate_options(&self, options: &AgentOptions) -> Result<()> {
        let mut unsupported = Vec::new();

        if options.system_prompt.is_some() {
            unsupported.push("system_prompt".to_string());
        }
        if options.can_use_tool.is_some() {
            unsupported.push("can_use_tool".to_string());
        }
        if options.hooks.is_some() {
            unsupported.push("hooks".to_string());
        }
        if options.mcp_servers.is_some() {
            unsupported.push("mcp_servers".to_string());
        }
        if options.fork_session {
            unsupported.push("fork_session".to_string());
        }
        if options.setting_sources.is_some() {
            unsupported.push("setting_sources".to_string());
        }
        if !options.plugins.is_empty() {
            unsupported.push("plugins".to_string());
        }
        if options.permission_prompt_tool_name.is_some() {
            unsupported.push("permission_prompt_tool_name".to_string());
        }
        if options.output_format.is_some() {
            unsupported.push("output_format (structured output)".to_string());
        }

        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(Error::UnsupportedOptions {
                backend: "Cursor".to_string(),
                options: unsupported,
            })
        }
    }

    fn one_shot_query(
        &self,
        prompt: Prompt,
        options: &AgentOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<Message>> + Send>>> {
        self.validate_options(options)?;
        Ok(transport::one_shot_query(prompt, options))
    }

    async fn create_session(
        &self,
        options: &AgentOptions,
        prompt: Option<Prompt>,
    ) -> Result<Box<dyn Session + Send>> {
        self.validate_options(options)?;
        let session = session::CursorSession::new(options, prompt).await?;
        Ok(Box::new(session))
    }
}
