//! Streaming mode examples - corresponds to Python examples/streaming_mode.py
//!
//! Run with: cargo run --example streaming_mode [basic_streaming|multi_turn|all]

use coda_agent_sdk::{AgentSdkClient, Message};
use std::env;

#[allow(dead_code)]
fn display_message(msg: &Message) {
    match msg {
        Message::Assistant(a) => {
            for block in &a.content {
                if let coda_agent_sdk::ContentBlock::Text(t) = block {
                    println!("Claude: {}", t.text);
                }
            }
        }
        Message::Result(_) => println!("Result ended"),
        _ => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let example = args.get(1).map(|s| s.as_str()).unwrap_or("");

    if example.is_empty() {
        println!("Usage: cargo run --example streaming_mode <example_name>");
        println!("\nAvailable examples:");
        println!("  basic_streaming  - Basic streaming with context manager");
        println!("  multi_turn       - Multi-turn conversation");
        println!("  all              - Run all examples");
        return Ok(());
    }

    println!("=== Basic Streaming Example ===");
    let mut client = AgentSdkClient::new(None, None);
    if let Err(e) = client.connect(None).await {
        println!("Note: connect() failed: {}", e);
        return Ok(());
    }

    println!("User: What is 2+2?");
    if let Err(e) = client.query("What is 2+2?", "default").await {
        println!("Note: query() failed: {}", e);
    }

    let _ = client.disconnect().await;
    println!();

    Ok(())
}
