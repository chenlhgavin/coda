//! Tests for message parser - corresponds to Python test_message_parser.py

use coda_agent_sdk::{ContentBlock, Error, Message, UserContent, parse_message};
use serde_json::json;

#[test]
fn test_parse_valid_user_message() {
    let data = json!({
        "type": "user",
        "message": {"content": [{"type": "text", "text": "Hello"}]}
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::User(u) => match &u.content {
            UserContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    ContentBlock::Text(t) => assert_eq!(t.text, "Hello"),
                    _ => panic!("expected TextBlock"),
                }
            }
            _ => panic!("expected Blocks"),
        },
        _ => panic!("expected UserMessage"),
    }
}

#[test]
fn test_parse_user_message_with_uuid() {
    let data = json!({
        "type": "user",
        "uuid": "msg-abc123-def456",
        "message": {"content": [{"type": "text", "text": "Hello"}]}
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::User(u) => assert_eq!(u.uuid.as_deref(), Some("msg-abc123-def456")),
        _ => panic!("expected UserMessage"),
    }
}

#[test]
fn test_parse_user_message_with_tool_use() {
    let data = json!({
        "type": "user",
        "message": {
            "content": [
                {"type": "text", "text": "Let me read this file"},
                {"type": "tool_use", "id": "tool_456", "name": "Read", "input": {"file_path": "/example.txt"}}
            ]
        }
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::User(u) => match &u.content {
            UserContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                match &blocks[1] {
                    ContentBlock::ToolUse(t) => {
                        assert_eq!(t.id, "tool_456");
                        assert_eq!(t.name, "Read");
                        assert_eq!(t.input["file_path"], "/example.txt");
                    }
                    _ => panic!("expected ToolUseBlock"),
                }
            }
            _ => panic!("expected Blocks"),
        },
        _ => panic!("expected UserMessage"),
    }
}

#[test]
fn test_parse_user_message_with_tool_result() {
    let data = json!({
        "type": "user",
        "message": {
            "content": [{"type": "tool_result", "tool_use_id": "tool_789", "content": "File contents here"}]
        }
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::User(u) => match &u.content {
            UserContent::Blocks(blocks) => match &blocks[0] {
                ContentBlock::ToolResult(t) => {
                    assert_eq!(t.tool_use_id, "tool_789");
                    assert_eq!(
                        t.content.as_ref().and_then(|v| v.as_str()),
                        Some("File contents here")
                    );
                }
                _ => panic!("expected ToolResultBlock"),
            },
            _ => panic!("expected Blocks"),
        },
        _ => panic!("expected UserMessage"),
    }
}

#[test]
fn test_parse_valid_assistant_message() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "tool_use", "id": "tool_123", "name": "Read", "input": {"file_path": "/test.txt"}}
            ],
            "model": "claude-opus-4-1-20250805"
        }
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::Assistant(a) => {
            assert_eq!(a.content.len(), 2);
            assert_eq!(a.model, "claude-opus-4-1-20250805");
        }
        _ => panic!("expected AssistantMessage"),
    }
}

#[test]
fn test_parse_assistant_message_with_thinking() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [
                {"type": "thinking", "thinking": "I'm thinking about the answer...", "signature": "sig-123"},
                {"type": "text", "text": "Here's my response"}
            ],
            "model": "claude-opus-4-1-20250805"
        }
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::Assistant(a) => match &a.content[0] {
            ContentBlock::Thinking(t) => {
                assert_eq!(t.thinking, "I'm thinking about the answer...");
                assert_eq!(t.signature, "sig-123");
            }
            _ => panic!("expected ThinkingBlock"),
        },
        _ => panic!("expected AssistantMessage"),
    }
}

#[test]
fn test_parse_valid_system_message() {
    let data = json!({"type": "system", "subtype": "start"});
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::System(s) => assert_eq!(s.subtype, "start"),
        _ => panic!("expected SystemMessage"),
    }
}

#[test]
fn test_parse_valid_result_message() {
    let data = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1000,
        "duration_api_ms": 500,
        "is_error": false,
        "num_turns": 2,
        "session_id": "session_123"
    });
    let message = parse_message(&data).unwrap();
    let msg = message.expect("should parse");
    match &msg {
        Message::Result(r) => {
            assert_eq!(r.subtype, "success");
            assert_eq!(r.duration_ms, 1000);
        }
        _ => panic!("expected ResultMessage"),
    }
}

#[test]
fn test_parse_invalid_data_type() {
    let data = json!("not a dict");
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(s.contains("expected object"));
        assert!(s.contains("string"));
    }
}

#[test]
fn test_parse_missing_type_field() {
    let data = json!({"message": {"content": []}});
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(s.contains("'type'"));
    }
}

#[test]
fn test_parse_unknown_message_type() {
    let data = json!({"type": "unknown_type"});
    let message = parse_message(&data).unwrap();
    assert!(message.is_none());
}

#[test]
fn test_parse_user_message_missing_fields() {
    let data = json!({"type": "user"});
    let result = parse_message(&data);
    assert!(result.is_err());
}

// ============ Strict field requirement tests (Issue 11) ============

#[test]
fn test_result_message_missing_is_error() {
    let data = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1000,
        "duration_api_ms": 500,
        "num_turns": 2,
        "session_id": "session_123"
    });
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(s.contains("is_error"), "Expected is_error in error: {}", s);
    }
}

#[test]
fn test_result_message_missing_num_turns() {
    let data = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1000,
        "duration_api_ms": 500,
        "is_error": false,
        "session_id": "session_123"
    });
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(
            s.contains("num_turns"),
            "Expected num_turns in error: {}",
            s
        );
    }
}

#[test]
fn test_result_message_missing_session_id() {
    let data = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1000,
        "duration_api_ms": 500,
        "is_error": false,
        "num_turns": 2
    });
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(
            s.contains("session_id"),
            "Expected session_id in error: {}",
            s
        );
    }
}

#[test]
fn test_thinking_block_missing_signature() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [
                {"type": "thinking", "thinking": "I'm thinking..."}
            ],
            "model": "claude-opus-4-1-20250805"
        }
    });
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(
            s.contains("signature"),
            "Expected signature in error: {}",
            s
        );
    }
}

#[test]
fn test_tool_use_block_missing_input() {
    let data = json!({
        "type": "assistant",
        "message": {
            "content": [
                {"type": "tool_use", "id": "tool_123", "name": "Read"}
            ],
            "model": "claude-opus-4-1-20250805"
        }
    });
    let result = parse_message(&data);
    assert!(result.is_err());
    if let Err(Error::MessageParse(s)) = result {
        assert!(s.contains("input"), "Expected input in error: {}", s);
    }
}
