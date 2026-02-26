//! Message parser for Codex CLI output.
//!
//! Translates Codex `exec --json` events and app-server JSON-RPC notifications
//! into the SDK's unified [`Message`] types.
//!
//! ## Codex Event Mapping
//!
//! | Codex Event / Notification | SDK Message |
//! |---|---|
//! | `thread/started` | `SystemMessage { subtype: "init" }` |
//! | `item/completed` (agent_message) | `AssistantMessage { content: [TextBlock] }` |
//! | `item/completed` (reasoning) | `AssistantMessage { content: [ThinkingBlock] }` |
//! | `item/completed` (command_execution) | `AssistantMessage { content: [ToolUseBlock, ToolResultBlock] }` |
//! | `item/completed` (file_change) | `AssistantMessage { content: [ToolUseBlock, ToolResultBlock] }` |
//! | `turn/completed` | `ResultMessage { usage, duration_ms, ... }` |
//! | `item/agentMessage/delta` | `StreamEvent { content_block_delta }` (streaming) |

use crate::error::Result;
use crate::types::*;
use serde_json::Value;

/// Parse a Codex `exec --json` output event into a [`Message`].
///
/// Codex exec outputs JSONL events; each line is one event object.
/// Handles both the newer flat format (`type: "message"`) and the
/// item-based format (`type: "item.completed"`) that production
/// Codex CLIs emit.
pub fn parse_exec_event(data: &Value) -> Result<Option<Message>> {
    let event_type = data.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        // Newer flat format
        "message" => parse_exec_message(data),
        "function_call" => parse_exec_function_call(data),
        "function_call_output" => parse_exec_function_output(data),
        // Item-based format emitted by production Codex exec --json
        "item.completed" => parse_item_completed(data),
        "turn.completed" => parse_turn_completed(data),
        "thread.started" => parse_exec_thread_started(data),
        "turn.started" => Ok(Some(Message::System(SystemMessage {
            subtype: "turn_started".to_string(),
            data: data.clone(),
        }))),
        "error" => {
            let message = data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            Ok(Some(Message::System(SystemMessage {
                subtype: "error".to_string(),
                data: serde_json::json!({"type": "system", "subtype": "error", "message": message}),
            })))
        }
        "turn.failed" => {
            let message = data
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Turn failed")
                .to_string();
            Ok(Some(Message::System(SystemMessage {
                subtype: "error".to_string(),
                data: serde_json::json!({"type": "system", "subtype": "error", "message": message}),
            })))
        }
        _ => Ok(None),
    }
}

/// Parses a `thread.started` event from exec JSONL output.
///
/// Exec mode uses `thread_id` (snake_case) while app-server uses
/// `threadId` (camelCase).
fn parse_exec_thread_started(data: &Value) -> Result<Option<Message>> {
    let thread_id = data
        .get("threadId")
        .or_else(|| data.get("thread_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut map = serde_json::Map::new();
    map.insert("type".to_string(), Value::String("system".to_string()));
    map.insert("subtype".to_string(), Value::String("init".to_string()));
    map.insert("threadId".to_string(), Value::String(thread_id));
    Ok(Some(Message::System(SystemMessage {
        subtype: "init".to_string(),
        data: Value::Object(map),
    })))
}

fn parse_exec_message(data: &Value) -> Result<Option<Message>> {
    let role = data.get("role").and_then(|v| v.as_str()).unwrap_or("");
    let content_val = data.get("content");

    match role {
        "assistant" => {
            let text = content_val
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                return Ok(None);
            }
            Ok(Some(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::Text(TextBlock { text })],
                model: String::new(),
                parent_tool_use_id: None,
                error: None,
            })))
        }
        "system" => Ok(Some(Message::System(SystemMessage {
            subtype: "init".to_string(),
            data: data.clone(),
        }))),
        _ => Ok(None),
    }
}

fn parse_exec_function_call(data: &Value) -> Result<Option<Message>> {
    let call_id = data
        .get("call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = data
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let arguments = data
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let input: Value = serde_json::from_str(arguments).unwrap_or(Value::Object(Default::default()));

    Ok(Some(Message::Assistant(AssistantMessage {
        content: vec![ContentBlock::ToolUse(ToolUseBlock {
            id: call_id,
            name,
            input,
        })],
        model: String::new(),
        parent_tool_use_id: None,
        error: None,
    })))
}

fn parse_exec_function_output(data: &Value) -> Result<Option<Message>> {
    let call_id = data
        .get("call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let output = data.get("output").cloned();

    Ok(Some(Message::Assistant(AssistantMessage {
        content: vec![ContentBlock::ToolResult(ToolResultBlock {
            tool_use_id: call_id,
            content: output,
            is_error: None,
        })],
        model: String::new(),
        parent_tool_use_id: None,
        error: None,
    })))
}

/// Parse a Codex app-server JSON-RPC notification into a [`Message`].
///
/// The app-server protocol uses JSON-RPC 2.0 notifications with `method` and `params`.
pub fn parse_app_server_notification(method: &str, params: &Value) -> Result<Option<Message>> {
    match method {
        "thread/started" => {
            let thread_id = params
                .get("threadId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut data = serde_json::Map::new();
            data.insert("type".to_string(), Value::String("system".to_string()));
            data.insert("subtype".to_string(), Value::String("init".to_string()));
            data.insert("threadId".to_string(), Value::String(thread_id));
            Ok(Some(Message::System(SystemMessage {
                subtype: "init".to_string(),
                data: Value::Object(data),
            })))
        }
        "item/completed" => parse_item_completed(params),
        "item/agentMessage/delta" => parse_agent_message_delta(params),
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            parse_reasoning_delta(params)
        }
        "item/commandExecution/outputDelta" => parse_command_output_delta(params),
        "item/fileChange/outputDelta" => parse_file_change_delta(params),
        "turn/completed" => parse_turn_completed(params),
        "turn/started" => Ok(Some(Message::System(SystemMessage {
            subtype: "turn_started".to_string(),
            data: params.clone(),
        }))),
        "error" => {
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error")
                .to_string();
            Ok(Some(Message::System(SystemMessage {
                subtype: "error".to_string(),
                data: serde_json::json!({"type": "system", "subtype": "error", "message": message}),
            })))
        }
        _ => {
            tracing::debug!("Skipping unknown Codex notification: {}", method);
            Ok(None)
        }
    }
}

fn parse_item_completed(params: &Value) -> Result<Option<Message>> {
    let item = params.get("item").unwrap_or(params);
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match item_type {
        "agent_message" | "message" => {
            let content_arr = item.get("content").and_then(|v| v.as_array());
            let text = if let Some(arr) = content_arr {
                arr.iter()
                    .filter_map(|c| {
                        if c.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                            c.get("text").and_then(|v| v.as_str())
                        } else {
                            c.get("text").and_then(|v| v.as_str())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            } else {
                item.get("rawText")
                    .or_else(|| item.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };

            if text.is_empty() {
                return Ok(None);
            }

            Ok(Some(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::Text(TextBlock { text })],
                model: item
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                parent_tool_use_id: None,
                error: None,
            })))
        }
        "reasoning" => {
            let text = item
                .get("summary")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.get("text").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .or_else(|| item.get("text").and_then(|v| v.as_str()).map(String::from))
                .unwrap_or_default();

            if text.is_empty() {
                return Ok(None);
            }

            Ok(Some(Message::Assistant(AssistantMessage {
                content: vec![ContentBlock::Thinking(ThinkingBlock {
                    thinking: text,
                    signature: String::new(),
                })],
                model: String::new(),
                parent_tool_use_id: None,
                error: None,
            })))
        }
        "command_execution" => {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let command = item
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let output = item.get("output").cloned();
            let exit_code = item.get("exitCode").and_then(|v| v.as_i64());

            let mut blocks = vec![ContentBlock::ToolUse(ToolUseBlock {
                id: id.clone(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": command}),
            })];

            blocks.push(ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: id,
                content: output,
                is_error: exit_code.map(|c| c != 0),
            }));

            Ok(Some(Message::Assistant(AssistantMessage {
                content: blocks,
                model: String::new(),
                parent_tool_use_id: None,
                error: None,
            })))
        }
        "file_change" => {
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let file_path = item
                .get("filePath")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let mut blocks = vec![ContentBlock::ToolUse(ToolUseBlock {
                id: id.clone(),
                name: "Edit".to_string(),
                input: serde_json::json!({"file_path": file_path}),
            })];

            blocks.push(ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: id,
                content: Some(serde_json::json!("File change applied")),
                is_error: Some(false),
            }));

            Ok(Some(Message::Assistant(AssistantMessage {
                content: blocks,
                model: String::new(),
                parent_tool_use_id: None,
                error: None,
            })))
        }
        _ => {
            tracing::debug!("Skipping unknown Codex item type: {}", item_type);
            Ok(None)
        }
    }
}

fn parse_agent_message_delta(params: &Value) -> Result<Option<Message>> {
    let text = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");

    if text.is_empty() {
        return Ok(None);
    }

    // Emit as StreamEvent so session.rs extract_text_delta() picks it up
    // and forwards it to the TUI via SessionEvent::TextDelta.
    // Using Message::Assistant here would: (a) not trigger streaming display,
    // (b) double-count text with the final item/completed message, and
    // (c) spuriously increment turn_count.
    Ok(Some(Message::StreamEvent(StreamEvent {
        uuid: String::new(),
        session_id: String::new(),
        event: serde_json::json!({
            "type": "content_block_delta",
            "delta": {
                "type": "text_delta",
                "text": text,
            }
        }),
        parent_tool_use_id: None,
    })))
}

fn parse_reasoning_delta(params: &Value) -> Result<Option<Message>> {
    let text = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");

    if text.is_empty() {
        return Ok(None);
    }

    Ok(Some(Message::Assistant(AssistantMessage {
        content: vec![ContentBlock::Thinking(ThinkingBlock {
            thinking: text.to_string(),
            signature: String::new(),
        })],
        model: String::new(),
        parent_tool_use_id: None,
        error: None,
    })))
}

fn parse_command_output_delta(params: &Value) -> Result<Option<Message>> {
    let delta = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");

    if delta.is_empty() {
        return Ok(None);
    }

    Ok(Some(Message::System(SystemMessage {
        subtype: "command_output".to_string(),
        data: serde_json::json!({
            "type": "system",
            "subtype": "command_output",
            "output": delta,
        }),
    })))
}

fn parse_file_change_delta(params: &Value) -> Result<Option<Message>> {
    let delta = params.get("delta").and_then(|v| v.as_str()).unwrap_or("");

    if delta.is_empty() {
        return Ok(None);
    }

    Ok(Some(Message::System(SystemMessage {
        subtype: "file_change_output".to_string(),
        data: serde_json::json!({
            "type": "system",
            "subtype": "file_change_output",
            "output": delta,
        }),
    })))
}

fn parse_turn_completed(params: &Value) -> Result<Option<Message>> {
    let usage = params.get("usage").cloned();
    let thread_id = params
        .get("threadId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(Some(Message::Result(ResultMessage {
        subtype: "success".to_string(),
        duration_ms: 0,
        duration_api_ms: 0,
        is_error: false,
        num_turns: 1,
        session_id: thread_id,
        total_cost_usd: None,
        usage,
        result: None,
        structured_output: None,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_should_parse_thread_started_as_system_init() {
        let msg =
            parse_app_server_notification("thread/started", &json!({"threadId": "t-123"})).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::System(s) => {
                assert_eq!(s.subtype, "init");
                assert_eq!(s.data["threadId"], "t-123");
            }
            _ => panic!("expected SystemMessage"),
        }
    }

    #[test]
    fn test_should_parse_agent_message_item_completed() {
        let params = json!({
            "item": {
                "type": "agent_message",
                "rawText": "Hello from Codex!",
                "model": "o4-mini"
            }
        });
        let msg = parse_app_server_notification("item/completed", &params).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.content.len(), 1);
                match &a.content[0] {
                    ContentBlock::Text(t) => assert_eq!(t.text, "Hello from Codex!"),
                    _ => panic!("expected TextBlock"),
                }
                assert_eq!(a.model, "o4-mini");
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_command_execution_item_completed() {
        let params = json!({
            "item": {
                "type": "command_execution",
                "id": "cmd-1",
                "command": "ls -la",
                "output": "total 42\n...",
                "exitCode": 0
            }
        });
        let msg = parse_app_server_notification("item/completed", &params).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.content.len(), 2);
                match &a.content[0] {
                    ContentBlock::ToolUse(t) => {
                        assert_eq!(t.name, "Bash");
                        assert_eq!(t.input["command"], "ls -la");
                    }
                    _ => panic!("expected ToolUseBlock"),
                }
                match &a.content[1] {
                    ContentBlock::ToolResult(t) => {
                        assert_eq!(t.tool_use_id, "cmd-1");
                        assert_eq!(t.is_error, Some(false));
                    }
                    _ => panic!("expected ToolResultBlock"),
                }
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_turn_completed_as_result() {
        let params = json!({
            "threadId": "t-456",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });
        let msg = parse_app_server_notification("turn/completed", &params).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Result(r) => {
                assert_eq!(r.session_id, "t-456");
                assert!(r.usage.is_some());
            }
            _ => panic!("expected ResultMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_message() {
        let data = json!({
            "type": "message",
            "role": "assistant",
            "content": "Hello from codex exec"
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.content.len(), 1);
                match &a.content[0] {
                    ContentBlock::Text(t) => assert_eq!(t.text, "Hello from codex exec"),
                    _ => panic!("expected TextBlock"),
                }
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_function_call() {
        let data = json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": "shell",
            "arguments": "{\"command\": \"echo hi\"}"
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => match &a.content[0] {
                ContentBlock::ToolUse(t) => {
                    assert_eq!(t.id, "call-1");
                    assert_eq!(t.name, "shell");
                    assert_eq!(t.input["command"], "echo hi");
                }
                _ => panic!("expected ToolUseBlock"),
            },
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_return_none_for_unknown_notification() {
        let msg = parse_app_server_notification("unknown/method", &json!({})).unwrap();
        assert!(msg.is_none());
    }

    // ── parse_exec_event: item-based format tests ──────────────

    #[test]
    fn test_should_parse_exec_item_completed_agent_message() {
        let data = json!({
            "type": "item.completed",
            "item": {
                "id": "item_0",
                "type": "agent_message",
                "text": "```yaml\nissues: []\n```"
            }
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.content.len(), 1);
                match &a.content[0] {
                    ContentBlock::Text(t) => assert!(t.text.contains("issues: []")),
                    _ => panic!("expected TextBlock"),
                }
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_item_completed_command_execution() {
        let data = json!({
            "type": "item.completed",
            "item": {
                "id": "cmd-1",
                "type": "command_execution",
                "command": "git diff main -- src/main.rs",
                "output": "+new line",
                "exitCode": 0
            }
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Assistant(a) => {
                assert_eq!(a.content.len(), 2);
                match &a.content[0] {
                    ContentBlock::ToolUse(t) => {
                        assert_eq!(t.name, "Bash");
                        assert_eq!(t.input["command"], "git diff main -- src/main.rs");
                    }
                    _ => panic!("expected ToolUseBlock"),
                }
            }
            _ => panic!("expected AssistantMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_turn_completed() {
        let data = json!({
            "type": "turn.completed",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::Result(r) => {
                assert!(r.usage.is_some());
            }
            _ => panic!("expected ResultMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_thread_started_snake_case() {
        let data = json!({
            "type": "thread.started",
            "thread_id": "019c7fd5"
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::System(s) => {
                assert_eq!(s.subtype, "init");
                assert_eq!(s.data["threadId"], "019c7fd5");
            }
            _ => panic!("expected SystemMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_error_event() {
        let data = json!({
            "type": "error",
            "message": "Model not supported"
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::System(s) => {
                assert_eq!(s.subtype, "error");
                assert_eq!(s.data["message"], "Model not supported");
            }
            _ => panic!("expected SystemMessage"),
        }
    }

    #[test]
    fn test_should_parse_exec_turn_failed_event() {
        let data = json!({
            "type": "turn.failed",
            "error": {"message": "The model is not supported"}
        });
        let msg = parse_exec_event(&data).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::System(s) => {
                assert_eq!(s.subtype, "error");
                assert_eq!(s.data["message"], "The model is not supported");
            }
            _ => panic!("expected SystemMessage"),
        }
    }

    #[test]
    fn test_should_return_none_for_unknown_exec_event() {
        let data = json!({"type": "some.unknown.event"});
        let msg = parse_exec_event(&data).unwrap();
        assert!(msg.is_none());
    }

    // ── parse_agent_message_delta tests ──────────────────────────

    #[test]
    fn test_should_parse_agent_message_delta_as_stream_event() {
        let params = json!({"delta": "Hello from Codex"});
        let msg = parse_app_server_notification("item/agentMessage/delta", &params).unwrap();
        let msg = msg.expect("should parse");
        match msg {
            Message::StreamEvent(se) => {
                assert_eq!(se.event["type"], "content_block_delta");
                assert_eq!(se.event["delta"]["type"], "text_delta");
                assert_eq!(se.event["delta"]["text"], "Hello from Codex");
            }
            _ => panic!("expected StreamEvent, got {:?}", msg),
        }
    }

    #[test]
    fn test_should_return_none_for_empty_agent_message_delta() {
        let params = json!({"delta": ""});
        let msg = parse_app_server_notification("item/agentMessage/delta", &params).unwrap();
        assert!(msg.is_none());
    }

    #[test]
    fn test_should_return_none_for_missing_agent_message_delta() {
        let params = json!({});
        let msg = parse_app_server_notification("item/agentMessage/delta", &params).unwrap();
        assert!(msg.is_none());
    }
}
