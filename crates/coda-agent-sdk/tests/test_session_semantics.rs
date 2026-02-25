#![cfg(unix)]

use coda_agent_sdk::{AgentOptions, AgentSdkClient, BackendKind, Message};
use futures::StreamExt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("failed to create temp directory");
        Self { path }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    fn write_executable_script(&self, name: &str, content: &str) -> PathBuf {
        let path = self.join(name);
        fs::write(&path, content).expect("failed to write script");
        let mut perms = fs::metadata(&path)
            .expect("failed to stat script")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("failed to chmod script");
        path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

async fn wait_for_pid_file(pid_file: &Path, timeout: Duration) -> i32 {
    let start = Instant::now();
    loop {
        if let Ok(content) = fs::read_to_string(pid_file) {
            let trimmed = content.trim();
            if let Ok(pid) = trimmed.parse::<i32>() {
                return pid;
            }
        }
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for pid file {}",
            pid_file.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_pid_exit(pid: i32, timeout: Duration) {
    let start = Instant::now();
    loop {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            return;
        }
        assert!(
            start.elapsed() < timeout,
            "process {pid} still alive after {:?}",
            timeout
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn build_fake_codex_cli_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "app-server" ]]; then
  if [[ -n "${PID_FILE:-}" ]]; then
    echo $$ > "$PID_FILE"
  fi
  thread_id="${THREAD_ID:-thread-1}"

  while IFS= read -r line; do
    if [[ "$line" == *'"method":"initialize"'* ]]; then
      id="$(echo "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p' || true)"
      echo "{\"jsonrpc\":\"2.0\",\"id\":${id:-1},\"result\":{}}"
    elif [[ "$line" == *'"method":"thread/start"'* ]]; then
      id="$(echo "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p' || true)"
      echo "{\"jsonrpc\":\"2.0\",\"id\":${id:-2},\"result\":{\"threadId\":\"$thread_id\"}}"
    elif [[ "$line" == *'"method":"turn/start"'* ]]; then
      prompt="$(echo "$line" | sed -n 's/.*"content":"\([^"]*\)".*/\1/p' || true)"
      echo "{\"jsonrpc\":\"2.0\",\"method\":\"item/completed\",\"params\":{\"item\":{\"type\":\"agent_message\",\"rawText\":\"$prompt\"}}}"
      echo "{\"jsonrpc\":\"2.0\",\"method\":\"turn/completed\",\"params\":{\"threadId\":\"$thread_id\",\"usage\":{}}}"
    elif [[ "$line" == *'"method":"turn/interrupt"'* ]]; then
      id="$(echo "$line" | sed -n 's/.*"id":[[:space:]]*\([0-9][0-9]*\).*/\1/p' || true)"
      echo "{\"jsonrpc\":\"2.0\",\"id\":${id:-3},\"result\":{}}"
    fi
  done

  if [[ "${LINGER_AFTER_EOF:-0}" == "1" ]]; then
    sleep 300
  fi
  exit 0
fi

echo "{}"
"#
}

fn build_fake_cursor_cli_script() -> &'static str {
    r#"#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${PID_FILE:-}" ]]; then
  echo $$ > "$PID_FILE"
fi

chat_id="${CURSOR_CHAT_ID:-chat-1}"
prompt="${@: -1}"

if [[ "$*" != *"--resume"* ]]; then
  echo "{\"type\":\"system\",\"subtype\":\"init\",\"chatId\":\"$chat_id\"}"
fi

echo "{\"type\":\"assistant\",\"text\":\"$prompt\"}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"$chat_id\",\"is_error\":false,\"num_turns\":1}"

if [[ "${LINGER_AFTER_OUTPUT:-0}" == "1" ]]; then
  sleep 300
fi
"#
}

#[tokio::test]
async fn codex_connect_text_prompt_is_not_auto_sent() {
    let temp = TempTestDir::new("codex-connect");
    let cli_path = temp.write_executable_script("codex", build_fake_codex_cli_script());

    let options = AgentOptions::builder()
        .backend(BackendKind::Codex)
        .cli_path(&cli_path)
        .build();

    let mut client = AgentSdkClient::new(Some(options), None);
    client
        .connect(Some("auto-should-not-send".into()))
        .await
        .expect("connect should succeed");

    let mut no_auto_stream = client.receive_response();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), no_auto_stream.next())
            .await
            .is_err(),
        "connect(text) should not auto-start a turn"
    );
    drop(no_auto_stream);

    client
        .query("manual-turn", "")
        .await
        .expect("manual query should succeed");
    let mut response_stream = client.receive_response();
    let mut saw_result = false;
    let mut attempts = 0;

    while attempts < 8 && !saw_result {
        attempts += 1;
        let item = tokio::time::timeout(Duration::from_secs(1), response_stream.next())
            .await
            .expect("timed out waiting for response item")
            .expect("response stream ended unexpectedly")
            .expect("response item should be Ok");

        if matches!(item, Message::Result(_)) {
            saw_result = true;
        }
    }

    assert!(saw_result, "did not receive result message");
    drop(response_stream);
    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
}

#[tokio::test]
async fn cursor_connect_text_prompt_is_not_auto_sent() {
    let temp = TempTestDir::new("cursor-connect");
    let cli_path = temp.write_executable_script("agent", build_fake_cursor_cli_script());

    let options = AgentOptions::builder()
        .backend(BackendKind::Cursor)
        .cli_path(&cli_path)
        .build();

    let mut client = AgentSdkClient::new(Some(options), None);
    client
        .connect(Some("auto-should-not-send".into()))
        .await
        .expect("connect should succeed");

    let mut no_auto_stream = client.receive_response();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), no_auto_stream.next())
            .await
            .is_err(),
        "connect(text) should not auto-start a turn"
    );
    drop(no_auto_stream);

    client
        .query("cursor-manual-turn", "")
        .await
        .expect("manual query should succeed");
    let mut response_stream = client.receive_response();
    let mut saw_result = false;
    let mut attempts = 0;

    while attempts < 8 && !saw_result {
        attempts += 1;
        let item = tokio::time::timeout(Duration::from_secs(1), response_stream.next())
            .await
            .expect("timed out waiting for response item")
            .expect("response stream ended unexpectedly")
            .expect("response item should be Ok");

        if matches!(item, Message::Result(_)) {
            saw_result = true;
        }
    }

    assert!(saw_result, "did not receive result message");
    drop(response_stream);
    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
}

#[tokio::test]
async fn cursor_receive_messages_stream_stays_open_after_result() {
    let temp = TempTestDir::new("cursor-stream");
    let cli_path = temp.write_executable_script("agent", build_fake_cursor_cli_script());

    let options = AgentOptions::builder()
        .backend(BackendKind::Cursor)
        .cli_path(&cli_path)
        .build();

    let mut client = AgentSdkClient::new(Some(options), None);
    client.connect(None).await.expect("connect should succeed");
    client
        .query("first-turn", "")
        .await
        .expect("query should succeed");

    let mut all_messages = client.receive_messages();
    let mut saw_result = false;
    for _ in 0..8 {
        let item = tokio::time::timeout(Duration::from_secs(1), all_messages.next())
            .await
            .expect("timed out waiting for message")
            .expect("message stream ended unexpectedly")
            .expect("message item should be Ok");
        if matches!(item, Message::Result(_)) {
            saw_result = true;
            break;
        }
    }
    assert!(saw_result, "first turn did not produce result");

    assert!(
        tokio::time::timeout(Duration::from_millis(250), all_messages.next())
            .await
            .is_err(),
        "receive_messages should remain open for session-continuous streaming"
    );

    drop(all_messages);
    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
}

#[tokio::test]
async fn codex_disconnect_terminates_lingering_process() {
    let temp = TempTestDir::new("codex-close");
    let cli_path = temp.write_executable_script("codex", build_fake_codex_cli_script());
    let pid_file = temp.join("codex.pid");

    let options = AgentOptions::builder()
        .backend(BackendKind::Codex)
        .cli_path(&cli_path)
        .env("PID_FILE", pid_file.to_string_lossy().to_string())
        .env("LINGER_AFTER_EOF", "1")
        .build();

    let mut client = AgentSdkClient::new(Some(options), None);
    client.connect(None).await.expect("connect should succeed");

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(2)).await;
    let start = Instant::now();
    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
    assert!(
        start.elapsed() < Duration::from_secs(13),
        "disconnect took too long: {:?}",
        start.elapsed()
    );
    wait_for_pid_exit(pid, Duration::from_secs(2)).await;
}

#[tokio::test]
async fn cursor_disconnect_terminates_lingering_process() {
    let temp = TempTestDir::new("cursor-close");
    let cli_path = temp.write_executable_script("agent", build_fake_cursor_cli_script());
    let pid_file = temp.join("cursor.pid");

    let options = AgentOptions::builder()
        .backend(BackendKind::Cursor)
        .cli_path(&cli_path)
        .env("PID_FILE", pid_file.to_string_lossy().to_string())
        .env("LINGER_AFTER_OUTPUT", "1")
        .build();

    let mut client = AgentSdkClient::new(Some(options), None);
    client.connect(None).await.expect("connect should succeed");
    client
        .query("close-check", "")
        .await
        .expect("query should succeed");

    let pid = wait_for_pid_file(&pid_file, Duration::from_secs(2)).await;
    let start = Instant::now();
    client
        .disconnect()
        .await
        .expect("disconnect should succeed");
    assert!(
        start.elapsed() < Duration::from_secs(13),
        "disconnect took too long: {:?}",
        start.elapsed()
    );
    wait_for_pid_exit(pid, Duration::from_secs(2)).await;
}
