//! Integration tests - corresponds to Python test_integration.py
//! These tests use mocked transport. E2E tests with real API are in e2e-tests/.

use coda_agent_sdk::{AgentOptions, parse_message, types::*};
use serde_json::json;

#[test]
fn test_simple_query_response_types() {
    // Verify we can parse the response format that integration would produce
    let assistant_data = json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": "2 + 2 equals 4"}],
            "model": "claude-opus-4-1-20250805"
        }
    });
    let result_msg = parse_message(&assistant_data).unwrap();
    let msg = result_msg.expect("should parse");
    match &msg {
        Message::Assistant(a) => {
            assert_eq!(a.content.len(), 1);
            match &a.content[0] {
                ContentBlock::Text(t) => assert_eq!(t.text, "2 + 2 equals 4"),
                _ => panic!("expected TextBlock"),
            }
        }
        _ => panic!("expected AssistantMessage"),
    }

    let result_data = json!({
        "type": "result",
        "subtype": "success",
        "duration_ms": 1000,
        "duration_api_ms": 800,
        "is_error": false,
        "num_turns": 1,
        "session_id": "test-session",
        "total_cost_usd": 0.001
    });
    let result_msg = parse_message(&result_data).unwrap();
    let msg = result_msg.expect("should parse");
    match &msg {
        Message::Result(r) => {
            assert_eq!(r.total_cost_usd, Some(0.001));
            assert_eq!(r.session_id, "test-session");
        }
        _ => panic!("expected ResultMessage"),
    }
}

#[test]
fn test_options_continuation() {
    let options = AgentOptions::builder()
        .continue_conversation(true)
        .resume("session-123")
        .build();
    assert!(options.continue_conversation);
    assert_eq!(options.resume.as_deref(), Some("session-123"));
}

#[test]
fn test_prompt_from_string() {
    let prompt: Prompt = "Hello".into();
    assert!(matches!(prompt, Prompt::Text(ref s) if s == "Hello"));

    let prompt: Prompt = String::from("World").into();
    assert!(matches!(prompt, Prompt::Text(ref s) if s == "World"));
}

#[test]
fn test_prompt_debug() {
    let prompt: Prompt = "test".into();
    let debug = format!("{:?}", prompt);
    assert!(debug.contains("Text"));
    assert!(debug.contains("test"));
}

#[test]
fn test_sdk_mcp_tool_creation() {
    let tool = coda_agent_sdk::sdk_mcp_tool(
        "echo",
        "Echo the input",
        json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        |args| {
            Box::pin(async move {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("empty");
                Ok(json!({"content": [{"type": "text", "text": text}]}))
            })
        },
    );
    assert_eq!(tool.name, "echo");
    assert_eq!(tool.description, "Echo the input");
}

#[test]
fn test_create_sdk_mcp_server() {
    let tool =
        coda_agent_sdk::sdk_mcp_tool("add", "Add numbers", json!({"type": "object"}), |_| {
            Box::pin(async { Ok(json!({"content": []})) })
        });
    let server = coda_agent_sdk::create_sdk_mcp_server("calc", "2.0.0", vec![tool]);
    assert_eq!(server.name, "calc");
    assert_eq!(server.version, "2.0.0");
    assert_eq!(server.tools.len(), 1);
    assert_eq!(server.tools[0].name, "add");
}

#[test]
fn test_not_connected_error_on_receive() {
    let client = coda_agent_sdk::AgentSdkClient::new(None, None);

    // receive_messages should yield NotConnected error
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use futures::StreamExt;
        let mut stream = client.receive_messages();
        let first = stream.next().await;
        assert!(first.is_some());
        let err = first.unwrap().unwrap_err();
        assert!(
            matches!(err, coda_agent_sdk::Error::NotConnected),
            "Expected NotConnected, got: {:?}",
            err
        );
    });
}

#[test]
fn test_not_connected_error_on_server_info() {
    let client = coda_agent_sdk::AgentSdkClient::new(None, None);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let result = client.get_server_info().await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            coda_agent_sdk::Error::NotConnected
        ));
    });
}
