//! Subprocess transport using Claude Code CLI.

use crate::backend::claude::{cli_finder, command_builder};
use crate::error::{Error, Result};
use crate::options::AgentOptions;
use crate::transport::Transport;
use async_stream::stream;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const DEFAULT_MAX_BUFFER_SIZE: usize = 1024 * 1024; // 1MB

pub struct SubprocessCliTransport {
    options: AgentOptions,
    cli_path: String,
    cwd: Option<String>,
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    ready: bool,
    exit_error: Option<Error>,
    max_buffer_size: usize,
}

impl SubprocessCliTransport {
    pub fn new(_prompt: &str, options: AgentOptions) -> Result<Self> {
        let cli_path = cli_finder::find_cli(&options)?;
        let cwd = options
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().to_string());

        let max_buffer_size = options.max_buffer_size.unwrap_or(DEFAULT_MAX_BUFFER_SIZE);

        Ok(Self {
            options,
            cli_path,
            cwd,
            process: None,
            stdin: None,
            stdout: None,
            ready: false,
            exit_error: None,
            max_buffer_size,
        })
    }

    fn build_command(&self) -> Vec<String> {
        command_builder::build_command(&self.cli_path, &self.options)
    }
}

#[async_trait::async_trait]
impl Transport for SubprocessCliTransport {
    async fn connect(&mut self) -> Result<()> {
        if self.process.is_some() {
            return Ok(());
        }

        if std::env::var("CLAUDE_AGENT_SDK_SKIP_VERSION_CHECK").is_err() {
            cli_finder::check_claude_version(&self.cli_path).await;
        }

        let cmd = self.build_command();
        let cmd0 = cmd[0].clone();
        let cmd_rest: Vec<String> = cmd[1..].to_vec();

        let should_pipe_stderr = self.options.stderr.is_some()
            || self.options.extra_args.contains_key("debug-to-stderr");
        let stderr_dest = if should_pipe_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        };

        let mut child_cmd = Command::new(cmd0);
        child_cmd
            .args(&cmd_rest)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_dest)
            .env("CLAUDE_CODE_ENTRYPOINT", "sdk-rs")
            .env("CLAUDE_AGENT_SDK_VERSION", env!("CARGO_PKG_VERSION"));

        for (k, v) in &self.options.env {
            child_cmd.env(k, v);
        }
        if self.options.enable_file_checkpointing {
            child_cmd.env("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "true");
        }
        if let Some(ref cwd) = self.cwd {
            child_cmd.env("PWD", cwd);
            child_cmd.current_dir(cwd);
        }

        // Resolve user option to OS uid (matching Python SDK's `user` param to open_process)
        #[cfg(unix)]
        if let Some(ref username) = self.options.user {
            let user = nix::unistd::User::from_name(username).map_err(|e| {
                Error::Other(format!("Failed to resolve user '{}': {}", username, e))
            })?;
            if let Some(user) = user {
                child_cmd.uid(user.uid.as_raw());
            } else {
                return Err(Error::Other(format!("User '{}' not found", username)));
            }
        }

        let mut process = child_cmd.spawn().map_err(|e| {
            if let Some(ref cwd) = self.cwd
                && !Path::new(cwd).exists()
            {
                return Error::Other(format!("Working directory does not exist: {}", cwd));
            }
            Error::CliNotFound(format!(
                "Claude Code not found at: {} - {}",
                self.cli_path, e
            ))
        })?;

        self.stdin = process.stdin.take();
        self.stdout = process.stdout.take();

        if should_pipe_stderr && let Some(stderr) = process.stderr.take() {
            let stderr_callback = self.options.stderr.clone();
            let stderr_reader = BufReader::new(stderr);
            // Spawn task to read stderr and invoke callback
            tokio::spawn(async move {
                let mut lines = stderr_reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line_str = line.trim_end();
                    if !line_str.is_empty()
                        && let Some(ref cb) = stderr_callback
                    {
                        cb(line_str);
                    }
                }
            });
        }

        self.process = Some(process);
        self.ready = true;

        Ok(())
    }

    async fn write(&mut self, data: &str) -> Result<()> {
        if !self.ready {
            return Err(Error::Other("Transport not ready for writing".to_string()));
        }
        if let Some(ref mut stdin) = self.stdin {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(data.as_bytes()).await?;
            stdin.flush().await?;
            Ok(())
        } else {
            Err(Error::Other("Stdin not available".to_string()))
        }
    }

    fn read_messages(
        &mut self,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = Result<serde_json::Value>> + Send>> {
        let stdout = self.stdout.take();
        let max_buffer_size = self.max_buffer_size;

        let stream = stream! {
            let stdout = match stdout {
                Some(s) => s,
                None => {
                    yield Err(Error::Other("Not connected".to_string()));
                    return;
                }
            };

            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut json_buffer = String::new();

            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        for json_line in line.split('\n') {
                            let json_line = json_line.trim();
                            if json_line.is_empty() {
                                continue;
                            }
                            json_buffer.push_str(json_line);

                            if json_buffer.len() > max_buffer_size {
                                json_buffer.clear();
                                yield Err(Error::Other(format!(
                                    "JSON message exceeded maximum buffer size of {} bytes",
                                    max_buffer_size
                                )));
                                return;
                            }

                            match serde_json::from_str(&json_buffer) {
                                Ok(data) => {
                                    json_buffer.clear();
                                    yield Ok(data);
                                }
                                Err(_) => continue,
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(Error::Connection(e));
                        break;
                    }
                }
            }
        };

        Box::pin(stream)
    }

    async fn close(&mut self) -> Result<()> {
        self.ready = false;
        self.stdin = None;
        self.stdout = None;
        if let Some(mut process) = self.process.take() {
            let _ = process.kill().await;
            let _ = process.wait().await;
        }
        self.exit_error = None;
        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    async fn end_input(&mut self) -> Result<()> {
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
        }
        Ok(())
    }
}
