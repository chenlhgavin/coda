//! Tests for type definitions - corresponds to Python test_types.py

use coda_agent_sdk::types::*;
use coda_agent_sdk::{AgentOptions, AgentOptionsBuilder};

#[test]
fn test_user_message_creation() {
    let msg = UserMessage {
        content: UserContent::String("Hello, Claude!".to_string()),
        uuid: None,
        parent_tool_use_id: None,
        tool_use_result: None,
    };
    match &msg.content {
        UserContent::String(s) => assert_eq!(s, "Hello, Claude!"),
        _ => panic!("expected string content"),
    }
}

#[test]
fn test_assistant_message_with_text() {
    let text_block = TextBlock {
        text: "Hello, human!".to_string(),
    };
    let msg = AssistantMessage {
        content: vec![ContentBlock::Text(text_block)],
        model: "claude-opus-4-1-20250805".to_string(),
        parent_tool_use_id: None,
        error: None,
    };
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::Text(t) => assert_eq!(t.text, "Hello, human!"),
        _ => panic!("expected TextBlock"),
    }
}

#[test]
fn test_tool_use_block() {
    let block = ToolUseBlock {
        id: "tool-123".to_string(),
        name: "Read".to_string(),
        input: serde_json::json!({"file_path": "/test.txt"}),
    };
    assert_eq!(block.id, "tool-123");
    assert_eq!(block.name, "Read");
    assert_eq!(block.input["file_path"], "/test.txt");
}

#[test]
fn test_tool_result_block() {
    let block = ToolResultBlock {
        tool_use_id: "tool-123".to_string(),
        content: Some(serde_json::Value::String("File contents here".to_string())),
        is_error: Some(false),
    };
    assert_eq!(block.tool_use_id, "tool-123");
    assert_eq!(
        block.content.as_ref().and_then(|v| v.as_str()),
        Some("File contents here")
    );
    assert_eq!(block.is_error, Some(false));
}

#[test]
fn test_result_message() {
    let msg = ResultMessage {
        subtype: "success".to_string(),
        duration_ms: 1500,
        duration_api_ms: 1200,
        is_error: false,
        num_turns: 1,
        session_id: "session-123".to_string(),
        total_cost_usd: Some(0.01),
        usage: None,
        result: None,
        structured_output: None,
    };
    assert_eq!(msg.subtype, "success");
    assert_eq!(msg.total_cost_usd, Some(0.01));
    assert_eq!(msg.session_id, "session-123");
}

#[test]
fn test_default_options() {
    let options = AgentOptions::default();
    assert!(options.allowed_tools.is_empty());
    assert!(options.system_prompt.is_none());
    assert!(options.permission_mode.is_none());
    assert!(!options.continue_conversation);
    assert!(options.disallowed_tools.is_empty());
}

#[test]
fn test_options_with_tools() {
    let options = AgentOptionsBuilder::new()
        .allowed_tools(["Read", "Write", "Edit"])
        .disallowed_tools(["Bash"])
        .build();
    assert_eq!(options.allowed_tools, vec!["Read", "Write", "Edit"]);
    assert_eq!(options.disallowed_tools, vec!["Bash"]);
}

#[test]
fn test_options_with_permission_mode() {
    use coda_agent_sdk::PermissionMode;
    let options = AgentOptionsBuilder::new()
        .permission_mode("bypassPermissions")
        .build();
    assert_eq!(
        options.permission_mode,
        Some(PermissionMode::BypassPermissions)
    );
}

#[test]
fn test_options_with_system_prompt() {
    let options = AgentOptionsBuilder::new()
        .system_prompt("You are a helpful assistant.")
        .build();
    assert!(options.system_prompt.is_some());
}
