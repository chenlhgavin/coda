//! Code Agent SDK for Rust
//!
//! Multi-backend SDK supporting Claude Code and Codex CLIs.
//! See [arch-rust.md](../docs/arch-rust.md) for architecture design.

pub mod backend;
pub mod client;
pub mod error;
pub mod internal;
pub mod options;
pub mod transport;
pub mod types;

// Primary exports
pub use backend::BackendKind;
pub use client::AgentSdkClient;
pub use error::{Error, Result};
pub use internal::message_parser::parse_message;
pub use options::{
    AgentDefinition, AgentModel, AgentOptions, AgentOptionsBuilder, AssistantMessageError,
    CodexOptions, Effort, HookEvent, HookMatcher, McpHttpConfig, McpSdkConfig, McpServerConfig,
    McpServersConfig, McpSseConfig, McpStdioConfig, PermissionMode, PermissionResult,
    PermissionResultAllow, PermissionResultDeny, SandboxSettings, SdkBeta, SdkMcpTool,
    SdkMcpToolHandler, SdkPluginConfig, SettingSource, ToolPermissionContext,
};
pub use types::*;

/// Create an SDK MCP server configuration with tools for in-process execution.
///
/// # Examples
///
/// ```
/// use coda_agent_sdk::{create_sdk_mcp_server, SdkMcpTool, sdk_mcp_tool};
/// use serde_json::json;
///
/// let add_tool = sdk_mcp_tool(
///     "add",
///     "Add two numbers",
///     json!({"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}}}),
///     |args| Box::pin(async move {
///         let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
///         let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
///         Ok(json!({"content": [{"type": "text", "text": format!("{}", a + b)}]}))
///     }),
/// );
///
/// let server = create_sdk_mcp_server("calculator", "1.0.0", vec![add_tool]);
/// assert_eq!(server.name, "calculator");
/// assert_eq!(server.version, "1.0.0");
/// assert_eq!(server.tools.len(), 1);
/// ```
pub fn create_sdk_mcp_server(name: &str, version: &str, tools: Vec<SdkMcpTool>) -> McpSdkConfig {
    McpSdkConfig {
        name: name.to_string(),
        version: version.to_string(),
        tools,
    }
}

/// Convenience builder for creating an [`SdkMcpTool`].
///
/// This is the Rust equivalent of the Python `@tool` decorator.
pub fn sdk_mcp_tool<F>(
    name: &str,
    description: &str,
    input_schema: serde_json::Value,
    handler: F,
) -> SdkMcpTool
where
    F: Fn(
            serde_json::Value,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send>>
        + Send
        + Sync
        + 'static,
{
    SdkMcpTool {
        name: name.to_string(),
        description: description.to_string(),
        input_schema,
        handler: std::sync::Arc::new(handler),
    }
}

/// One-shot query supporting all backends.
///
/// Accepts a string prompt or a [`Prompt`] enum for stream-based input.
/// The backend is selected via [`AgentOptions::backend`].
pub fn query(
    prompt: impl Into<Prompt> + Send + 'static,
    options: Option<AgentOptions>,
) -> impl futures::Stream<Item = Result<Message>> + Send {
    let options = options.unwrap_or_default();
    internal::client::InternalClient::new().process_query(prompt.into(), options)
}
