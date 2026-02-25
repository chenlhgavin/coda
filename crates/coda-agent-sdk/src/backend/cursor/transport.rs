//! One-shot transport for the Cursor Agent CLI.
//!
//! Spawns `agent --print --output-format stream-json <prompt>` and reads
//! JSONL events from stdout.

use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::types::{Message, Prompt};
use async_stream::stream;
use futures::Stream;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::message_parser;

/// Find the Cursor Agent CLI binary.
pub fn find_cursor_cli(options: &AgentOptions) -> Result<String> {
    if let Some(ref p) = options.cli_path {
        return Ok(p.to_string_lossy().to_string());
    }

    if let Ok(path) = std::env::var("CURSOR_CLI_PATH") {
        return Ok(path);
    }

    // Search PATH
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let full = path.join("agent");
            if full.is_file() {
                return Some(full.to_string_lossy().to_string())
                    .ok_or_else(|| Error::CliNotFound("agent not found".to_string()));
            }
        }
    }

    Err(Error::CliNotFound(
        "Cursor Agent CLI not found. Install from cursor.com.\n\n\
         Or set CURSOR_CLI_PATH environment variable"
            .to_string(),
    ))
}

/// Build command-line arguments for a one-shot Cursor Agent query.
fn build_cursor_command(cli_path: &str, prompt: &str, options: &AgentOptions) -> Vec<String> {
    let mut cmd = vec![
        cli_path.to_string(),
        "--print".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];

    if let Some(ref m) = options.model {
        cmd.push("--model".to_string());
        cmd.push(m.clone());
    }

    if let Some(ref cursor_opts) = options.cursor {
        if cursor_opts.force_approve {
            cmd.push("--force".to_string());
        }
        if let Some(ref mode) = cursor_opts.mode {
            cmd.push("--mode".to_string());
            cmd.push(mode.clone());
        }
        if cursor_opts.trust_workspace {
            cmd.push("--trust".to_string());
        }
    }

    for (key, value) in &options.extra_args {
        if let Some(v) = value {
            cmd.push(format!("--{}", key));
            cmd.push(v.clone());
        } else {
            cmd.push(format!("--{}", key));
        }
    }

    cmd.push(prompt.to_string());

    cmd
}

/// Execute a one-shot Cursor Agent query, returning a stream of messages.
pub fn one_shot_query(
    prompt: Prompt,
    options: &AgentOptions,
) -> Pin<Box<dyn Stream<Item = Result<Message>> + Send>> {
    let options = options.clone();

    let stream = stream! {
        let prompt_text = match prompt {
            Prompt::Text(s) => s,
            Prompt::Stream(_) => {
                yield Err(Error::Other(
                    "Cursor Agent one-shot query does not support stream prompts. \
                     Use create_session() for multi-turn interaction."
                        .to_string(),
                ));
                return;
            }
        };

        let cli_path = match find_cursor_cli(&options) {
            Ok(p) => p,
            Err(e) => {
                yield Err(e);
                return;
            }
        };

        let cmd = build_cursor_command(&cli_path, &prompt_text, &options);

        let mut child_cmd = Command::new(&cmd[0]);
        child_cmd
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        if let Some(ref cwd) = options.cwd {
            child_cmd.current_dir(cwd);
        }

        for (k, v) in &options.env {
            child_cmd.env(k, v);
        }

        let mut process = match child_cmd.spawn() {
            Ok(p) => p,
            Err(e) => {
                yield Err(Error::CliNotFound(format!(
                    "Cursor Agent CLI not found at: {} - {}",
                    cli_path, e
                )));
                return;
            }
        };

        let stdout = match process.stdout.take() {
            Some(s) => s,
            None => {
                yield Err(Error::Other("Failed to capture stdout".to_string()));
                return;
            }
        };

        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut got_result = false;

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(data) => {
                    match message_parser::parse_cursor_event(&data) {
                        Ok(Some(msg)) => {
                            if matches!(&msg, Message::Result(_)) {
                                got_result = true;
                            }
                            yield Ok(msg);
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            yield Err(e);
                            continue;
                        }
                    }
                }
                Err(_) => continue,
            }
        }

        // Wait for process to complete
        let _ = process.wait().await;

        // Emit synthetic result if the CLI didn't produce one
        if !got_result {
            yield Ok(Message::Result(crate::types::ResultMessage {
                subtype: "success".to_string(),
                duration_ms: 0,
                duration_api_ms: 0,
                is_error: false,
                num_turns: 1,
                session_id: String::new(),
                total_cost_usd: None,
                usage: None,
                result: None,
                structured_output: None,
            }));
        }
    };

    Box::pin(stream)
}
