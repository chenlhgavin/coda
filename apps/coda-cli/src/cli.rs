//! CLI argument parsing.
//!
//! Defines the command-line interface for CODA using clap.
//! Supports five subcommands: `init`, `plan`, `run`, `list`, and `status`.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::app::App;

/// CODA - Claude Orchestrated Development Agent
#[derive(Parser)]
#[command(name = "coda")]
#[command(
    author,
    version,
    about = "CODA - Claude Orchestrated Development Agent"
)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

/// Available CODA commands.
#[derive(Subcommand)]
pub enum Commands {
    /// Initialize current repository as a CODA project.
    Init,

    /// Interactive feature planning.
    Plan {
        /// URL-safe feature slug (e.g., "add-user-auth").
        feature_slug: String,
    },

    /// Execute feature development.
    Run {
        /// URL-safe feature slug (e.g., "add-user-auth").
        feature_slug: String,

        /// Disable TUI and use plain text output (for CI/pipelines).
        #[arg(long)]
        no_tui: bool,
    },

    /// List all planned features.
    List,

    /// Show detailed status of a specific feature.
    Status {
        /// URL-safe feature slug (e.g., "add-user-auth").
        feature_slug: String,
    },

    /// Remove worktrees whose PR has been merged or closed.
    Clean {
        /// Show what would be removed without actually deleting.
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompt (use with caution).
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

impl Cli {
    /// Executes the parsed CLI command.
    pub async fn run(self) -> Result<()> {
        let app = App::new().await?;

        match self.command {
            Commands::Init => app.init().await,
            Commands::Plan { feature_slug } => app.plan(&feature_slug).await,
            Commands::Run {
                feature_slug,
                no_tui,
            } => app.run(&feature_slug, no_tui).await,
            Commands::List => app.list(),
            Commands::Status { feature_slug } => app.status(&feature_slug),
            Commands::Clean { dry_run, yes } => app.clean(dry_run, yes),
        }
    }
}
