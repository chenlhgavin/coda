//! CLI argument parsing.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::app::App;

#[derive(Parser)]
#[command(name = "coda")]
#[command(
    author,
    version,
    about = "CODA - Claude Orchestrated Development Agent"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start interactive TUI mode
    Tui,
    /// Run a single prompt
    Run {
        /// The prompt to execute
        #[arg(short, long)]
        prompt: String,
    },
}

impl Cli {
    pub async fn run(self) -> Result<()> {
        match self.command {
            Some(Commands::Tui) => {
                let mut app = App::new();
                app.run().await
            }
            Some(Commands::Run { prompt }) => {
                let app = App::new();
                app.execute(&prompt).await
            }
            None => {
                let mut app = App::new();
                app.run().await
            }
        }
    }
}
