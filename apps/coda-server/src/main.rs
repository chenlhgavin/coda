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
pub mod session;
mod slack_client;
mod socket;
mod state;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tracing::{info, warn};

/// Duration before session expiry at which a warning is sent.
const WARN_BEFORE_EXPIRY: Duration = Duration::from_secs(300);

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

    // Ensure workspace directory exists if configured
    let workspace = server_config.workspace.as_ref().map(PathBuf::from);
    if let Some(ref ws) = workspace {
        std::fs::create_dir_all(ws)
            .with_context(|| format!("Failed to create workspace directory: {}", ws.display()))?;
        info!(workspace = %ws.display(), "Workspace directory ready");
    }

    // Build application state
    let slack = slack_client::SlackClient::new(server_config.slack.bot_token.clone())?;
    let bindings = state::BindingStore::new(config_path, server_config.bindings.clone());
    let running_tasks = state::RunningTasks::new();
    let sessions = session::SessionManager::new();
    let repo_locks = state::RepoLocks::new();
    let app_state = Arc::new(state::AppState::new(
        slack.clone(),
        bindings,
        running_tasks,
        sessions,
        workspace,
        repo_locks,
    ));

    info!(
        binding_count = server_config.bindings.len(),
        "Application state initialized"
    );

    // Setup graceful shutdown on SIGINT/SIGTERM
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        // First signal: graceful shutdown
        wait_for_shutdown_signal().await;
        info!("Received shutdown signal, shutting down gracefully...");
        let _ = shutdown_tx.send(true);

        // Second signal: force exit
        wait_for_shutdown_signal().await;
        warn!("Received second signal, forcing exit");
        std::process::exit(1);
    });

    // Start background session reaper
    let reaper_interval = Duration::from_secs(server_config.reaper_interval_secs);
    let max_session_idle = Duration::from_secs(server_config.session_idle_timeout_secs);
    info!(
        session_idle_timeout_secs = server_config.session_idle_timeout_secs,
        reaper_interval_secs = server_config.reaper_interval_secs,
        "Session reaper configured"
    );

    let reaper_state = Arc::clone(&app_state);
    let mut reaper_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(reaper_interval) => {
                    // Send pre-timeout warnings for sessions approaching expiry
                    let warnings = reaper_state
                        .sessions()
                        .warn_approaching(max_session_idle, WARN_BEFORE_EXPIRY);
                    for (channel, thread_ts, remaining_secs) in warnings {
                        let mins = remaining_secs / 60;
                        let msg = format!(
                            ":warning: This planning session will expire in ~{mins} minute(s) \
                             due to inactivity. Reply to keep it alive.",
                        );
                        if let Err(e) = reaper_state
                            .slack()
                            .post_thread_reply(&channel, &thread_ts, &msg)
                            .await
                        {
                            warn!(error = %e, channel, thread_ts, "Failed to send session warning");
                        }
                    }

                    // Reap idle sessions and notify users
                    let reaped = reaper_state
                        .sessions()
                        .reap_idle(max_session_idle)
                        .await;
                    for (channel, thread_ts) in &reaped {
                        let timeout_mins = max_session_idle.as_secs() / 60;
                        let msg = format!(
                            ":no_entry_sign: Planning session expired after {timeout_mins} minutes \
                             of inactivity. Use `/coda plan` to start a new session.",
                        );
                        if let Err(e) = reaper_state
                            .slack()
                            .post_thread_reply(channel, thread_ts, &msg)
                            .await
                        {
                            warn!(error = %e, channel, thread_ts, "Failed to send expiry notification");
                        }
                    }
                    if !reaped.is_empty() {
                        info!(reaped = reaped.len(), "Session reaper completed");
                    }
                }
                _ = reaper_shutdown.changed() => {
                    info!("Session reaper shutting down");
                    break;
                }
            }
        }
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

    // Clean up active plan sessions before exiting
    let session_count = app_state.sessions().len();
    if session_count > 0 {
        info!(session_count, "Disconnecting active plan sessions");
        match tokio::time::timeout(
            Duration::from_secs(5),
            app_state.sessions().disconnect_all(),
        )
        .await
        {
            Ok(disconnected) => {
                info!(disconnected, "Plan sessions disconnected");
            }
            Err(_) => {
                warn!("Session cleanup timed out after 5s");
            }
        }
    }

    info!("Server shut down cleanly");
    Ok(())
}

/// Waits for either a SIGINT (Ctrl+C) or SIGTERM signal.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("register SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}
