//! Message parser for Cursor Agent CLI output.
//!
//! Translates Cursor Agent's `stream-json` output events into SDK [`Message`] types.
//!
//! ## Cursor Event Mapping
//!
//! | Cursor Event | SDK Message |
//! |---|---|
//! | `{ type: "system", subtype: "init" }` | `SystemMessage { subtype: "init" }` |
//! | `{ type: "assistant" }` | `AssistantMessage { content: [TextBlock] }` |
//! | `{ type: "thinking" }` | `AssistantMessage { content: [ThinkingBlock] }` |
//! | `{ type: "tool_call", subtype: "started" }` | `AssistantMessage { content: [ToolUseBlock] }` |
//! | `{ type: "tool_call", subtype: "completed" }` | `AssistantMessage { content: [ToolResultBlock] }` |
//! | `{ type: "result" }` | `ResultMessage` |

use crate::error::Result;
use crate::types::*;
use serde_json::Value;

/// Parse a Cursor Agent stream-json event into a [`Message`].
///
/// Returns `Ok(None)` for unrecognized event types (forward compatibility).
pub fn parse_cursor_event(data: &Value) -> Result<Option<Message>> {
    let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "system" => parse_system_event(data),
        "assistant" => parse_assistant_event(data),
        "thinking" => parse_thinking_event(data),
        "tool_call" => parse_tool_call_event(data),
        "result" => parse_result_event(data),
        "user" => parse_user_event(data),
        _ => {
            tracing::debug!("Skipping unknown Cursor event type: {}", event_type);
            Ok(None)
        }
    }
}

fn parse_system_event(data: &Value) -> Result<Option<Message>> {
    let subtype = data
        .get("subtype")
        .and_then(|v| v.as_str())
        .unwrap_or("init")
        .to_string();

    let mut event_data = serde_json::Map::new();
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            event_data.insert(k.clone(), v.clone());
        }
    }

    Ok(Some(Message::System(SystemMessage {
        subtype,
        data: Value::Object(event_data),
    })))
}

fn parse_assistant_event(data: &Value) -> Result<Option<Message>> {
    let message = data.get("message").unwrap_or(data);
    let content = message.get("content");

    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            // Extract text from content blocks
            arr.iter()
                .filter_map(|block| {
                    let block_type = block.get("type").and_then(|v| v.as_str())?;
                    if block_type == "text" {
                        block.get("text").and_then(|v| v.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        }
        _ => data
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    };

    if text.is_empty() {
        return Ok(None);
    }

    let model = message
        .get("model")
        .or_else(|| data.get("model"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Some(Message::Assistant(AssistantMessage {
        content: vec![ContentBlock::Text(TextBlock { text })],
        model,
        parent_tool_use_id: data
            .get("parent_tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        error: None,
    })))
}

fn parse_thinking_event(data: &Value) -> Result<Option<Message>> {
    let thinking = data
        .get("thinking")
        .or_else(|| data.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if thinking.is_empty() {
        return Ok(None);
    }

    let signature = data
        .get("signature")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Some(Message::Assistant(AssistantMessage {
        content: vec![ContentBlock::Thinking(ThinkingBlock {
            thinking,
            signature,
        })],
        model: String::new(),
        parent_tool_use_id: None,
        error: None,
    })))
}

fn parse_tool_call_event(data: &Value) -> Result<Option<Message>> {
    let subtype = data.get("subtype").and_then(|v| v.as_str()).unwrap_or("");

    match subtype {
        "started" => {
            let id = data
                .get("id")
                .or_else(|| data.get("tool_use_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = data
                .get("name")
                .or_else(|| data.get("tool_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input = data
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));

            Ok(Some(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolUse(ToolUseBlock { id, name, input })],
                model: String::new(),
                parent_tool_use_id: data
                    .get("parent_tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                error: None,
            })))
        }
        "completed" => {
            let tool_use_id = data
                .get("tool_use_id")
                .or_else(|| data.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = data.get("output").or_else(|| data.get("content")).cloned();
            let is_error = data.get("is_error").and_then(|v| v.as_bool());

            Ok(Some(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::ToolResult(ToolResultBlock {
                    tool_use_id,
                    content,
                    is_error,
                })],
                model: String::new(),
                parent_tool_use_id: None,
                error: None,
            })))
        }
        _ => Ok(None),
    }
}

fn parse_result_event(data: &Value) -> Result<Option<Message>> {
    let subtype = data
        .get("subtype")
        .and_then(|v| v.as_str())
        .unwrap_or("success")
        .to_string();
    let duration_ms = data
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let duration_api_ms = data
        .get("duration_api_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let is_error = data
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let num_turns = data.get("num_turns").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let session_id = data
        .get("session_id")
        .or_else(|| data.get("chatId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Some(Message::Result(ResultMessage {
        subtype,
        duration_ms,
        duration_api_ms,
        is_error,
        num_turns,
        session_id,
        total_cost_usd: data.get("total_cost_usd").and_then(|v| v.as_f64()),
        usage: data.get("usage").cloned(),
        result: data
            .get("result")
            .and_then(|v| v.as_str())
            .map(String::from),
        structured_output: data.get("structured_output").cloned(),
    })))
}

fn parse_user_event(data: &Value) -> Result<Option<Message>> {
    let message = data.get("message").unwrap_or(data);
    let content = match message.get("content") {
        Some(Value::String(s)) => UserContent::String(s.clone()),
        Some(Value::Array(_)) => {
            // For simplicity, convert array to string
            UserContent::String(
                message
                    .get("content")
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            )
        }
        _ => UserContent::String(String::new()),
    };

    Ok(Some(Message::User(UserMessage {
        content,
        uuid: data.get("uuid").and_then(|v| v.as_str()).map(String::from),
        parent_tool_use_id: data
            .get("parent_tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        tool_use_result: data.get("tool_use_result").cloned(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_should_parse_system_init() {
        let data = json!({"type": "system", "subtype": "init", "chatId": "abc-123"});
        let msg = parse_cursor_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::System(s) => {
                assert_eq!(s.subtype, "init");
            }
            _ => panic!("expected SystemMessage"),
        }
    }

    #[test]
    fn test_should_parse_assistant_text() {
        let data = json!({"type": "assistant", "text": "Hello from Cursor!", "model": "gpt-5"});
        let msg = parse_cursor_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.content.len(), 1);
                match &a.content[0] {
                    ContentBlock::Text(t) => assert_eq!(t.text, "Hello from Cursor!"),
                    _ => panic!("expected TextBlock"),
                }
                assert_eq!(a.model, "gpt-5");
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_tool_call_started() {
        let data = json!({
            "type": "tool_call",
            "subtype": "started",
            "id": "tc-1",
            "name": "Read",
            "input": {"file_path": "/foo.txt"}
        });
        let msg = parse_cursor_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => match &a.content[0] {
                ContentBlock::ToolUse(t) => {
                    assert_eq!(t.id, "tc-1");
                    assert_eq!(t.name, "Read");
                    assert_eq!(t.input["file_path"], "/foo.txt");
                }
                _ => panic!("expected ToolUseBlock"),
            },
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_tool_call_completed() {
        let data = json!({
            "type": "tool_call",
            "subtype": "completed",
            "tool_use_id": "tc-1",
            "output": "file contents here",
            "is_error": false
        });
        let msg = parse_cursor_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => match &a.content[0] {
                ContentBlock::ToolResult(t) => {
                    assert_eq!(t.tool_use_id, "tc-1");
                    assert_eq!(t.is_error, Some(false));
                }
                _ => panic!("expected ToolResultBlock"),
            },
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_result_event() {
        let data = json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 5000,
            "session_id": "chat-456",
            "is_error": false,
            "num_turns": 3
        });
        let msg = parse_cursor_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Result(r) => {
                assert_eq!(r.subtype, "success");
                assert_eq!(r.duration_ms, 5000);
                assert_eq!(r.session_id, "chat-456");
                assert_eq!(r.num_turns, 3);
            }
            _ => panic!("expected ResultMessage"),
        }
    }

    #[test]
    fn test_should_return_none_for_unknown_type() {
        let data = json!({"type": "unknown_event_type"});
        let msg = parse_cursor_event(&data).unwrap();
        assert!(msg.is_none());
    }
}
