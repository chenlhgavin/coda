//! Tests for error types - corresponds to Python test_errors.py

use coda_agent_sdk::Error;

#[test]
fn test_base_error() {
    let error = Error::Other("Something went wrong".to_string());
    assert_eq!(error.to_string(), "Something went wrong");
}

#[test]
fn test_cli_not_found_error() {
    let error = Error::CliNotFound("Claude Code not found".to_string());
    assert!(error.to_string().contains("Claude Code not found"));
}

#[test]
fn test_connection_error() {
    let error = Error::Connection(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "Failed to connect to CLI",
    ));
    assert!(error.to_string().contains("Failed to connect"));
}

#[test]
fn test_process_error() {
    let error = Error::Process {
        exit_code: 1,
        stderr: Some("Command not found".to_string()),
    };
    let s = error.to_string();
    assert!(s.contains("Process failed"));
    assert!(s.contains("exit code"));
    assert!(s.contains("1"));
}

#[test]
fn test_json_decode_error() {
    let inner = serde_json::from_str::<serde_json::Value>("{invalid json}").unwrap_err();
    let error = Error::JsonDecode(inner);
    assert!(error.to_string().contains("JSON") || error.to_string().contains("json"));
}

#[test]
fn test_not_connected_error() {
    let error = Error::NotConnected;
    let s = error.to_string();
    assert!(s.contains("Not connected"));
    assert!(s.contains("connect()"));
}
