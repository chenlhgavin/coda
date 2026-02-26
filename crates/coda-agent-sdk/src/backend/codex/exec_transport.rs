//! One-shot transport for `codex exec --json`.
//!
//! Spawns `codex exec --json <prompt>`, reads JSONL events from stdout,
//! and maps them to SDK [`Message`] types.

use crate::error::{Error, Result};
use crate::options::{AgentOptions, SystemPromptConfig};
use crate::types::{Message, Prompt};
use async_stream::stream;
use futures::Stream;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::message_parser;

/// Escape a string for use as a TOML basic string value (wrapped in double quotes).
pub(super) fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Extract instruction text from a [`SystemPromptConfig`].
///
/// For [`SystemPromptConfig::Preset`] (Claude-specific), returns the `append`
/// text since the preset name is irrelevant to the Codex backend.
pub(super) fn extract_instructions(sp: &SystemPromptConfig) -> Option<&str> {
    match sp {
        SystemPromptConfig::String(s) => Some(s.as_str()),
        SystemPromptConfig::Preset { append, .. } => append.as_deref(),
    }
}

/// Find the Codex CLI binary.
pub fn find_codex_cli(options: &AgentOptions) -> Result<String> {
    if let Some(ref p) = options.cli_path {
        return Ok(p.to_string_lossy().to_string());
    }

    if let Ok(path) = std::env::var("CODEX_CLI_PATH") {
        return Ok(path);
    }

    // Search PATH
    if let Some(paths) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&paths) {
            let full = path.join("codex");
            if full.is_file() {
                return Some(full.to_string_lossy().to_string())
                    .ok_or_else(|| Error::CliNotFound("codex not found".to_string()));
            }
        }
    }

    Err(Error::CliNotFound(
        "Codex CLI not found. Install with:\n  npm install -g @openai/codex\n\n\
         Or set CODEX_CLI_PATH environment variable"
            .to_string(),
    ))
}

/// Build command-line arguments for `codex exec`.
fn build_exec_command(cli_path: &str, prompt: &str, options: &AgentOptions) -> Vec<String> {
    let mut cmd = vec![
        cli_path.to_string(),
        "exec".to_string(),
        "--json".to_string(),
    ];

    if let Some(ref m) = options.model {
        cmd.push("--model".to_string());
        cmd.push(m.clone());
    }

    if let Some(ref codex_opts) = options.codex {
        if let Some(ref policy) = codex_opts.approval_policy {
            cmd.push("--config".to_string());
            cmd.push(format!("approval_policy=\"{}\"", policy));
        }
        if let Some(ref sandbox) = codex_opts.sandbox_mode {
            let permissions = match sandbox.as_str() {
                "read-only" => vec![],
                "workspace-write" => vec!["disk-full-read-access", "disk-write-cwd"],
                "danger-full-access" => {
                    vec![
                        "disk-full-read-access",
                        "disk-full-write-access",
                        "network-full-access",
                    ]
                }
                _ => vec![],
            };
            if !permissions.is_empty() {
                let perm_str = permissions
                    .iter()
                    .map(|p| format!("\"{}\"", p))
                    .collect::<Vec<_>>()
                    .join(", ");
                cmd.push("--config".to_string());
                cmd.push(format!("sandbox_permissions=[{}]", perm_str));
            }
        }
    }

    // Translate system_prompt → developer_instructions
    if let Some(ref sp) = options.system_prompt
        && let Some(instructions) = extract_instructions(sp)
        && !instructions.is_empty()
    {
        cmd.push("--config".to_string());
        cmd.push(format!(
            "developer_instructions={}",
            toml_escape(instructions)
        ));
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

/// Execute a one-shot Codex query, returning a stream of messages.
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
                    "Codex one-shot query does not support stream prompts. \
                     Use create_session() for multi-turn interaction."
                        .to_string(),
                ));
                return;
            }
        };

        let cli_path = match find_codex_cli(&options) {
            Ok(p) => p,
            Err(e) => {
                yield Err(e);
                return;
            }
        };

        let cmd = build_exec_command(&cli_path, &prompt_text, &options);

        let mut child_cmd = Command::new(&cmd[0]);
        child_cmd
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

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
                    "Codex CLI not found at: {} - {}",
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

        // Read stderr in background for error reporting
        let stderr = process.stderr.take();
        let stderr_handle = stderr.map(|s| {
            tokio::spawn(async move {
                let reader = BufReader::new(s);
                let mut lines = reader.lines();
                let mut output = String::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&line);
                }
                output
            })
        });

        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(data) => {
                    match message_parser::parse_exec_event(&data) {
                        Ok(Some(msg)) => yield Ok(msg),
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
        match process.wait().await {
            Ok(status) => {
                if !status.success() {
                    let stderr_output = if let Some(handle) = stderr_handle {
                        handle.await.ok()
                    } else {
                        None
                    };
                    yield Err(Error::Process {
                        exit_code: status.code().unwrap_or(-1),
                        stderr: stderr_output,
                    });
                    return;
                }
            }
            Err(e) => {
                yield Err(Error::Other(format!("Failed to wait for process: {}", e)));
                return;
            }
        }

        // Emit a synthetic result message for completion
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
    };

    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_build_exec_command_basic() {
        let options = AgentOptions::default();
        let cmd = build_exec_command("/usr/bin/codex", "hello world", &options);
        assert_eq!(cmd[0], "/usr/bin/codex");
        assert_eq!(cmd[1], "exec");
        assert_eq!(cmd[2], "--json");
        assert_eq!(cmd.last().unwrap(), "hello world");
    }

    #[test]
    fn test_should_build_exec_command_with_model() {
        let options = AgentOptions {
            model: Some("o4-mini".to_string()),
            ..Default::default()
        };
        let cmd = build_exec_command("/usr/bin/codex", "test", &options);
        assert!(cmd.contains(&"--model".to_string()));
        assert!(cmd.contains(&"o4-mini".to_string()));
    }

    #[test]
    fn test_should_build_exec_command_with_codex_options() {
        let options = AgentOptions {
            codex: Some(crate::options::CodexOptions {
                approval_policy: Some("never".to_string()),
                sandbox_mode: Some("danger-full-access".to_string()),
            }),
            ..Default::default()
        };
        let cmd = build_exec_command("/usr/bin/codex", "test", &options);
        assert!(cmd.contains(&"--config".to_string()));
        assert!(cmd.iter().any(|s| s.contains("approval_policy")));
        assert!(cmd.iter().any(|s| s.contains("sandbox_permissions")));
    }

    #[test]
    fn test_should_build_exec_command_with_system_prompt_string() {
        let options = AgentOptions {
            system_prompt: Some(SystemPromptConfig::String(
                "You are a helpful assistant.".to_string(),
            )),
            ..Default::default()
        };
        let cmd = build_exec_command("/usr/bin/codex", "test", &options);
        assert!(cmd.iter().any(|s| s.contains("developer_instructions=")));
        assert!(
            cmd.iter()
                .any(|s| s.contains("You are a helpful assistant."))
        );
    }

    #[test]
    fn test_should_build_exec_command_with_system_prompt_preset() {
        let options = AgentOptions {
            system_prompt: Some(SystemPromptConfig::Preset {
                preset: "claude_code".to_string(),
                append: Some("Extra instructions here.".to_string()),
            }),
            ..Default::default()
        };
        let cmd = build_exec_command("/usr/bin/codex", "test", &options);
        assert!(cmd.iter().any(|s| s.contains("developer_instructions=")));
        assert!(cmd.iter().any(|s| s.contains("Extra instructions here.")));
    }

    #[test]
    fn test_should_skip_developer_instructions_when_empty() {
        let options = AgentOptions {
            system_prompt: Some(SystemPromptConfig::String(String::new())),
            ..Default::default()
        };
        let cmd = build_exec_command("/usr/bin/codex", "test", &options);
        assert!(!cmd.iter().any(|s| s.contains("developer_instructions")));
    }

    #[test]
    fn test_should_skip_developer_instructions_when_preset_has_no_append() {
        let options = AgentOptions {
            system_prompt: Some(SystemPromptConfig::Preset {
                preset: "claude_code".to_string(),
                append: None,
            }),
            ..Default::default()
        };
        let cmd = build_exec_command("/usr/bin/codex", "test", &options);
        assert!(!cmd.iter().any(|s| s.contains("developer_instructions")));
    }

    #[test]
    fn test_should_escape_toml_special_characters() {
        assert_eq!(toml_escape("hello"), "\"hello\"");
        assert_eq!(toml_escape("line1\nline2"), "\"line1\\nline2\"");
        assert_eq!(toml_escape("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(toml_escape("path\\to\\file"), "\"path\\\\to\\\\file\"");
        assert_eq!(toml_escape("tab\there"), "\"tab\\there\"");
        assert_eq!(toml_escape("cr\rhere"), "\"cr\\rhere\"");
    }
}
