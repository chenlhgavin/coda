//! Query - control protocol handler for bidirectional streaming.

use crate::error::{Error, Result};
use crate::internal::message_parser::parse_message;
use crate::options::{
    HookCallback, HookContext, HookEvent, HookJSONOutput, HookMatcher, McpSdkConfig,
    PermissionResult, ToolPermissionContext,
};
use crate::transport::Transport;
use crate::types::Message;
use async_stream::stream;
use futures::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, mpsc};

const INITIALIZE_TIMEOUT_SECS: u64 = 60;
const MESSAGE_BUFFER_SIZE: usize = 100;

#[derive(Debug, Clone)]
enum ControlMessage {
    Data(serde_json::Value),
    End,
    Error(String),
}

/// Query handles the control protocol for ClaudeSDKClient.
pub struct Query {
    write_tx: Option<mpsc::Sender<String>>,
    message_tx: broadcast::Sender<ControlMessage>,
    request_counter: AtomicU64,
    init_result: tokio::sync::RwLock<Option<serde_json::Value>>,
}

impl Query {
    pub fn new(
        mut transport: Box<dyn Transport + Send>,
        options: &crate::options::AgentOptions,
    ) -> Self {
        let (message_tx, _) = broadcast::channel(MESSAGE_BUFFER_SIZE);
        let (write_tx, mut write_rx) = mpsc::channel::<String>(64);

        let mut read_stream = transport.read_messages();
        let msg_tx = message_tx.clone();
        let can_use_tool = options.can_use_tool.clone();
        let hook_callbacks = build_hook_callbacks(options.hooks.as_ref());
        let sdk_mcp_servers = extract_sdk_mcp_servers(options);

        tokio::spawn(async move {
            while let Some(s) = write_rx.recv().await {
                let _ = transport.write(&format!("{}\n", s)).await;
            }
            let _ = transport.end_input().await;
            let _ = transport.close().await;
        });

        let write_tx_for_read = write_tx.clone();
        tokio::spawn(async move {
            use futures::StreamExt;

            while let Some(item) = read_stream.next().await {
                match item {
                    Ok(data) => {
                        let msg_type = data.get("type").and_then(|v| v.as_str());
                        if msg_type == Some("control_cancel_request") {
                            // Handle cancel requests - currently just ignored
                            continue;
                        }
                        if msg_type == Some("control_request") {
                            let request_id = data
                                .get("request_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if let Err(e) = handle_control_request(
                                &data,
                                &write_tx_for_read,
                                can_use_tool.as_ref(),
                                hook_callbacks.as_ref(),
                                sdk_mcp_servers.as_ref(),
                            )
                            .await
                            {
                                let _ = write_tx_for_read
                                    .send(format_control_error(&request_id, &e.to_string()))
                                    .await;
                            }
                            continue;
                        }
                        if msg_type == Some("end") {
                            let _ = msg_tx.send(ControlMessage::End);
                            break;
                        }
                        if msg_type == Some("error") {
                            let err = data
                                .get("error")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let _ = msg_tx.send(ControlMessage::Error(err));
                            break;
                        }
                        let _ = msg_tx.send(ControlMessage::Data(data));
                    }
                    Err(e) => {
                        let _ = msg_tx.send(ControlMessage::Error(e.to_string()));
                        break;
                    }
                }
            }
            let _ = msg_tx.send(ControlMessage::End);
        });

        Self {
            write_tx: Some(write_tx),
            message_tx,
            request_counter: AtomicU64::new(0),
            init_result: tokio::sync::RwLock::new(None),
        }
    }

    pub async fn initialize(&mut self, options: &crate::options::AgentOptions) -> Result<()> {
        let request_id = self.next_request_id();
        let agents_json = options.agents.as_ref().and_then(|a| {
            let m: serde_json::Map<_, _> = a
                .iter()
                .filter_map(|(name, def)| serde_json::to_value(def).ok().map(|v| (name.clone(), v)))
                .collect();
            if m.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(m))
            }
        });

        let hooks_config = build_hooks_config_for_initialize(options.hooks.as_ref());

        let init_request = serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": {
                "subtype": "initialize",
                "hooks": hooks_config,
                "agents": agents_json
            }
        });

        // Subscribe before sending to avoid missing fast responses.
        let mut rx = self.message_tx.subscribe();
        self.write_tx
            .as_ref()
            .ok_or_else(|| Error::Other("Query closed".to_string()))?
            .send(serde_json::to_string(&init_request)?)
            .await
            .map_err(|_| Error::Other("Write channel closed".to_string()))?;

        let timeout = tokio::time::Duration::from_secs(INITIALIZE_TIMEOUT_SECS);

        tokio::time::timeout(timeout, async {
            loop {
                match rx.recv().await {
                    Ok(ControlMessage::Data(data)) => {
                        let resp = data.get("response").and_then(|v| v.as_object());
                        let req_id = resp
                            .and_then(|r| r.get("request_id"))
                            .and_then(|v| v.as_str());
                        if req_id == Some(&request_id) {
                            let result = data
                                .get("response")
                                .and_then(|r| r.get("response"))
                                .cloned();
                            *self.init_result.write().await = result;
                            return Ok(());
                        }
                        continue;
                    }
                    Ok(ControlMessage::End) => {
                        return Err(Error::Other(
                            "Stream ended before initialize response".to_string(),
                        ));
                    }
                    Ok(ControlMessage::Error(e)) => {
                        return Err(Error::Other(e));
                    }
                    Err(_) => return Err(Error::Other("Channel closed".to_string())),
                }
            }
        })
        .await
        .map_err(|_| Error::ControlTimeout("initialize".to_string()))?
    }

    fn next_request_id(&self) -> String {
        let n = self.request_counter.fetch_add(1, Ordering::SeqCst);
        let hash = n.wrapping_mul(2_654_435_761) & 0xFFFF_FFFF;
        format!("req_{}_{:08x}", n, hash)
    }

    pub async fn send_control_request(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request_id = self.next_request_id();
        let full_request = serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": request
        });

        // Subscribe before sending to avoid missing fast responses.
        let mut rx = self.message_tx.subscribe();
        self.write_tx
            .as_ref()
            .ok_or_else(|| Error::Other("Query closed".to_string()))?
            .send(serde_json::to_string(&full_request)?)
            .await
            .map_err(|_| Error::Other("Write channel closed".to_string()))?;

        let timeout = tokio::time::Duration::from_secs(60);

        tokio::time::timeout(timeout, async {
            loop {
                match rx.recv().await {
                    Ok(ControlMessage::Data(data)) => {
                        let resp = data.get("response").and_then(|v| v.as_object());
                        let req_id = resp
                            .and_then(|r| r.get("request_id"))
                            .and_then(|v| v.as_str());
                        if req_id == Some(&request_id) {
                            if resp.and_then(|r| r.get("subtype")).and_then(|v| v.as_str())
                                == Some("error")
                            {
                                let err = resp
                                    .and_then(|r| r.get("error"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Unknown");
                                return Err(Error::Other(err.to_string()));
                            }
                            let result = resp
                                .and_then(|r| r.get("response"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            return Ok(result);
                        }
                        continue;
                    }
                    Ok(ControlMessage::End) => {
                        return Err(Error::Other("Stream ended".to_string()));
                    }
                    Ok(ControlMessage::Error(e)) => {
                        return Err(Error::Other(e));
                    }
                    Err(_) => return Err(Error::Other("Channel closed".to_string())),
                }
            }
        })
        .await
        .map_err(|_| Error::ControlTimeout(request_id))?
    }

    pub fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let mut rx = self.message_tx.subscribe();

        let stream = stream! {
            loop {
                match rx.recv().await {
                    Ok(ControlMessage::Data(data)) => {
                        match parse_message(&data) {
                            Ok(Some(m)) => yield Ok(m),
                            Ok(None) => continue, // Forward-compatible: skip unknown types
                            Err(e) => {
                                yield Err(e);
                                continue;
                            }
                        }
                    }
                    Ok(ControlMessage::End) => break,
                    Ok(ControlMessage::Error(e)) => {
                        yield Err(Error::Other(e));
                        break;
                    }
                    Err(_) => break,
                }
            }
        };

        Box::pin(stream)
    }

    pub fn receive_response(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let mut rx = self.message_tx.subscribe();

        let stream = stream! {
            loop {
                match rx.recv().await {
                    Ok(ControlMessage::Data(data)) => {
                        match parse_message(&data) {
                            Ok(Some(m)) => {
                                let is_result = matches!(&m, Message::Result(_));
                                yield Ok(m);
                                if is_result {
                                    break;
                                }
                            }
                            Ok(None) => continue, // Forward-compatible: skip unknown types
                            Err(e) => {
                                yield Err(e);
                                continue;
                            }
                        }
                    }
                    Ok(ControlMessage::End) => break,
                    Ok(ControlMessage::Error(e)) => {
                        yield Err(Error::Other(e));
                        break;
                    }
                    Err(_) => break,
                }
            }
        };

        Box::pin(stream)
    }

    pub async fn get_server_info(&self) -> Option<serde_json::Value> {
        self.init_result.read().await.clone()
    }

    pub async fn write_user_message(&mut self, prompt: &str, session_id: &str) -> Result<()> {
        let user_message = serde_json::json!({
            "type": "user",
            "session_id": session_id,
            "message": {"role": "user", "content": prompt},
            "parent_tool_use_id": serde_json::Value::Null
        });
        self.write_tx
            .as_ref()
            .ok_or_else(|| Error::Other("Query closed".to_string()))?
            .send(serde_json::to_string(&user_message)?)
            .await
            .map_err(|_| Error::Other("Write channel closed".to_string()))?;
        Ok(())
    }

    /// Stream input messages from an async stream to the transport.
    ///
    /// Each message from the stream is written as a JSON line. This method spawns
    /// a background task that iterates the stream and closes stdin when done.
    /// Matches the Python SDK's `stream_input()` method.
    pub async fn stream_input(
        &mut self,
        input_stream: std::pin::Pin<Box<dyn futures::Stream<Item = serde_json::Value> + Send>>,
    ) -> Result<()> {
        let write_tx = self
            .write_tx
            .clone()
            .ok_or_else(|| Error::Other("Query closed".to_string()))?;

        // Spawn a background task to iterate the stream and write messages
        let write_tx_for_close = self.write_tx.take();
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = input_stream;
            while let Some(msg) = stream.next().await {
                if let Ok(json_str) = serde_json::to_string(&msg)
                    && write_tx.send(json_str).await.is_err()
                {
                    break;
                }
            }
            // Close the write channel when stream ends (triggers stdin close)
            drop(write_tx);
            drop(write_tx_for_close);
        });

        Ok(())
    }

    /// End the input stream (signals the transport to close stdin).
    /// For one-shot queries, this is called after sending the user message.
    pub async fn end_input(&mut self) -> Result<()> {
        drop(self.write_tx.take());
        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        drop(self.write_tx.take());
        Ok(())
    }
}

fn build_hooks_config_for_initialize(
    hooks: Option<&HashMap<HookEvent, Vec<HookMatcher>>>,
) -> serde_json::Value {
    let hooks = match hooks {
        Some(h) if !h.is_empty() => h,
        _ => return serde_json::Value::Null,
    };
    let mut config = serde_json::Map::new();
    let mut next_id = 0u32;
    for (event, matchers) in hooks {
        if matchers.is_empty() {
            continue;
        }
        let matcher_configs: Vec<serde_json::Value> = matchers
            .iter()
            .map(|matcher| {
                let ids: Vec<_> = (0..matcher.hooks.len())
                    .map(|_| {
                        let id = format!("hook_{}", next_id);
                        next_id += 1;
                        serde_json::Value::String(id)
                    })
                    .collect();
                let mut m = serde_json::Map::new();
                m.insert(
                    "matcher".to_string(),
                    serde_json::to_value(&matcher.matcher).unwrap_or(serde_json::Value::Null),
                );
                m.insert("hookCallbackIds".to_string(), serde_json::Value::Array(ids));
                if let Some(t) = matcher.timeout {
                    m.insert("timeout".to_string(), serde_json::json!(t));
                }
                serde_json::Value::Object(m)
            })
            .collect();
        config.insert(event.to_string(), serde_json::Value::Array(matcher_configs));
    }
    if config.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(config)
    }
}

fn build_hook_callbacks(
    hooks: Option<&HashMap<HookEvent, Vec<HookMatcher>>>,
) -> Option<HashMap<String, HookCallback>> {
    let hooks = hooks?;
    let mut map = HashMap::new();
    let mut next_id = 0u32;
    for matchers in hooks.values() {
        for matcher in matchers {
            for hook in &matcher.hooks {
                let id = format!("hook_{}", next_id);
                next_id += 1;
                map.insert(id, Arc::clone(hook));
            }
        }
    }
    if map.is_empty() { None } else { Some(map) }
}

fn format_control_error(request_id: &str, error: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error
        }
    }))
    .unwrap_or_default()
}

fn parse_permission_suggestions(
    value: Option<&serde_json::Value>,
) -> Vec<crate::options::PermissionUpdate> {
    let arr = match value.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return vec![],
    };
    arr.iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let type_ = obj.get("type")?.as_str()?.to_string();
            let rules = obj.get("rules").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|r| {
                        let robj = r.as_object()?;
                        Some(crate::options::PermissionRuleValue {
                            tool_name: robj.get("toolName")?.as_str()?.to_string(),
                            rule_content: robj
                                .get("ruleContent")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                        })
                    })
                    .collect()
            });
            let behavior = obj
                .get("behavior")
                .and_then(|v| v.as_str())
                .map(String::from);
            let mode = obj.get("mode").and_then(|v| v.as_str()).map(String::from);
            let directories = obj
                .get("directories")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                });
            let destination = obj
                .get("destination")
                .and_then(|v| v.as_str())
                .map(String::from);
            Some(crate::options::PermissionUpdate {
                type_,
                rules,
                behavior,
                mode,
                directories,
                destination,
            })
        })
        .collect()
}

async fn handle_control_request(
    data: &serde_json::Value,
    write_tx: &mpsc::Sender<String>,
    can_use_tool: Option<&crate::options::CanUseToolCallback>,
    hook_callbacks: Option<&HashMap<String, HookCallback>>,
    sdk_mcp_servers: Option<&Arc<HashMap<String, McpSdkConfig>>>,
) -> Result<()> {
    let request_id = data
        .get("request_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let request_data = data
        .get("request")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::Other("control_request missing request".to_string()))?;
    let subtype = request_data
        .get("subtype")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other("control_request missing subtype".to_string()))?;

    let response_data = match subtype {
        "can_use_tool" => {
            let cb = can_use_tool
                .ok_or_else(|| Error::Other("can_use_tool callback not configured".to_string()))?;
            let tool_name = request_data
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let original_input = request_data
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let suggestions =
                parse_permission_suggestions(request_data.get("permission_suggestions"));
            let ctx = ToolPermissionContext {
                signal: None,
                suggestions,
            };
            let result = cb(tool_name, original_input.clone(), ctx).await;
            match result {
                PermissionResult::Allow(a) => {
                    let mut m = serde_json::Map::new();
                    m.insert("behavior".to_string(), serde_json::json!("allow"));
                    m.insert(
                        "updatedInput".to_string(),
                        a.updated_input.unwrap_or(original_input),
                    );
                    if let Some(ref perms) = a.updated_permissions {
                        m.insert(
                            "updatedPermissions".to_string(),
                            serde_json::Value::Array(
                                perms
                                    .iter()
                                    .map(|p| p.to_control_protocol_value())
                                    .collect(),
                            ),
                        );
                    }
                    serde_json::Value::Object(m)
                }
                PermissionResult::Deny(d) => {
                    let mut m = serde_json::Map::new();
                    m.insert("behavior".to_string(), serde_json::json!("deny"));
                    m.insert("message".to_string(), serde_json::json!(d.message));
                    if d.interrupt {
                        m.insert("interrupt".to_string(), serde_json::json!(true));
                    }
                    serde_json::Value::Object(m)
                }
            }
        }
        "hook_callback" => {
            let callback_id = request_data
                .get("callback_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Other("hook_callback missing callback_id".to_string()))?;
            let callbacks =
                hook_callbacks.ok_or_else(|| Error::Other("hooks not configured".to_string()))?;
            let cb = callbacks
                .get(callback_id)
                .ok_or_else(|| Error::Other(format!("Hook callback {} not found", callback_id)))?;
            let input = request_data
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let tool_use_id = request_data
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            let ctx = HookContext { signal: None };
            let output = cb(input, tool_use_id, ctx).await?;
            hook_output_to_json(&output)
        }
        "mcp_message" => handle_sdk_mcp_request(request_data, sdk_mcp_servers).await?,
        _ => {
            return Err(Error::Other(format!(
                "Unsupported control_request subtype: {}",
                subtype
            )));
        }
    };

    let success_response = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response_data
        }
    });
    write_tx
        .send(serde_json::to_string(&success_response)?)
        .await
        .map_err(|_| Error::Other("Write channel closed".to_string()))?;
    Ok(())
}

/// Extract SDK MCP server configs from options for in-process routing.
fn extract_sdk_mcp_servers(
    options: &crate::options::AgentOptions,
) -> Option<Arc<HashMap<String, McpSdkConfig>>> {
    let servers = match options.mcp_servers.as_ref()? {
        crate::options::McpServersConfig::Dict(dict) => dict,
        crate::options::McpServersConfig::Path(_) => return None,
    };

    let sdk_servers: HashMap<String, McpSdkConfig> = servers
        .iter()
        .filter_map(|(name, config)| match config {
            crate::options::McpServerConfig::Sdk(sdk_config) => {
                Some((name.clone(), sdk_config.clone()))
            }
            _ => None,
        })
        .collect();

    if sdk_servers.is_empty() {
        None
    } else {
        Some(Arc::new(sdk_servers))
    }
}

/// Handle an incoming SDK MCP JSONRPC request by routing to the appropriate in-process server.
async fn handle_sdk_mcp_request(
    request_data: &serde_json::Map<String, serde_json::Value>,
    sdk_mcp_servers: Option<&Arc<HashMap<String, McpSdkConfig>>>,
) -> Result<serde_json::Value> {
    let servers =
        sdk_mcp_servers.ok_or_else(|| Error::Other("No SDK MCP servers configured".to_string()))?;

    let server_name = request_data
        .get("server_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other("mcp_message missing server_name".to_string()))?;

    let message = request_data
        .get("message")
        .ok_or_else(|| Error::Other("mcp_message missing message".to_string()))?;

    let server = servers
        .get(server_name)
        .ok_or_else(|| Error::Other(format!("SDK MCP server '{}' not found", server_name)))?;

    let method = message.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let jsonrpc_id = message
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let params = message
        .get("params")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let jsonrpc_result = match method {
        "initialize" => {
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": server.name,
                    "version": server.version
                }
            })
        }
        "notifications/initialized" => {
            // Acknowledge with empty result
            serde_json::Value::Object(serde_json::Map::new())
        }
        "tools/list" => {
            let tools: Vec<serde_json::Value> = server
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    })
                })
                .collect();
            serde_json::json!({ "tools": tools })
        }
        "tools/call" => {
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Other("tools/call missing tool name".to_string()))?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            let tool = server
                .tools
                .iter()
                .find(|t| t.name == tool_name)
                .ok_or_else(|| {
                    Error::Other(format!(
                        "Tool '{}' not found in server '{}'",
                        tool_name, server_name
                    ))
                })?;

            match (tool.handler)(arguments).await {
                Ok(result) => result,
                Err(e) => {
                    serde_json::json!({
                        "content": [{"type": "text", "text": e.to_string()}],
                        "isError": true,
                    })
                }
            }
        }
        _ => {
            return Err(Error::Other(format!("Unsupported MCP method: {}", method)));
        }
    };

    // Wrap in JSONRPC response envelope
    let jsonrpc_response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": jsonrpc_id,
        "result": jsonrpc_result,
    });

    // Return mcp_response wrapped result
    Ok(serde_json::json!({
        "mcp_response": jsonrpc_response,
    }))
}

fn hook_output_to_json(output: &HookJSONOutput) -> serde_json::Value {
    match output {
        HookJSONOutput::Async { async_timeout } => {
            let mut m = serde_json::Map::new();
            m.insert("async".to_string(), serde_json::json!(true));
            if let Some(t) = async_timeout {
                m.insert("asyncTimeout".to_string(), serde_json::json!(t));
            }
            serde_json::Value::Object(m)
        }
        HookJSONOutput::Sync {
            continue_,
            suppress_output,
            stop_reason,
            decision,
            system_message,
            reason,
            hook_specific_output,
        } => {
            let mut m = serde_json::Map::new();
            if let Some(v) = continue_ {
                m.insert("continue".to_string(), serde_json::json!(v));
            }
            if let Some(v) = suppress_output {
                m.insert("suppressOutput".to_string(), serde_json::json!(v));
            }
            if let Some(v) = stop_reason {
                m.insert("stopReason".to_string(), serde_json::json!(v));
            }
            if let Some(v) = decision {
                m.insert("decision".to_string(), serde_json::json!(v));
            }
            if let Some(v) = system_message {
                m.insert("systemMessage".to_string(), serde_json::json!(v));
            }
            if let Some(v) = reason {
                m.insert("reason".to_string(), serde_json::json!(v));
            }
            if let Some(v) = hook_specific_output {
                m.insert("hookSpecificOutput".to_string(), v.clone());
            }
            serde_json::Value::Object(m)
        }
    }
}
