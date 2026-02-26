//! OpenAI Codex CLI backend.
//!
//! Supports two modes:
//! - One-shot: `codex exec --json <prompt>` (process exits after completion)
//! - Multi-turn: `codex app-server` (long-lived JSON-RPC 2.0 subprocess)

pub mod app_server;
pub mod exec_transport;
pub mod jsonrpc;
pub mod message_parser;

use crate::backend::{Backend, Capabilities, Session};
use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

fn codex_capabilities() -> Capabilities {
    Capabilities {
        control_protocol: false,
        tool_approval: true,
        hooks: false,
        sdk_mcp_routing: false,
        persistent_session: true,
        interrupt: true,
        runtime_config_changes: false,
    }
}

/// Backend implementation for the Codex CLI.
#[derive(Debug)]
pub struct CodexBackend {
    capabilities: Capabilities,
}

impl CodexBackend {
    /// Create a new Codex backend.
    pub fn new() -> Self {
        Self {
            capabilities: codex_capabilities(),
        }
    }
}

impl Default for CodexBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Backend for CodexBackend {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        "Codex"
    }

    fn validate_options(&self, options: &AgentOptions) -> Result<()> {
        let mut unsupported = Vec::new();

        // system_prompt: translated to `developer_instructions` by command builders.
        // hooks: silently ignored — Codex uses its own sandbox system.

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

        if unsupported.is_empty() {
            Ok(())
        } else {
            Err(Error::UnsupportedOptions {
                backend: "Codex".to_string(),
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
        Ok(exec_transport::one_shot_query(prompt, options))
    }

    async fn create_session(
        &self,
        options: &AgentOptions,
        prompt: Option<Prompt>,
    ) -> Result<Box<dyn Session + Send>> {
        self.validate_options(options)?;
        let session = app_server::CodexSession::new(options, prompt).await?;
        Ok(Box::new(session))
    }
}
