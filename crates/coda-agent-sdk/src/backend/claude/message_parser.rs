//! Message parser for Claude CLI output.
//!
//! Parses JSON messages from the Claude CLI's `stream-json` output format
//! into the SDK's [`Message`] types.

use crate::error::{Error, Result};
use crate::types::*;
use serde_json::Value;

/// Parse a JSON message from Claude CLI output into a typed [`Message`].
///
/// Returns `Ok(None)` for unrecognized message types (forward compatibility).
///
/// # Errors
///
/// Returns [`Error::MessageParse`] if the message is malformed or missing
/// required fields.
pub fn parse_message(data: &serde_json::Value) -> Result<Option<Message>> {
    let obj = data.as_object().ok_or_else(|| {
        Error::MessageParse(format!(
            "Invalid message data type (expected object, got {})",
            type_name(data)
        ))
    })?;

    let message_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::MessageParse("Message missing 'type' field".to_string()))?;

    match message_type {
        "user" => parse_user_message(obj),
        "assistant" => parse_assistant_message(obj),
        "system" => parse_system_message(obj),
        "result" => parse_result_message(obj),
        "stream_event" => parse_stream_event(obj),
        _ => Ok(None),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn parse_content_block(block: &Value) -> Result<Option<ContentBlock>> {
    let obj = block
        .as_object()
        .ok_or_else(|| Error::MessageParse("Content block must be object".to_string()))?;
    let block_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::MessageParse("Content block missing 'type' field".to_string()))?;

    match block_type {
        "text" => {
            let text = obj.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::MessageParse("Text block missing 'text' field".to_string())
            })?;
            Ok(Some(ContentBlock::Text(TextBlock {
                text: text.to_string(),
            })))
        }
        "thinking" => {
            let thinking = obj
                .get("thinking")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::MessageParse("Thinking block missing 'thinking' field".to_string())
                })?;
            let signature = obj
                .get("signature")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::MessageParse("Thinking block missing 'signature' field".to_string())
                })?
                .to_string();
            Ok(Some(ContentBlock::Thinking(ThinkingBlock {
                thinking: thinking.to_string(),
                signature,
            })))
        }
        "tool_use" => {
            let id = obj.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::MessageParse("ToolUse block missing 'id' field".to_string())
            })?;
            let name = obj.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::MessageParse("ToolUse block missing 'name' field".to_string())
            })?;
            let input = obj.get("input").cloned().ok_or_else(|| {
                Error::MessageParse("ToolUse block missing 'input' field".to_string())
            })?;
            Ok(Some(ContentBlock::ToolUse(ToolUseBlock {
                id: id.to_string(),
                name: name.to_string(),
                input,
            })))
        }
        "tool_result" => {
            let tool_use_id = obj
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::MessageParse("ToolResult block missing 'tool_use_id' field".to_string())
                })?;
            let content = obj.get("content").cloned();
            let is_error = obj.get("is_error").and_then(|v| v.as_bool());
            Ok(Some(ContentBlock::ToolResult(ToolResultBlock {
                tool_use_id: tool_use_id.to_string(),
                content,
                is_error,
            })))
        }
        _ => {
            tracing::debug!("Skipping unknown content block type: {}", block_type);
            Ok(None)
        }
    }
}

fn parse_user_message(obj: &serde_json::Map<String, Value>) -> Result<Option<Message>> {
    let message = obj
        .get("message")
        .ok_or_else(|| Error::MessageParse("User message missing 'message' field".to_string()))?;

    let content = match message.get("content") {
        Some(Value::String(s)) => UserContent::String(s.clone()),
        Some(Value::Array(arr)) => {
            let mut blocks = Vec::new();
            for block in arr {
                if let Some(b) = parse_content_block(block)? {
                    blocks.push(b);
                }
            }
            UserContent::Blocks(blocks)
        }
        _ => {
            return Err(Error::MessageParse(
                "User message content must be string or array".to_string(),
            ));
        }
    };

    Ok(Some(Message::User(UserMessage {
        content,
        uuid: obj.get("uuid").and_then(|v| v.as_str()).map(String::from),
        parent_tool_use_id: obj
            .get("parent_tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        tool_use_result: obj.get("tool_use_result").cloned(),
    })))
}

fn parse_assistant_message(obj: &serde_json::Map<String, Value>) -> Result<Option<Message>> {
    let message = obj.get("message").ok_or_else(|| {
        Error::MessageParse("Assistant message missing 'message' field".to_string())
    })?;

    let content_arr = message
        .get("content")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::MessageParse("Assistant message missing 'content' array".to_string())
        })?;

    let mut content = Vec::new();
    for block in content_arr {
        if let Some(b) = parse_content_block(block)? {
            content.push(b);
        }
    }

    let model = message
        .get("model")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::MessageParse("Assistant message missing 'model' field".to_string())
        })?;

    Ok(Some(Message::Assistant(AssistantMessage {
        content,
        model: model.to_string(),
        parent_tool_use_id: obj
            .get("parent_tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        error: obj
            .get("error")
            .and_then(|v| v.as_str())
            .map(crate::options::AssistantMessageError::from),
    })))
}

fn parse_system_message(obj: &serde_json::Map<String, Value>) -> Result<Option<Message>> {
    let subtype = obj
        .get("subtype")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::MessageParse("System message missing 'subtype' field".to_string()))?;

    let mut data: serde_json::Map<String, Value> = serde_json::Map::new();
    for (k, v) in obj {
        data.insert(k.clone(), v.clone());
    }

    Ok(Some(Message::System(SystemMessage {
        subtype: subtype.to_string(),
        data: Value::Object(data),
    })))
}

fn parse_result_message(obj: &serde_json::Map<String, Value>) -> Result<Option<Message>> {
    let subtype = obj
        .get("subtype")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::MessageParse("Result message missing 'subtype' field".to_string()))?;
    let duration_ms = obj
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            Error::MessageParse("Result message missing 'duration_ms' field".to_string())
        })?;
    let duration_api_ms = obj
        .get("duration_api_ms")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            Error::MessageParse("Result message missing 'duration_api_ms' field".to_string())
        })?;
    let is_error = obj
        .get("is_error")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            Error::MessageParse("Result message missing 'is_error' field".to_string())
        })?;
    let num_turns = obj
        .get("num_turns")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            Error::MessageParse("Result message missing 'num_turns' field".to_string())
        })? as u32;
    let session_id = obj
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::MessageParse("Result message missing 'session_id' field".to_string())
        })?
        .to_string();

    Ok(Some(Message::Result(ResultMessage {
        subtype: subtype.to_string(),
        duration_ms,
        duration_api_ms,
        is_error,
        num_turns,
        session_id,
        total_cost_usd: obj.get("total_cost_usd").and_then(|v| v.as_f64()),
        usage: obj.get("usage").cloned(),
        result: obj.get("result").and_then(|v| v.as_str()).map(String::from),
        structured_output: obj.get("structured_output").cloned(),
    })))
}

fn parse_stream_event(obj: &serde_json::Map<String, Value>) -> Result<Option<Message>> {
    let uuid = obj
        .get("uuid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::MessageParse("StreamEvent missing 'uuid' field".to_string()))?;
    let session_id = obj
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::MessageParse("StreamEvent missing 'session_id' field".to_string()))?;
    let event = obj.get("event").cloned().unwrap_or(Value::Null);

    Ok(Some(Message::StreamEvent(StreamEvent {
        uuid: uuid.to_string(),
        session_id: session_id.to_string(),
        event,
        parent_tool_use_id: obj
            .get("parent_tool_use_id")
            .and_then(|v| v.as_str())
            .map(String::from),
    })))
}
