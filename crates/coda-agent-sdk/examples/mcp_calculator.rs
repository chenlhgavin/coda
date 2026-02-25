//! MCP Calculator example - corresponds to Python examples/mcp_calculator.py
//!
//! Run with: cargo run --example mcp_calculator
//!
//! This example demonstrates creating SDK MCP tools and routing them in-process.

use coda_agent_sdk::{
    AgentOptions, AgentSdkClient, McpServerConfig, McpServersConfig, create_sdk_mcp_server,
    sdk_mcp_tool,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("MCP Calculator Example");
    println!("======================\n");

    // Define calculator tools
    let add_tool = sdk_mcp_tool(
        "add",
        "Add two numbers together",
        json!({
            "type": "object",
            "properties": {
                "a": {"type": "number", "description": "First number"},
                "b": {"type": "number", "description": "Second number"}
            },
            "required": ["a", "b"]
        }),
        |args| {
            Box::pin(async move {
                let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
                Ok(json!({"content": [{"type": "text", "text": format!("{}", a + b)}]}))
            })
        },
    );

    let subtract_tool = sdk_mcp_tool(
        "subtract",
        "Subtract two numbers",
        json!({
            "type": "object",
            "properties": {
                "a": {"type": "number", "description": "First number"},
                "b": {"type": "number", "description": "Second number to subtract"}
            },
            "required": ["a", "b"]
        }),
        |args| {
            Box::pin(async move {
                let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
                Ok(json!({"content": [{"type": "text", "text": format!("{}", a - b)}]}))
            })
        },
    );

    let multiply_tool = sdk_mcp_tool(
        "multiply",
        "Multiply two numbers",
        json!({
            "type": "object",
            "properties": {
                "a": {"type": "number", "description": "First number"},
                "b": {"type": "number", "description": "Second number"}
            },
            "required": ["a", "b"]
        }),
        |args| {
            Box::pin(async move {
                let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
                Ok(json!({"content": [{"type": "text", "text": format!("{}", a * b)}]}))
            })
        },
    );

    // Create SDK MCP server
    let calculator = create_sdk_mcp_server(
        "calculator",
        "1.0.0",
        vec![add_tool, subtract_tool, multiply_tool],
    );

    // Configure with MCP server and allowed tools
    let mut servers = HashMap::new();
    servers.insert("calc".to_string(), McpServerConfig::Sdk(calculator));

    let options = AgentOptions::builder()
        .mcp_servers(McpServersConfig::Dict(servers))
        .allowed_tools([
            "mcp__calc__add",
            "mcp__calc__subtract",
            "mcp__calc__multiply",
        ])
        .build();

    let mut client = AgentSdkClient::new(Some(options), None);
    if let Err(e) = client.connect(None).await {
        println!("Note: connect() failed: {}", e);
        return Ok(());
    }

    println!("Asking Claude to calculate 15 + 27...");
    if let Err(e) = client
        .query("Calculate 15 + 27 using the calculator tool", "default")
        .await
    {
        println!("Note: query() failed: {}", e);
    } else {
        use futures::StreamExt;
        let mut stream = client.receive_response();
        while let Some(msg_result) = stream.next().await {
            match msg_result {
                Ok(coda_agent_sdk::Message::Assistant(ref a)) => {
                    for block in &a.content {
                        if let coda_agent_sdk::ContentBlock::Text(t) = block {
                            println!("Claude: {}", t.text);
                        }
                    }
                }
                Ok(coda_agent_sdk::Message::Result(_)) => {
                    println!("Done.");
                    break;
                }
                Err(e) => {
                    println!("Error: {}", e);
                    break;
                }
                _ => {}
            }
        }
    }

    let _ = client.disconnect().await;
    Ok(())
}
