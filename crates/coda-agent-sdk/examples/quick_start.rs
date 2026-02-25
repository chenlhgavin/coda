//! Quick start example - corresponds to Python examples/quick_start.py
//!
//! Run with: cargo run --example quick_start

use coda_agent_sdk::{AgentOptions, Message, query};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Basic Example ===");

    {
        use futures::StreamExt;
        let mut stream = query("What is 2 + 2?", None);
        while let Some(msg_result) = stream.next().await {
            if let Ok(Message::Assistant(ref a)) = msg_result {
                for block in &a.content {
                    if let coda_agent_sdk::ContentBlock::Text(t) = block {
                        println!("Claude: {}", t.text);
                    }
                }
            }
        }
    }
    println!();

    println!("=== With Options Example ===");
    let options = AgentOptions::builder()
        .system_prompt("You are a helpful assistant that explains things simply.")
        .max_turns(1)
        .build();

    {
        use futures::StreamExt;
        let mut stream = query("Explain what Rust is in one sentence.", Some(options));
        while let Some(msg_result) = stream.next().await {
            if let Ok(Message::Assistant(ref a)) = msg_result {
                for block in &a.content {
                    if let coda_agent_sdk::ContentBlock::Text(t) = block {
                        println!("Claude: {}", t.text);
                    }
                }
            }
        }
    }
    println!();

    println!("=== With Tools Example ===");
    let options = AgentOptions::builder()
        .allowed_tools(["Read", "Write"])
        .system_prompt("You are a helpful file assistant.")
        .build();

    {
        use futures::StreamExt;
        let mut stream = query(
            "Create a file called hello.txt with 'Hello, World!' in it",
            Some(options),
        );
        while let Some(msg_result) = stream.next().await {
            if let Ok(Message::Assistant(ref a)) = msg_result {
                for block in &a.content {
                    if let coda_agent_sdk::ContentBlock::Text(t) = block {
                        println!("Claude: {}", t.text);
                    }
                }
            }
            if let Ok(Message::Result(ref r)) = msg_result
                && let Some(cost) = r.total_cost_usd
            {
                println!("\nCost: ${:.4}", cost);
            }
        }
    }
    println!();

    Ok(())
}
