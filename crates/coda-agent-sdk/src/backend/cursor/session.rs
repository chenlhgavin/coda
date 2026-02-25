//! Cursor Agent spawn-per-turn session management.
//!
//! Cursor Agent CLI does not support a long-lived server mode. Multi-turn
//! sessions are achieved by spawning a new process for each turn using
//! `agent --print --resume <chatId>`.

use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use async_stream::stream;
use futures::Stream;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

use super::message_parser;
use super::transport::find_cursor_cli;

const MESSAGE_BUFFER_SIZE: usize = 100;
const CHAT_ID_WAIT_SECS: u64 = 5;
const CLOSE_TIMEOUT_SECS: u64 = 5;

/// Internal control message for the cursor session.
#[derive(Debug, Clone)]
enum SessionMessage {
    SdkMessage(Message),
    End,
    Error(String),
}

/// Multi-turn session for Cursor Agent using spawn-per-turn.
///
/// Each call to [`send_message`](CursorSession::send_message) spawns a new
/// `agent --print --resume <chatId>` process. The `chatId` is extracted
/// from the first turn's `system/init` event.
pub struct CursorSession {
    cli_path: String,
    options: AgentOptions,
    chat_id: Option<String>,
    message_tx: broadcast::Sender<SessionMessage>,
    active_process: Option<Child>,
    read_task: Option<JoinHandle<()>>,
    has_started_turn: bool,
}

impl std::fmt::Debug for CursorSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CursorSession")
            .field("cli_path", &self.cli_path)
            .field("chat_id", &self.chat_id)
            .field("has_started_turn", &self.has_started_turn)
            .field("turn_running", &self.active_process.is_some())
            .finish_non_exhaustive()
    }
}

impl CursorSession {
    /// Create a new Cursor session, optionally running an initial prompt.
    pub async fn new(options: &AgentOptions, prompt: Option<Prompt>) -> Result<Self> {
        let cli_path = find_cursor_cli(options)?;
        let (message_tx, _) = broadcast::channel(MESSAGE_BUFFER_SIZE);

        let session = Self {
            cli_path,
            options: options.clone(),
            chat_id: None,
            message_tx,
            active_process: None,
            read_task: None,
            has_started_turn: false,
        };

        // Keep connect semantics consistent across backends:
        // Prompt::Text is accepted but not auto-sent.
        if let Some(prompt) = prompt {
            match prompt {
                Prompt::Text(_) => {}
                Prompt::Stream(_) => {
                    return Err(Error::Other(
                        "Cursor session does not support stream prompts".to_string(),
                    ));
                }
            }
        }

        Ok(session)
    }

    async fn sync_completed_turn_state(&mut self) -> Result<()> {
        let Some(process) = self.active_process.as_mut() else {
            return Ok(());
        };

        match process.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    let exit_code = status.code().unwrap_or(-1);
                    let _ = self.message_tx.send(SessionMessage::Error(format!(
                        "Process exited with code {}",
                        exit_code
                    )));
                    self.active_process = None;
                    if let Some(handle) = self.read_task.take() {
                        let _ = handle.await;
                    }
                    return Err(Error::Process {
                        exit_code,
                        stderr: None,
                    });
                }
                self.active_process = None;
                if let Some(handle) = self.read_task.take() {
                    let _ = handle.await;
                }
            }
            Ok(None) => {}
            Err(e) => {
                return Err(Error::Other(format!(
                    "Failed to check Cursor process status: {}",
                    e
                )));
            }
        }

        Ok(())
    }

    /// Spawn a new agent process for one turn.
    async fn run_turn(&mut self, prompt: &str) -> Result<()> {
        self.sync_completed_turn_state().await?;
        if self.active_process.is_some() {
            return Err(Error::Other(
                "Previous Cursor turn is still running. Wait for receive_response() to complete."
                    .to_string(),
            ));
        }
        if self.has_started_turn && self.chat_id.is_none() {
            return Err(Error::Other(
                "Cursor session id not available yet. Wait for the previous turn to emit init/result before sending the next message."
                    .to_string(),
            ));
        }

        let mut cmd_args = vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
        ];

        if let Some(ref chat_id) = self.chat_id {
            cmd_args.push("--resume".to_string());
            cmd_args.push(chat_id.clone());
        }

        if let Some(ref m) = self.options.model {
            cmd_args.push("--model".to_string());
            cmd_args.push(m.clone());
        }

        if let Some(ref cursor_opts) = self.options.cursor {
            if cursor_opts.force_approve {
                cmd_args.push("--force".to_string());
            }
            if let Some(ref mode) = cursor_opts.mode {
                cmd_args.push("--mode".to_string());
                cmd_args.push(mode.clone());
            }
            if cursor_opts.trust_workspace {
                cmd_args.push("--trust".to_string());
            }
        }

        for (key, value) in &self.options.extra_args {
            if let Some(v) = value {
                cmd_args.push(format!("--{}", key));
                cmd_args.push(v.clone());
            } else {
                cmd_args.push(format!("--{}", key));
            }
        }

        // Prompt goes last
        cmd_args.push(prompt.to_string());

        let mut child_cmd = tokio::process::Command::new(&self.cli_path);
        child_cmd
            .args(&cmd_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        if let Some(ref cwd) = self.options.cwd {
            child_cmd.current_dir(cwd);
        }

        for (k, v) in &self.options.env {
            child_cmd.env(k, v);
        }

        let mut process = child_cmd.spawn().map_err(|e| {
            Error::CliNotFound(format!(
                "Cursor Agent CLI not found at: {} - {}",
                self.cli_path, e
            ))
        })?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| Error::Other("Failed to capture stdout".to_string()))?;

        let msg_tx = self.message_tx.clone();
        let (chat_id_tx, chat_id_rx) = oneshot::channel::<Option<String>>();

        // Spawn a reader task for this turn.
        let read_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut chat_id_tx = Some(chat_id_tx);
            let mut emitted_chat_id: Option<String> = None;

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let data: serde_json::Value = match serde_json::from_str(line) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                let extracted_chat_id =
                    if data.get("type").and_then(|v| v.as_str()) == Some("system") {
                        data.get("chatId")
                            .or_else(|| data.get("session_id"))
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string)
                    } else if data.get("type").and_then(|v| v.as_str()) == Some("result") {
                        data.get("session_id")
                            .or_else(|| data.get("chatId"))
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string)
                    } else {
                        None
                    };

                if let Some(chat_id) = extracted_chat_id {
                    emitted_chat_id = Some(chat_id.clone());
                    if let Some(tx) = chat_id_tx.take() {
                        let _ = tx.send(Some(chat_id));
                    }
                }

                match message_parser::parse_cursor_event(&data) {
                    Ok(Some(msg)) => {
                        let _ = msg_tx.send(SessionMessage::SdkMessage(msg));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = msg_tx.send(SessionMessage::Error(format!("Parse error: {}", e)));
                    }
                }
            }
            if let Some(tx) = chat_id_tx.take() {
                let _ = tx.send(emitted_chat_id);
            }
        });

        self.active_process = Some(process);
        self.read_task = Some(read_task);
        self.has_started_turn = true;

        if self.chat_id.is_none()
            && let Ok(Ok(Some(id))) = tokio::time::timeout(
                std::time::Duration::from_secs(CHAT_ID_WAIT_SECS),
                chat_id_rx,
            )
            .await
        {
            self.chat_id = Some(id);
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::backend::Session for CursorSession {
    async fn send_message(&mut self, prompt: Prompt, _session_id: &str) -> Result<()> {
        let prompt_text = match prompt {
            Prompt::Text(s) => s,
            Prompt::Stream(_) => {
                return Err(Error::Other(
                    "Cursor session does not support stream prompts. Use Prompt::Text.".to_string(),
                ));
            }
        };
        self.run_turn(&prompt_text).await
    }

    fn receive_messages(&self) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send + '_>> {
        let mut rx = self.message_tx.subscribe();

        let stream = stream! {
            loop {
                match rx.recv().await {
                    Ok(SessionMessage::SdkMessage(msg)) => yield Ok(msg),
                    Ok(SessionMessage::End) => break,
                    Ok(SessionMessage::Error(e)) => {
                        yield Err(Error::Other(e));
                        break;
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
                    Ok(SessionMessage::SdkMessage(msg)) => {
                        let is_result = matches!(&msg, Message::Result(_));
                        yield Ok(msg);
                        if is_result {
                            break;
                        }
                    }
                    Ok(SessionMessage::End) => break,
                    Ok(SessionMessage::Error(e)) => {
                        yield Err(Error::Other(e));
                        break;
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
        Err(Error::UnsupportedFeature {
            feature: format!("control request '{}'", subtype),
            backend: "Cursor".to_string(),
        })
    }

    async fn get_server_info(&self) -> Option<serde_json::Value> {
        self.chat_id
            .as_ref()
            .map(|id| serde_json::json!({"chatId": id}))
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(mut process) = self.active_process.take() {
            let wait_result = tokio::time::timeout(
                std::time::Duration::from_secs(CLOSE_TIMEOUT_SECS),
                process.wait(),
            )
            .await;

            match wait_result {
                Ok(Ok(status)) => {
                    if !status.success() {
                        let _ = self.message_tx.send(SessionMessage::Error(format!(
                            "Process exited with code {}",
                            status.code().unwrap_or(-1)
                        )));
                    }
                }
                Ok(Err(e)) => {
                    return Err(Error::Other(format!(
                        "Failed waiting for Cursor process shutdown: {}",
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

        let _ = self.message_tx.send(SessionMessage::End);
        Ok(())
    }
}
