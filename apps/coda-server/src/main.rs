//! CODA Slack Server — Socket Mode client for triggering CODA workflows from Slack.
//!
//! Connects to Slack via outbound WebSocket (Socket Mode), receives slash
//! commands, and delegates to `coda-core` for feature development operations.
//! Designed for personal use on a local machine.

mod commands;
mod config;
mod dispatch;
mod error;
mod formatter;
mod handlers;
mod slack_client;
mod socket;
mod state;

use std::sync::Arc;

use anyhow::Context;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with env filter
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("coda_server=info".parse()?),
        )
        .init();

    // Load configuration
    let config_path = config::default_config_path().context("Failed to determine config path")?;
    let server_config = config::ServerConfig::load(&config_path)
        .with_context(|| format!("Failed to load config from {}", config_path.display()))?;

    info!("Configuration loaded successfully");

    // Build application state
    let slack = slack_client::SlackClient::new(server_config.slack.bot_token.clone());
    let bindings = state::BindingStore::new(config_path, server_config.bindings.clone());
    let app_state = Arc::new(state::AppState::new(slack.clone(), bindings));

    info!(
        binding_count = server_config.bindings.len(),
        "Application state initialized"
    );

    // Setup graceful shutdown on SIGINT/SIGTERM
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("register SIGTERM handler");

        #[cfg(unix)]
        tokio::select! {
            _ = ctrl_c => {
                info!("Received SIGINT, shutting down...");
            }
            _ = sigterm.recv() => {
                info!("Received SIGTERM, shutting down...");
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c.await.ok();
            info!("Received SIGINT, shutting down...");
        }

        let _ = shutdown_tx.send(true);
    });

    // Connect and run Socket Mode event loop
    let socket = socket::SocketClient::new(server_config.slack.app_token.clone(), slack);
    let state_for_handler = Arc::clone(&app_state);

    info!("Starting Socket Mode connection...");
    socket
        .run(
            move |envelope| {
                let state = Arc::clone(&state_for_handler);
                async move {
                    dispatch::dispatch(state, envelope).await;
                }
            },
            shutdown_rx,
        )
        .await
        .context("Socket Mode event loop failed")?;

    info!("Server shut down cleanly");
    Ok(())
}
