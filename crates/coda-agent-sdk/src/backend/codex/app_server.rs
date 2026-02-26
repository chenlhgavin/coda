//! Codex app-server multi-turn session using JSON-RPC 2.0.
//!
//! Spawns `codex app-server` as a long-lived subprocess and communicates
//! via stdin/stdout using the JSON-RPC 2.0 protocol.
//!
//! ## Protocol Flow
//!
//! 1. Client sends `initialize` request -> server responds with capabilities
//! 2. Client sends `initialized` notification
//! 3. Client sends `thread/start` request -> server responds with `threadId`
//! 4. Server sends `thread/started` notification
//! 5. Client sends `turn/start` request with user input
//! 6. Server sends `item/*` notifications and `turn/completed` notification
//! 7. For approval: server sends `item/commandExecution/requestApproval` request,
//!    client responds with `{decision: "accept"|"decline"}`

use crate::backend::Session;
use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use async_stream::stream;
use futures::Stream;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::exec_transport::{extract_instructions, find_codex_cli, toml_escape};
use super::jsonrpc;
use super::message_parser;

const MESSAGE_BUFFER_SIZE: usize = 100;
const INITIALIZE_TIMEOUT_SECS: u64 = 60;
const CLOSE_TIMEOUT_SECS: u64 = 5;

/// Internal message type for the app-server protocol.
#[derive(Debug, Clone)]
enum AppServerMessage {
    /// A parsed SDK message from a notification.
    SdkMessage(Message),
    /// A JSON-RPC response (matched by request ID).
    Response(serde_json::Value),
    /// End of stream.
    End,
    /// Error.
    Error(String),
}

/// Multi-turn session for the Codex app-server.
pub struct CodexSession {
    write_tx: Option<mpsc::Sender<String>>,
    message_tx: broadcast::Sender<AppServerMessage>,
    id_gen: jsonrpc::RequestIdGenerator,
    thread_id: Option<String>,
    can_use_tool: Option<crate::options::CanUseToolCallback>,
    write_task: Option<JoinHandle<()>>,
    read_task: Option<JoinHandle<()>>,
    process: Option<Child>,
}

impl std::fmt::Debug for CodexSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexSession")
            .field("thread_id", &self.thread_id)
            .field(
                "can_use_tool",
                &self.can_use_tool.as_ref().map(|_| "<callback>"),
            )
            .finish_non_exhaustive()
    }
}

impl CodexSession {
    /// Create and initialize a new Codex app-server session.
    pub async fn new(options: &AgentOptions, prompt: Option<Prompt>) -> Result<Self> {
        let cli_path = find_codex_cli(options)?;

        let mut cmd_args = vec!["app-server".to_string()];

        if let Some(ref codex_opts) = options.codex
            && let Some(ref policy) = codex_opts.approval_policy
        {
            cmd_args.push("-c".to_string());
            cmd_args.push(format!("approval_policy=\"{}\"", policy));
        }

        // Translate system_prompt → developer_instructions
        if let Some(ref sp) = options.system_prompt
            && let Some(instructions) = extract_instructions(sp)
            && !instructions.is_empty()
        {
            cmd_args.push("-c".to_string());
            cmd_args.push(format!(
                "developer_instructions={}",
                toml_escape(instructions)
            ));
        }

        let mut child_cmd = tokio::process::Command::new(&cli_path);
        child_cmd
            .args(&cmd_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        if let Some(ref cwd) = options.cwd {
            child_cmd.current_dir(cwd);
        }

        for (k, v) in &options.env {
            child_cmd.env(k, v);
        }

        let mut process = child_cmd.spawn().map_err(|e| {
            Error::CliNotFound(format!("Codex CLI not found at: {} - {}", cli_path, e))
        })?;

        let stdin = process
            .stdin
            .take()
            .ok_or_else(|| Error::Other("Failed to capture stdin".to_string()))?;
        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::Other("Failed to capture stdout".to_string()))?;

        let (message_tx, _) = broadcast::channel(MESSAGE_BUFFER_SIZE);
        let (write_tx, mut write_rx) = mpsc::channel::<String>(64);

        // Write task
        let mut stdin_writer = stdin;
        let write_task = tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                if stdin_writer
                    .write_all(format!("{}\n", msg).as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                let _ = stdin_writer.flush().await;
            }
        });

        // Read task
        let msg_tx = message_tx.clone();
        let can_use_tool_for_read = options.can_use_tool.clone();
        let write_tx_for_read = write_tx.clone();

        let read_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let data: serde_json::Value = match serde_json::from_str(line) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                if jsonrpc::is_response(&data) {
                    let _ = msg_tx.send(AppServerMessage::Response(data));
                    continue;
                }

                if jsonrpc::is_request(&data) {
                    // Handle server requests (approval, tool calls)
                    let method = jsonrpc::get_method(&data).unwrap_or("").to_string();
                    let id = data.get("id").cloned().unwrap_or(serde_json::Value::Null);
                    let params = data
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);

                    let response = handle_server_request(
                        &method,
                        &id,
                        &params,
                        can_use_tool_for_read.as_ref(),
                    )
                    .await;

                    if let Ok(resp_str) = serde_json::to_string(&response) {
                        let _ = write_tx_for_read.send(resp_str).await;
                    }
                    continue;
                }

                if jsonrpc::is_notification(&data) {
                    let method = jsonrpc::get_method(&data).unwrap_or("");
                    let params = data
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);

                    match message_parser::parse_app_server_notification(method, &params) {
                        Ok(Some(msg)) => {
                            let _ = msg_tx.send(AppServerMessage::SdkMessage(msg));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let _ =
                                msg_tx.send(AppServerMessage::Error(format!("Parse error: {}", e)));
                        }
                    }
                    continue;
                }
            }

            let _ = msg_tx.send(AppServerMessage::End);
        });

        let id_gen = jsonrpc::RequestIdGenerator::new();

        // Initialize the app-server
        let init_id = id_gen.next_id();
        let init_request = jsonrpc::build_request(
            init_id,
            "initialize",
            serde_json::json!({
                "clientName": "code-agent-sdk",
                "clientVersion": env!("CARGO_PKG_VERSION"),
            }),
        );

        // Subscribe before sending to avoid missing fast responses.
        let mut rx = message_tx.subscribe();
        write_tx
            .send(serde_json::to_string(&init_request)?)
            .await
            .map_err(|_| Error::Other("Write channel closed".to_string()))?;

        // Wait for initialize response
        let init_result = tokio::time::timeout(
            std::time::Duration::from_secs(INITIALIZE_TIMEOUT_SECS),
            async {
                loop {
                    match rx.recv().await {
                        Ok(AppServerMessage::Response(resp)) => {
                            if jsonrpc::get_id(&resp) == Some(init_id) {
                                return Ok(resp);
                            }
                        }
                        Ok(AppServerMessage::End) | Ok(AppServerMessage::Error(_)) => {
                            return Err(Error::Other(
                                "App-server stream ended before initialize response".to_string(),
                            ));
                        }
                        Err(_) => {
                            return Err(Error::Other("Channel closed".to_string()));
                        }
                        _ => continue,
                    }
                }
            },
        )
        .await
        .map_err(|_| Error::Other("Initialize timeout".to_string()))??;

        let _ = init_result; // Could extract server capabilities here

        // Send initialized notification
        let initialized_notif = jsonrpc::build_notification("initialized", serde_json::json!({}));
        write_tx
            .send(serde_json::to_string(&initialized_notif)?)
            .await
            .map_err(|_| Error::Other("Write channel closed".to_string()))?;

        let mut session = Self {
            write_tx: Some(write_tx),
            message_tx,
            id_gen,
            thread_id: None,
            can_use_tool: options.can_use_tool.clone(),
            write_task: Some(write_task),
            read_task: Some(read_task),
            process: Some(process),
        };

        // Start a thread
        let thread_start_id = session.id_gen.next_id();
        let thread_start_req =
            jsonrpc::build_request(thread_start_id, "thread/start", serde_json::json!({}));

        // Subscribe before sending to avoid missing fast responses.
        let mut rx = session.message_tx.subscribe();
        session
            .send_raw(&serde_json::to_string(&thread_start_req)?)
            .await?;

        // Wait for thread/start response to get threadId
        let thread_resp = tokio::time::timeout(
            std::time::Duration::from_secs(INITIALIZE_TIMEOUT_SECS),
            async {
                loop {
                    match rx.recv().await {
                        Ok(AppServerMessage::Response(resp)) => {
                            if jsonrpc::get_id(&resp) == Some(thread_start_id) {
                                return Ok(resp);
                            }
                        }
                        Ok(AppServerMessage::End) | Ok(AppServerMessage::Error(_)) => {
                            return Err(Error::Other(
                                "App-server stream ended before thread/start response".to_string(),
                            ));
                        }
                        Err(_) => {
                            return Err(Error::Other("Channel closed".to_string()));
                        }
                        _ => continue,
                    }
                }
            },
        )
        .await
        .map_err(|_| Error::Other("thread/start timeout".to_string()))??;

        let thread_id = thread_resp
            .get("result")
            .and_then(|r| r.get("threadId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        session.thread_id = Some(thread_id.clone());

        // Keep connect semantics consistent across backends:
        // Prompt::Text is accepted but not auto-sent.
        if let Some(prompt) = prompt {
            match prompt {
                Prompt::Text(_) => {}
                Prompt::Stream(_) => {
                    return Err(Error::Other(
                        "Codex session does not support stream prompts for initial message"
                            .to_string(),
                    ));
                }
            }
        }

        Ok(session)
    }

    async fn send_raw(&self, data: &str) -> Result<()> {
        self.write_tx
            .as_ref()
            .ok_or_else(|| Error::Other("Session closed".to_string()))?
            .send(data.to_string())
            .await
            .map_err(|_| Error::Other("Write channel closed".to_string()))
    }

    async fn start_turn(&self, prompt: &str) -> Result<()> {
        let thread_id = self
            .thread_id
            .as_ref()
            .ok_or_else(|| Error::Other("No active thread".to_string()))?;

        let turn_id = self.id_gen.next_id();
        let turn_request = jsonrpc::build_request(
            turn_id,
            "turn/start",
            serde_json::json!({
                "threadId": thread_id,
                "input": [{
                    "role": "user",
                    "content": prompt,
                }],
            }),
        );

        self.send_raw(&serde_json::to_string(&turn_request)?).await
    }
}

async fn handle_server_request(
    method: &str,
    id: &serde_json::Value,
    params: &serde_json::Value,
    can_use_tool: Option<&crate::options::CanUseToolCallback>,
) -> serde_json::Value {
    match method {
        "item/commandExecution/requestApproval" => {
            if let Some(cb) = can_use_tool {
                let command = params
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = serde_json::json!({"command": command});
                let ctx = crate::options::ToolPermissionContext {
                    signal: None,
                    suggestions: vec![],
                };
                let result = cb("Bash".to_string(), input, ctx).await;
                let decision = match result {
                    crate::options::PermissionResult::Allow(_) => "accept",
                    crate::options::PermissionResult::Deny(_) => "decline",
                };
                jsonrpc::build_response(id.clone(), serde_json::json!({"decision": decision}))
            } else {
                // Auto-accept if no callback
                jsonrpc::build_response(id.clone(), serde_json::json!({"decision": "accept"}))
            }
        }
        "item/fileChange/requestApproval" => {
            if let Some(cb) = can_use_tool {
                let file_path = params
                    .get("filePath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = serde_json::json!({"file_path": file_path});
                let ctx = crate::options::ToolPermissionContext {
                    signal: None,
                    suggestions: vec![],
                };
                let result = cb("Edit".to_string(), input, ctx).await;
                let decision = match result {
                    crate::options::PermissionResult::Allow(_) => "accept",
                    crate::options::PermissionResult::Deny(_) => "decline",
                };
                jsonrpc::build_response(id.clone(), serde_json::json!({"decision": decision}))
            } else {
                jsonrpc::build_response(id.clone(), serde_json::json!({"decision": "accept"}))
            }
        }
        "item/tool/call" => {
            // Dynamic tool calls - not supported yet, decline
            jsonrpc::build_error_response(
                id.clone(),
                -32601,
                "Dynamic tool calls not supported by SDK",
            )
        }
        _ => jsonrpc::build_error_response(
            id.clone(),
            -32601,
            &format!("Unknown server request: {}", method),
        ),
    }
}

#[async_trait::async_trait]
impl Session for CodexSession {
    async fn send_message(&mut self, prompt: Prompt, _session_id: &str) -> Result<()> {
        let prompt_text = match prompt {
            Prompt::Text(s) => s,
            Prompt::Stream(_) => {
                return Err(Error::Other(
                    "Codex session does not support stream prompts. Use Prompt::Text.".to_string(),
                ));
            }
        };
        self.start_turn(&prompt_text).await
    }

    fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let mut rx = self.message_tx.subscribe();

        let stream = stream! {
            loop {
                match rx.recv().await {
                    Ok(AppServerMessage::SdkMessage(msg)) => yield Ok(msg),
                    Ok(AppServerMessage::End) => break,
                    Ok(AppServerMessage::Error(e)) => {
                        yield Err(Error::Other(e));
                        break;
                    }
                    Ok(AppServerMessage::Response(_)) => {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        };

        Box::pin(stream)
    }

    fn receive_response(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let mut rx = self.message_tx.subscribe();

        let stream = stream! {
            loop {
                match rx.recv().await {
                    Ok(AppServerMessage::SdkMessage(msg)) => {
                        let is_result = matches!(&msg, Message::Result(_));
                        yield Ok(msg);
                        if is_result {
                            break;
                        }
                    }
                    Ok(AppServerMessage::End) => break,
                    Ok(AppServerMessage::Error(e)) => {
                        yield Err(Error::Other(e));
                        break;
                    }
                    Ok(AppServerMessage::Response(_)) => {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        };

        Box::pin(stream)
    }

    async fn send_control_request(
        &mut self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let subtype = request
            .get("subtype")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match subtype {
            "interrupt" => {
                let thread_id = self
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| Error::Other("No active thread".to_string()))?;
                let id = self.id_gen.next_id();
                let req = jsonrpc::build_request(
                    id,
                    "turn/interrupt",
                    serde_json::json!({"threadId": thread_id}),
                );
                self.send_raw(&serde_json::to_string(&req)?).await?;
                Ok(serde_json::Value::Null)
            }
            _ => Err(Error::UnsupportedFeature {
                feature: format!("control request '{}'", subtype),
                backend: "Codex".to_string(),
            }),
        }
    }

    async fn get_server_info(&self) -> Option<serde_json::Value> {
        self.thread_id
            .as_ref()
            .map(|id| serde_json::json!({"threadId": id}))
    }

    async fn close(&mut self) -> Result<()> {
        drop(self.write_tx.take());

        if let Some(mut process) = self.process.take() {
            let wait_result = tokio::time::timeout(
                std::time::Duration::from_secs(CLOSE_TIMEOUT_SECS),
                process.wait(),
            )
            .await;

            match wait_result {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    return Err(Error::Other(format!(
                        "Failed waiting for Codex app-server shutdown: {}",
                        e
                    )));
                }
                Err(_) => {
                    let _ = process.kill().await;
                    let _ = process.wait().await;
                }
            }
        }

        if let Some(mut handle) = self.read_task.take()
            && tokio::time::timeout(
                std::time::Duration::from_secs(CLOSE_TIMEOUT_SECS),
                &mut handle,
            )
            .await
            .is_err()
        {
            handle.abort();
            let _ = handle.await;
        }
        if let Some(mut handle) = self.write_task.take()
            && tokio::time::timeout(
                std::time::Duration::from_secs(CLOSE_TIMEOUT_SECS),
                &mut handle,
            )
            .await
            .is_err()
        {
            handle.abort();
            let _ = handle.await;
        }

        let _ = self.message_tx.send(AppServerMessage::End);
        Ok(())
    }
}
