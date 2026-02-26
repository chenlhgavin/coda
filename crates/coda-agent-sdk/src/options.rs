//! Agent options and builder.
//!
//! Agent options and builder for all backends.
//!
//! [`AgentOptions`] configures all backends. Backend-specific options are in
//! [`CodexOptions`] and [`CursorOptions`].

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::backend::BackendKind;

/// Permission modes for tool execution control.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
}

impl fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::AcceptEdits => write!(f, "acceptEdits"),
            Self::Plan => write!(f, "plan"),
            Self::BypassPermissions => write!(f, "bypassPermissions"),
        }
    }
}

impl From<&str> for PermissionMode {
    fn from(s: &str) -> Self {
        match s {
            "acceptEdits" => Self::AcceptEdits,
            "plan" => Self::Plan,
            "bypassPermissions" => Self::BypassPermissions,
            _ => Self::Default,
        }
    }
}

impl From<String> for PermissionMode {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Effort level for thinking depth.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Max,
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Max => write!(f, "max"),
        }
    }
}

impl From<&str> for Effort {
    fn from(s: &str) -> Self {
        match s {
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "max" => Self::Max,
            _ => Self::Medium,
        }
    }
}

impl From<String> for Effort {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// SDK beta features.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SdkBeta {
    /// Context 1M beta
    #[serde(rename = "context-1m-2025-08-07")]
    Context1M,
    /// Other beta features not yet enumerated
    #[serde(untagged)]
    Other(String),
}

impl fmt::Display for SdkBeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context1M => write!(f, "context-1m-2025-08-07"),
            Self::Other(s) => write!(f, "{}", s),
        }
    }
}

impl From<&str> for SdkBeta {
    fn from(s: &str) -> Self {
        match s {
            "context-1m-2025-08-07" => Self::Context1M,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<String> for SdkBeta {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Setting source types.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingSource {
    User,
    Project,
    Local,
}

impl fmt::Display for SettingSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Project => write!(f, "project"),
            Self::Local => write!(f, "local"),
        }
    }
}

impl From<&str> for SettingSource {
    fn from(s: &str) -> Self {
        match s {
            "user" => Self::User,
            "project" => Self::Project,
            "local" => Self::Local,
            _ => Self::User,
        }
    }
}

impl From<String> for SettingSource {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Agent model selection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentModel {
    Sonnet,
    Opus,
    Haiku,
    Inherit,
}

impl fmt::Display for AgentModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sonnet => write!(f, "sonnet"),
            Self::Opus => write!(f, "opus"),
            Self::Haiku => write!(f, "haiku"),
            Self::Inherit => write!(f, "inherit"),
        }
    }
}

impl From<&str> for AgentModel {
    fn from(s: &str) -> Self {
        match s {
            "sonnet" => Self::Sonnet,
            "opus" => Self::Opus,
            "haiku" => Self::Haiku,
            "inherit" => Self::Inherit,
            _ => Self::Inherit,
        }
    }
}

impl From<String> for AgentModel {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Hook event types.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum HookEvent {
    #[serde(rename = "PreToolUse")]
    PreToolUse,
    #[serde(rename = "PostToolUse")]
    PostToolUse,
    #[serde(rename = "PostToolUseFailure")]
    PostToolUseFailure,
    #[serde(rename = "UserPromptSubmit")]
    UserPromptSubmit,
    #[serde(rename = "Stop")]
    Stop,
    #[serde(rename = "SubagentStop")]
    SubagentStop,
    #[serde(rename = "PreCompact")]
    PreCompact,
    #[serde(rename = "Notification")]
    Notification,
    #[serde(rename = "SubagentStart")]
    SubagentStart,
    #[serde(rename = "PermissionRequest")]
    PermissionRequest,
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PreToolUse => write!(f, "PreToolUse"),
            Self::PostToolUse => write!(f, "PostToolUse"),
            Self::PostToolUseFailure => write!(f, "PostToolUseFailure"),
            Self::UserPromptSubmit => write!(f, "UserPromptSubmit"),
            Self::Stop => write!(f, "Stop"),
            Self::SubagentStop => write!(f, "SubagentStop"),
            Self::PreCompact => write!(f, "PreCompact"),
            Self::Notification => write!(f, "Notification"),
            Self::SubagentStart => write!(f, "SubagentStart"),
            Self::PermissionRequest => write!(f, "PermissionRequest"),
        }
    }
}

impl From<&str> for HookEvent {
    fn from(s: &str) -> Self {
        match s {
            "PreToolUse" => Self::PreToolUse,
            "PostToolUse" => Self::PostToolUse,
            "PostToolUseFailure" => Self::PostToolUseFailure,
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "Stop" => Self::Stop,
            "SubagentStop" => Self::SubagentStop,
            "PreCompact" => Self::PreCompact,
            "Notification" => Self::Notification,
            "SubagentStart" => Self::SubagentStart,
            "PermissionRequest" => Self::PermissionRequest,
            _ => Self::PreToolUse,
        }
    }
}

impl From<String> for HookEvent {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Assistant message error types.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantMessageError {
    AuthenticationFailed,
    BillingError,
    RateLimit,
    InvalidRequest,
    ServerError,
    Unknown,
}

impl fmt::Display for AssistantMessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticationFailed => write!(f, "authentication_failed"),
            Self::BillingError => write!(f, "billing_error"),
            Self::RateLimit => write!(f, "rate_limit"),
            Self::InvalidRequest => write!(f, "invalid_request"),
            Self::ServerError => write!(f, "server_error"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl From<&str> for AssistantMessageError {
    fn from(s: &str) -> Self {
        match s {
            "authentication_failed" => Self::AuthenticationFailed,
            "billing_error" => Self::BillingError,
            "rate_limit" => Self::RateLimit,
            "invalid_request" => Self::InvalidRequest,
            "server_error" => Self::ServerError,
            _ => Self::Unknown,
        }
    }
}

impl From<String> for AssistantMessageError {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

/// Codex-specific options.
#[derive(Debug, Clone, Default)]
pub struct CodexOptions {
    /// Approval policy: `"untrusted"`, `"on-failure"`, `"on-request"`, or `"never"`.
    pub approval_policy: Option<String>,
    /// Sandbox mode: `"read-only"`, `"workspace-write"`, or `"danger-full-access"`.
    pub sandbox_mode: Option<String>,
}

/// Cursor Agent-specific options.
#[derive(Debug, Clone, Default)]
pub struct CursorOptions {
    /// Force-approve all tool calls (`--force` / `--yolo`).
    pub force_approve: bool,
    /// Execution mode: `"plan"` or `"ask"`.
    pub mode: Option<String>,
    /// Trust the current workspace without prompting (`--trust`).
    pub trust_workspace: bool,
}

/// Agent options for all backends.
///
/// This is the primary configuration struct. Use [`BackendKind`] to select
/// the target CLI backend. Options not applicable to the selected backend
/// are validated at runtime and produce [`Error::UnsupportedOptions`](crate::error::Error::UnsupportedOptions).
#[derive(Clone, Default)]
pub struct AgentOptions {
    /// Which backend to use. Defaults to [`BackendKind::Claude`].
    pub backend: Option<BackendKind>,
    pub tools: Option<ToolsConfig>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub system_prompt: Option<SystemPromptConfig>,
    pub permission_mode: Option<PermissionMode>,
    pub model: Option<String>,
    pub fallback_model: Option<String>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub continue_conversation: bool,
    pub resume: Option<String>,
    pub cwd: Option<PathBuf>,
    pub cli_path: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub extra_args: HashMap<String, Option<String>>,
    pub add_dirs: Vec<PathBuf>,
    pub mcp_servers: Option<McpServersConfig>,
    pub include_partial_messages: bool,
    pub fork_session: bool,
    pub setting_sources: Option<Vec<SettingSource>>,
    pub plugins: Vec<SdkPluginConfig>,
    pub max_thinking_tokens: Option<u32>,
    pub effort: Option<Effort>,
    pub output_format: Option<serde_json::Value>,
    pub permission_prompt_tool_name: Option<String>,
    pub max_buffer_size: Option<usize>,
    pub enable_file_checkpointing: bool,
    pub betas: Vec<SdkBeta>,
    pub settings: Option<String>,
    pub sandbox: Option<SandboxSettings>,
    pub user: Option<String>,
    pub agents: Option<HashMap<String, AgentDefinition>>,
    pub thinking: Option<ThinkingConfig>,
    pub can_use_tool: Option<CanUseToolCallback>,
    pub hooks: Option<HashMap<HookEvent, Vec<HookMatcher>>>,
    pub stderr: Option<StderrCallback>,
    /// Codex-specific options.
    pub codex: Option<CodexOptions>,
    /// Cursor Agent-specific options.
    pub cursor: Option<CursorOptions>,
}

impl std::fmt::Debug for AgentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentOptions")
            .field("backend", &self.backend)
            .field("tools", &self.tools)
            .field("allowed_tools", &self.allowed_tools)
            .field("system_prompt", &self.system_prompt)
            .field("permission_mode", &self.permission_mode)
            .field("model", &self.model)
            .field("max_turns", &self.max_turns)
            .field("cwd", &self.cwd)
            .field("mcp_servers", &self.mcp_servers)
            .field(
                "can_use_tool",
                &self.can_use_tool.as_ref().map(|_| "<callback>"),
            )
            .field(
                "hooks",
                &self.hooks.as_ref().map(|h| h.keys().collect::<Vec<_>>()),
            )
            .field("stderr", &self.stderr.as_ref().map(|_| "<callback>"))
            .finish_non_exhaustive()
    }
}

/// Callback for stderr output from CLI. Receives each line.
pub type StderrCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Handler for an SDK MCP tool. Receives input JSON and returns result JSON.
pub type SdkMcpToolHandler = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::error::Result<serde_json::Value>> + Send>,
        > + Send
        + Sync,
>;

/// An SDK MCP tool definition with name, description, input schema, and async handler.
pub struct SdkMcpTool {
    /// Tool name (used in `mcp__<server>__<name>` format).
    pub name: String,
    /// Human-readable description of the tool.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
    /// Async handler invoked when the tool is called.
    pub handler: SdkMcpToolHandler,
}

impl std::fmt::Debug for SdkMcpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdkMcpTool")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .field("handler", &"<handler>")
            .finish()
    }
}

impl Clone for SdkMcpTool {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            handler: Arc::clone(&self.handler),
        }
    }
}

/// Callback for tool permission decisions. Returns PermissionResult.
pub type CanUseToolCallback = Arc<
    dyn Fn(
            String,
            serde_json::Value,
            ToolPermissionContext,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = PermissionResult> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, Default)]
pub struct ToolPermissionContext {
    pub signal: Option<()>,
    pub suggestions: Vec<PermissionUpdate>,
}

#[derive(Debug, Clone)]
pub struct PermissionUpdate {
    pub type_: String,
    pub rules: Option<Vec<PermissionRuleValue>>,
    pub behavior: Option<String>,
    pub mode: Option<String>,
    pub directories: Option<Vec<String>>,
    pub destination: Option<String>,
}

impl PermissionUpdate {
    /// Convert to dictionary format matching TypeScript control protocol (Python to_dict).
    pub fn to_control_protocol_value(&self) -> serde_json::Value {
        let mut result = serde_json::Map::new();
        result.insert("type".to_string(), serde_json::json!(self.type_));
        if let Some(ref d) = self.destination {
            result.insert("destination".to_string(), serde_json::json!(d));
        }
        match self.type_.as_str() {
            "addRules" | "replaceRules" | "removeRules" => {
                if let Some(ref rules) = self.rules {
                    let arr: Vec<serde_json::Value> = rules
                        .iter()
                        .map(|r| {
                            let mut m = serde_json::Map::new();
                            m.insert("toolName".to_string(), serde_json::json!(r.tool_name));
                            m.insert(
                                "ruleContent".to_string(),
                                r.rule_content
                                    .as_ref()
                                    .map(|s| serde_json::Value::String(s.clone()))
                                    .unwrap_or(serde_json::Value::Null),
                            );
                            serde_json::Value::Object(m)
                        })
                        .collect();
                    result.insert("rules".to_string(), serde_json::Value::Array(arr));
                }
                if let Some(ref b) = self.behavior {
                    result.insert("behavior".to_string(), serde_json::json!(b));
                }
            }
            "setMode" => {
                if let Some(ref m) = self.mode {
                    result.insert("mode".to_string(), serde_json::json!(m));
                }
            }
            "addDirectories" | "removeDirectories" => {
                if let Some(ref dirs) = self.directories {
                    result.insert(
                        "directories".to_string(),
                        serde_json::Value::Array(
                            dirs.iter()
                                .map(|s| serde_json::Value::String(s.clone()))
                                .collect(),
                        ),
                    );
                }
            }
            _ => {}
        }
        serde_json::Value::Object(result)
    }
}

#[derive(Debug, Clone)]
pub struct PermissionRuleValue {
    pub tool_name: String,
    pub rule_content: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PermissionResult {
    Allow(PermissionResultAllow),
    Deny(PermissionResultDeny),
}

#[derive(Debug, Clone)]
pub struct PermissionResultAllow {
    pub updated_input: Option<serde_json::Value>,
    pub updated_permissions: Option<Vec<PermissionUpdate>>,
}

#[derive(Debug, Clone)]
pub struct PermissionResultDeny {
    pub message: String,
    pub interrupt: bool,
}

#[derive(Clone)]
pub struct HookMatcher {
    pub matcher: Option<String>,
    pub hooks: Vec<HookCallback>,
    pub timeout: Option<f64>,
}

impl std::fmt::Debug for HookMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookMatcher")
            .field("matcher", &self.matcher)
            .field("hooks", &format!("[{} callbacks]", self.hooks.len()))
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Hook callback: (input, tool_use_id, context) -> HookJSONOutput
pub type HookCallback = Arc<
    dyn Fn(
            serde_json::Value,
            Option<String>,
            HookContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = crate::error::Result<HookJSONOutput>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct HookContext {
    pub signal: Option<()>,
}

#[derive(Debug, Clone)]
pub enum HookJSONOutput {
    Async {
        async_timeout: Option<u64>,
    },
    Sync {
        continue_: Option<bool>,
        suppress_output: Option<bool>,
        stop_reason: Option<String>,
        decision: Option<String>,
        system_message: Option<String>,
        reason: Option<String>,
        hook_specific_output: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentDefinition {
    pub description: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<AgentModel>,
}

#[derive(Debug, Clone)]
pub enum ThinkingConfig {
    Adaptive,
    Enabled { budget_tokens: u32 },
    Disabled,
}

#[derive(Debug, Clone, Default)]
pub struct SandboxSettings {
    pub enabled: Option<bool>,
    pub auto_allow_bash_if_sandboxed: Option<bool>,
    pub excluded_commands: Option<Vec<String>>,
    pub allow_unsandboxed_commands: Option<bool>,
    pub network: Option<SandboxNetworkConfig>,
    pub ignore_violations: Option<SandboxIgnoreViolations>,
    pub enable_weaker_nested_sandbox: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct SandboxNetworkConfig {
    pub allow_unix_sockets: Option<Vec<String>>,
    pub allow_all_unix_sockets: Option<bool>,
    pub allow_local_binding: Option<bool>,
    pub http_proxy_port: Option<u16>,
    pub socks_proxy_port: Option<u16>,
}

#[derive(Debug, Clone, Default)]
pub struct SandboxIgnoreViolations {
    pub file: Option<Vec<String>>,
    pub network: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum McpServersConfig {
    Dict(HashMap<String, McpServerConfig>),
    Path(String),
}

#[derive(Debug, Clone)]
pub enum McpServerConfig {
    Stdio(McpStdioConfig),
    Sse(McpSseConfig),
    Http(McpHttpConfig),
    Sdk(McpSdkConfig),
}

#[derive(Debug, Clone, Default)]
pub struct McpStdioConfig {
    pub command: String,
    pub args: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Default)]
pub struct McpSseConfig {
    pub url: String,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Default)]
pub struct McpHttpConfig {
    pub url: String,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct McpSdkConfig {
    /// Server name.
    pub name: String,
    /// Server version.
    pub version: String,
    /// Tools registered on this SDK MCP server.
    pub tools: Vec<SdkMcpTool>,
}

#[derive(Debug, Clone)]
pub struct SdkPluginConfig {
    /// Plugin type. Currently only "local" is supported.
    pub type_: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub enum ToolsConfig {
    List(Vec<String>),
    Preset { preset: String },
}

impl From<Vec<String>> for ToolsConfig {
    fn from(v: Vec<String>) -> Self {
        ToolsConfig::List(v)
    }
}

impl<const N: usize> From<[&str; N]> for ToolsConfig {
    fn from(arr: [&str; N]) -> Self {
        ToolsConfig::List(arr.into_iter().map(String::from).collect())
    }
}

impl From<HashMap<String, McpServerConfig>> for McpServersConfig {
    fn from(m: HashMap<String, McpServerConfig>) -> Self {
        McpServersConfig::Dict(m)
    }
}

#[derive(Debug, Clone)]
pub enum SystemPromptConfig {
    String(String),
    Preset {
        preset: String,
        append: Option<String>,
    },
}

/// Builder for [`AgentOptions`].
pub struct AgentOptionsBuilder {
    options: AgentOptions,
}

impl AgentOptionsBuilder {
    pub fn new() -> Self {
        Self {
            options: AgentOptions::default(),
        }
    }

    /// Select the backend CLI to use.
    pub fn backend(mut self, kind: BackendKind) -> Self {
        self.options.backend = Some(kind);
        self
    }

    pub fn allowed_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.options.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    pub fn disallowed_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.options.disallowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.options.system_prompt = Some(SystemPromptConfig::String(prompt.into()));
        self
    }

    pub fn permission_mode(mut self, mode: impl Into<PermissionMode>) -> Self {
        self.options.permission_mode = Some(mode.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.options.model = Some(model.into());
        self
    }

    pub fn max_turns(mut self, turns: u32) -> Self {
        self.options.max_turns = Some(turns);
        self
    }

    pub fn max_budget_usd(mut self, budget: f64) -> Self {
        self.options.max_budget_usd = Some(budget);
        self
    }

    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.options.cwd = Some(path.into());
        self
    }

    pub fn cli_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.options.cli_path = Some(path.into());
        self
    }

    pub fn continue_conversation(mut self, continue_conversation: bool) -> Self {
        self.options.continue_conversation = continue_conversation;
        self
    }

    pub fn resume(mut self, session_id: impl Into<String>) -> Self {
        self.options.resume = Some(session_id.into());
        self
    }

    pub fn build(self) -> AgentOptions {
        self.options
    }
}

impl AgentOptionsBuilder {
    pub fn betas(mut self, betas: impl IntoIterator<Item = impl Into<SdkBeta>>) -> Self {
        self.options.betas = betas.into_iter().map(Into::into).collect();
        self
    }

    pub fn settings(mut self, settings: impl Into<String>) -> Self {
        self.options.settings = Some(settings.into());
        self
    }

    pub fn sandbox(mut self, sandbox: SandboxSettings) -> Self {
        self.options.sandbox = Some(sandbox);
        self
    }

    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.options.user = Some(user.into());
        self
    }

    pub fn agents(mut self, agents: HashMap<String, AgentDefinition>) -> Self {
        self.options.agents = Some(agents);
        self
    }

    pub fn thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.options.thinking = Some(thinking);
        self
    }

    pub fn can_use_tool(mut self, callback: CanUseToolCallback) -> Self {
        self.options.can_use_tool = Some(callback);
        self
    }

    pub fn hooks(mut self, hooks: HashMap<HookEvent, Vec<HookMatcher>>) -> Self {
        self.options.hooks = Some(hooks);
        self
    }

    pub fn stderr(mut self, callback: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.options.stderr = Some(Arc::new(callback));
        self
    }

    pub fn mcp_servers(mut self, servers: impl Into<McpServersConfig>) -> Self {
        self.options.mcp_servers = Some(servers.into());
        self
    }

    pub fn tools(mut self, tools: impl Into<ToolsConfig>) -> Self {
        self.options.tools = Some(tools.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.env.insert(key.into(), value.into());
        self
    }

    pub fn extra_arg(mut self, flag: impl Into<String>, value: Option<String>) -> Self {
        self.options.extra_args.insert(flag.into(), value);
        self
    }

    pub fn add_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.options.add_dirs.push(path.into());
        self
    }

    pub fn plugin(mut self, path: impl Into<String>) -> Self {
        self.options.plugins.push(SdkPluginConfig {
            type_: "local".to_string(),
            path: path.into(),
        });
        self
    }

    pub fn max_thinking_tokens(mut self, tokens: u32) -> Self {
        self.options.max_thinking_tokens = Some(tokens);
        self
    }

    pub fn include_partial_messages(mut self, include: bool) -> Self {
        self.options.include_partial_messages = include;
        self
    }

    pub fn enable_file_checkpointing(mut self, enable: bool) -> Self {
        self.options.enable_file_checkpointing = enable;
        self
    }

    /// Set Codex-specific options.
    pub fn codex(mut self, codex_opts: CodexOptions) -> Self {
        self.options.codex = Some(codex_opts);
        self
    }

    /// Set Cursor Agent-specific options.
    pub fn cursor(mut self, cursor_opts: CursorOptions) -> Self {
        self.options.cursor = Some(cursor_opts);
        self
    }

    pub fn setting_sources(mut self, sources: impl IntoIterator<Item = SettingSource>) -> Self {
        self.options.setting_sources = Some(sources.into_iter().collect());
        self
    }

    pub fn output_format(mut self, format: serde_json::Value) -> Self {
        self.options.output_format = Some(format);
        self
    }

    pub fn fallback_model(mut self, model: impl Into<String>) -> Self {
        self.options.fallback_model = Some(model.into());
        self
    }

    pub fn effort(mut self, effort: Effort) -> Self {
        self.options.effort = Some(effort);
        self
    }
}

impl Default for AgentOptionsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentOptions {
    /// Create a builder for [`AgentOptions`].
    pub fn builder() -> AgentOptionsBuilder {
        AgentOptionsBuilder::new()
    }
}
