//! CODA CLI - Command Line Interface
//!
//! Entry point for the CODA development agent. Initializes tracing,
//! parses CLI arguments, and dispatches to the appropriate command handler.

mod app;
mod cli;
mod line_editor;
mod run_ui;
#[allow(dead_code)]
mod ui;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    cli.run().await
}
