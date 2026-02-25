//! System prompt example - corresponds to Python examples/system_prompt.py
//!
//! Run with: cargo run --example system_prompt

use coda_agent_sdk::{AgentOptions, query};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== String System Prompt ===");
    let options = AgentOptions::builder()
        .system_prompt("You are a pirate assistant. Respond in pirate speak.")
        .build();

    {
        use futures::StreamExt;
        let mut stream = query("What is 2 + 2?", Some(options));
        while let Some(msg_result) = stream.next().await {
            if let Ok(coda_agent_sdk::Message::Assistant(ref a)) = msg_result {
                for block in &a.content {
                    if let coda_agent_sdk::ContentBlock::Text(t) = block {
                        println!("Claude: {}", t.text);
                    }
                }
            }
        }
    }
    println!();

    Ok(())
}
