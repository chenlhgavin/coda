//! Tool permission callback example - corresponds to Python examples/tool_permission_callback.py
//!
//! Run with: cargo run --example tool_permission_callback
//!
//! NOTE: can_use_tool callback is not yet implemented.

use coda_agent_sdk::AgentOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("============================================================");
    println!("Tool Permission Callback Example");
    println!("============================================================");
    println!("\nThis example demonstrates how to:");
    println!("1. Allow/deny tools based on type");
    println!("2. Modify tool inputs for safety");
    println!("3. Log tool usage");
    println!("4. Prompt for unknown tools");
    println!("============================================================");
    println!("\nNOTE: can_use_tool callback is not yet implemented.");
    println!("When implemented, the usage would be:\n");
    println!("  let options = AgentOptions::builder()");
    println!("      .can_use_tool(my_permission_callback)");
    println!("      .permission_mode(\"default\")");
    println!("      .cwd(\".\")");
    println!("      .build();");
    println!();

    let _options = AgentOptions::builder()
        .permission_mode("default")
        .cwd(".")
        .build();

    Ok(())
}
