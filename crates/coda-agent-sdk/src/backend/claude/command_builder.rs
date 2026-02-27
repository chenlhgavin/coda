//! Claude CLI command builder.
//!
//! Translates [`AgentOptions`] into CLI arguments for the Claude Code binary.

use crate::options::{AgentOptions, McpServerConfig, McpServersConfig, SandboxSettings};

/// Build the full command-line arguments for invoking the Claude CLI.
///
/// The first element is the CLI path itself; remaining elements are flags/values.
pub fn build_command(cli_path: &str, options: &AgentOptions) -> Vec<String> {
    let mut cmd = vec![
        cli_path.to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];

    match &options.system_prompt {
        None => {
            cmd.push("--system-prompt".to_string());
            cmd.push(String::new());
        }
        Some(crate::options::SystemPromptConfig::String(s)) => {
            cmd.push("--system-prompt".to_string());
            cmd.push(s.clone());
        }
        Some(crate::options::SystemPromptConfig::Preset { append, .. }) => {
            if let Some(a) = append {
                cmd.push("--append-system-prompt".to_string());
                cmd.push(a.clone());
            }
        }
    }

    if let Some(ref tools) = options.tools {
        match tools {
            crate::options::ToolsConfig::List(list) => {
                cmd.push("--tools".to_string());
                cmd.push(if list.is_empty() {
                    String::new()
                } else {
                    list.join(",")
                });
            }
            crate::options::ToolsConfig::Preset { .. } => {
                cmd.push("--tools".to_string());
                cmd.push("default".to_string());
            }
        }
    }

    if !options.allowed_tools.is_empty() {
        cmd.push("--allowedTools".to_string());
        cmd.push(options.allowed_tools.join(","));
    }
    if let Some(t) = options.max_turns {
        cmd.push("--max-turns".to_string());
        cmd.push(t.to_string());
    }
    if let Some(b) = options.max_budget_usd {
        cmd.push("--max-budget-usd".to_string());
        cmd.push(b.to_string());
    }
    if !options.disallowed_tools.is_empty() {
        cmd.push("--disallowedTools".to_string());
        cmd.push(options.disallowed_tools.join(","));
    }
    if let Some(ref m) = options.model {
        cmd.push("--model".to_string());
        cmd.push(m.clone());
    }
    if let Some(ref m) = options.fallback_model {
        cmd.push("--fallback-model".to_string());
        cmd.push(m.clone());
    }
    if let Some(ref m) = options.permission_mode {
        cmd.push("--permission-mode".to_string());
        cmd.push(m.to_string());
    }
    if options.continue_conversation {
        cmd.push("--continue".to_string());
    }
    if let Some(ref r) = options.resume {
        cmd.push("--resume".to_string());
        cmd.push(r.clone());
    }
    if let Some(settings_value) = build_settings_value(options) {
        cmd.push("--settings".to_string());
        cmd.push(settings_value);
    }
    if !options.betas.is_empty() {
        cmd.push("--betas".to_string());
        let betas_str: Vec<String> = options.betas.iter().map(|b| b.to_string()).collect();
        cmd.push(betas_str.join(","));
    }
    for dir in &options.add_dirs {
        cmd.push("--add-dir".to_string());
        cmd.push(dir.to_string_lossy().to_string());
    }
    build_mcp_args(&mut cmd, options.mcp_servers.as_ref());
    if options.include_partial_messages {
        cmd.push("--include-partial-messages".to_string());
    }
    if options.fork_session {
        cmd.push("--fork-session".to_string());
    }
    {
        let sources_value = match options.setting_sources.as_ref() {
            Some(sources) => {
                let strs: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
                strs.join(",")
            }
            None => String::new(),
        };
        cmd.push("--setting-sources".to_string());
        cmd.push(sources_value);
    }
    for plugin in &options.plugins {
        if plugin.type_ == "local" {
            cmd.push("--plugin-dir".to_string());
            cmd.push(plugin.path.clone());
        }
    }
    for (flag, value) in &options.extra_args {
        if let Some(v) = value {
            cmd.push(format!("--{}", flag));
            cmd.push(v.clone());
        } else {
            cmd.push(format!("--{}", flag));
        }
    }
    let resolved_thinking = resolve_max_thinking_tokens(options);
    if let Some(tokens) = resolved_thinking {
        cmd.push("--max-thinking-tokens".to_string());
        cmd.push(tokens.to_string());
    }
    if let Some(ref e) = options.effort {
        cmd.push("--effort".to_string());
        cmd.push(e.to_string());
    }
    if let Some(ref of_val) = options.output_format
        && let Some(obj) = of_val.as_object()
        && obj.get("type").and_then(|v| v.as_str()) == Some("json_schema")
        && let Some(schema) = obj.get("schema")
    {
        cmd.push("--json-schema".to_string());
        cmd.push(schema.to_string());
    }
    if let Some(ref p) = options.permission_prompt_tool_name {
        cmd.push("--permission-prompt-tool".to_string());
        cmd.push(p.clone());
    }

    cmd.push("--input-format".to_string());
    cmd.push("stream-json".to_string());

    cmd
}

/// Converts a `HashMap<String, String>` to a `serde_json::Value::Object`.
fn string_map_to_json(map: &std::collections::HashMap<String, String>) -> serde_json::Value {
    let obj: serde_json::Map<_, _> = map
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    serde_json::Value::Object(obj)
}

fn build_mcp_args(cmd: &mut Vec<String>, mcp_servers: Option<&McpServersConfig>) {
    match mcp_servers {
        Some(McpServersConfig::Dict(servers)) => {
            let mut for_cli = serde_json::Map::new();
            for (name, config) in servers {
                let config_val = match config {
                    McpServerConfig::Sdk(c) => {
                        let mut m = serde_json::Map::new();
                        m.insert(
                            "type".to_string(),
                            serde_json::Value::String("sdk".to_string()),
                        );
                        m.insert(
                            "name".to_string(),
                            serde_json::Value::String(c.name.clone()),
                        );
                        m.insert(
                            "version".to_string(),
                            serde_json::Value::String(c.version.clone()),
                        );
                        serde_json::Value::Object(m)
                    }
                    McpServerConfig::Stdio(c) => {
                        let mut m = serde_json::Map::new();
                        m.insert("type".to_string(), serde_json::json!("stdio"));
                        m.insert("command".to_string(), serde_json::json!(c.command));
                        if let Some(ref args) = c.args {
                            m.insert(
                                "args".to_string(),
                                serde_json::to_value(args).unwrap_or_default(),
                            );
                        }
                        if let Some(ref env) = c.env {
                            m.insert("env".to_string(), string_map_to_json(env));
                        }
                        serde_json::Value::Object(m)
                    }
                    McpServerConfig::Sse(c) => {
                        let mut m = serde_json::Map::new();
                        m.insert("type".to_string(), serde_json::json!("sse"));
                        m.insert("url".to_string(), serde_json::json!(c.url));
                        if let Some(ref headers) = c.headers {
                            m.insert("headers".to_string(), string_map_to_json(headers));
                        }
                        serde_json::Value::Object(m)
                    }
                    McpServerConfig::Http(c) => {
                        let mut m = serde_json::Map::new();
                        m.insert("type".to_string(), serde_json::json!("http"));
                        m.insert("url".to_string(), serde_json::json!(c.url));
                        if let Some(ref headers) = c.headers {
                            m.insert("headers".to_string(), string_map_to_json(headers));
                        }
                        serde_json::Value::Object(m)
                    }
                };
                for_cli.insert(name.clone(), config_val);
            }
            if !for_cli.is_empty() {
                let mcp_config = serde_json::json!({"mcpServers": for_cli});
                cmd.push("--mcp-config".to_string());
                cmd.push(mcp_config.to_string());
            }
        }
        Some(McpServersConfig::Path(path)) => {
            cmd.push("--mcp-config".to_string());
            cmd.push(path.clone());
        }
        None => {}
    }
}

fn build_settings_value(options: &AgentOptions) -> Option<String> {
    let has_settings = options.settings.is_some();
    let has_sandbox = options.sandbox.is_some();

    if !has_settings && !has_sandbox {
        return None;
    }

    if has_settings && !has_sandbox {
        return options.settings.clone();
    }

    let mut settings_obj = serde_json::Map::new();
    if let Some(ref s) = options.settings {
        let s = s.trim();
        if s.starts_with('{') && s.ends_with('}') {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s)
                && let Some(obj) = parsed.as_object()
            {
                for (k, v) in obj {
                    settings_obj.insert(k.clone(), v.clone());
                }
            }
        } else {
            let settings_path = std::path::Path::new(s);
            if settings_path.exists() {
                if let Ok(content) = std::fs::read_to_string(settings_path)
                    && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(obj) = parsed.as_object()
                {
                    for (k, v) in obj {
                        settings_obj.insert(k.clone(), v.clone());
                    }
                }
            } else {
                tracing::warn!("Settings file not found: {}", s);
            }
        }
    }
    if let Some(ref sandbox) = options.sandbox {
        let sandbox_val = sandbox_to_json(sandbox);
        settings_obj.insert("sandbox".to_string(), sandbox_val);
    }
    Some(serde_json::to_string(&settings_obj).unwrap_or_default())
}

fn resolve_max_thinking_tokens(options: &AgentOptions) -> Option<u32> {
    if let Some(ref t) = options.thinking {
        match t {
            crate::options::ThinkingConfig::Adaptive => {
                options.max_thinking_tokens.or(Some(32_000))
            }
            crate::options::ThinkingConfig::Enabled { budget_tokens } => Some(*budget_tokens),
            crate::options::ThinkingConfig::Disabled => Some(0),
        }
    } else {
        options.max_thinking_tokens
    }
}

fn sandbox_to_json(s: &SandboxSettings) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    if let Some(v) = s.enabled {
        m.insert("enabled".to_string(), serde_json::json!(v));
    }
    if let Some(v) = s.auto_allow_bash_if_sandboxed {
        m.insert("autoAllowBashIfSandboxed".to_string(), serde_json::json!(v));
    }
    if let Some(v) = &s.excluded_commands {
        m.insert(
            "excludedCommands".to_string(),
            serde_json::to_value(v).unwrap_or_default(),
        );
    }
    if let Some(v) = s.allow_unsandboxed_commands {
        m.insert("allowUnsandboxedCommands".to_string(), serde_json::json!(v));
    }
    if let Some(ref n) = s.network {
        let mut nm = serde_json::Map::new();
        if let Some(v) = &n.allow_unix_sockets {
            nm.insert(
                "allowUnixSockets".to_string(),
                serde_json::to_value(v).unwrap_or_default(),
            );
        }
        if let Some(v) = n.allow_all_unix_sockets {
            nm.insert("allowAllUnixSockets".to_string(), serde_json::json!(v));
        }
        if let Some(v) = n.allow_local_binding {
            nm.insert("allowLocalBinding".to_string(), serde_json::json!(v));
        }
        if let Some(v) = n.http_proxy_port {
            nm.insert("httpProxyPort".to_string(), serde_json::json!(v));
        }
        if let Some(v) = n.socks_proxy_port {
            nm.insert("socksProxyPort".to_string(), serde_json::json!(v));
        }
        if !nm.is_empty() {
            m.insert("network".to_string(), serde_json::Value::Object(nm));
        }
    }
    if let Some(ref iv) = s.ignore_violations {
        let mut ivm = serde_json::Map::new();
        if let Some(v) = &iv.file {
            ivm.insert(
                "file".to_string(),
                serde_json::to_value(v).unwrap_or_default(),
            );
        }
        if let Some(v) = &iv.network {
            ivm.insert(
                "network".to_string(),
                serde_json::to_value(v).unwrap_or_default(),
            );
        }
        if !ivm.is_empty() {
            m.insert(
                "ignoreViolations".to_string(),
                serde_json::Value::Object(ivm),
            );
        }
    }
    if let Some(v) = s.enable_weaker_nested_sandbox {
        m.insert(
            "enableWeakerNestedSandbox".to_string(),
            serde_json::json!(v),
        );
    }
    serde_json::Value::Object(m)
}
