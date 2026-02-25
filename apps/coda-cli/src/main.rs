//! CODA CLI - Command Line Interface
//!
//! Entry point for the CODA development agent. Initializes tracing,
//! parses CLI arguments, and dispatches to the appropriate command handler.

mod app;
mod cli;
pub(crate) mod fmt_utils;
pub(crate) mod init_ui;
mod interactive_config;
mod line_editor;
pub mod markdown;
mod run_ui;
pub(crate) mod tui_widgets;
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
