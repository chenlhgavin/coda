//! `/coda init` command handler with live progress updates.
//!
//! Spawns a background tokio task that runs `Engine::init()`, subscribes
//! to [`InitEvent`]s via an mpsc channel, and debounces Slack message
//! updates to respect rate limits (~1 `chat.update` per second).
//!
//! Phase transitions (`PhaseStarting`, `PhaseCompleted`, `PhaseFailed`)
//! trigger an immediate `chat.update`. High-frequency events
//! (`StreamText`) are batched with updates at most every 3 seconds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coda_core::InitEvent;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::error::ServerError;
use crate::formatter::{self, InitPhaseDisplay, PhaseDisplayStatus};
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

use super::resolve_engine;

/// Minimum interval between debounced `chat.update` calls (3 seconds).
const UPDATE_DEBOUNCE: Duration = Duration::from_secs(3);

/// Handles `/coda init`.
///
/// Resolves the channel binding, checks for duplicate tasks, posts an
/// initial progress message, then spawns a background task that drives
/// the init pipeline and updates the Slack message in real time.
///
/// # Errors
///
/// Returns `ServerError` if the initial Slack message post fails or
/// the engine cannot be created. Errors during the background init
/// are reported by updating the Slack message.
pub async fn handle_init(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
) -> Result<(), ServerError> {
    let channel = payload.channel_id.clone();

    let Some((repo_path, engine)) = resolve_engine(state.as_ref(), &channel).await? else {
        return Ok(());
    };

    // Duplicate prevention — keyed by repo path, not channel
    let task_key = format!("init:{}", repo_path.display());
    if state.running_tasks().is_running(&task_key) {
        let blocks = formatter::error("An init is already running for this repository.");
        state.slack().post_message(&channel, blocks).await?;
        return Ok(());
    }

    // Acquire repo lock
    if let Err(holder) = state.repo_locks().try_lock(&repo_path, "init") {
        let blocks = formatter::error(&format!(
            "Repository is busy (`{holder}`). Try again later."
        ));
        state.slack().post_message(&channel, blocks).await?;
        return Ok(());
    }

    info!(channel, "Starting init command");

    // Post initial progress message
    let initial_blocks = formatter::init_progress(&[]);
    let resp = state.slack().post_message(&channel, initial_blocks).await?;
    let message_ts = resp.ts;

    // Spawn background task
    let state_clone = Arc::clone(&state);
    let channel_clone = channel.clone();
    let repo_path_clone = repo_path.clone();

    let handle = tokio::spawn(async move {
        run_init_task(
            state_clone,
            engine,
            channel_clone,
            message_ts,
            repo_path_clone,
        )
        .await;
    });

    state.running_tasks().insert(task_key, handle);

    Ok(())
}

/// Runs the init pipeline, consuming events and updating the Slack message.
async fn run_init_task(
    state: Arc<AppState>,
    engine: coda_core::Engine,
    channel: String,
    message_ts: String,
    repo_path: std::path::PathBuf,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let slack = state.slack().clone();

    // Drive event consumption in parallel with engine execution
    let event_handle = tokio::spawn(consume_init_events(
        slack,
        channel.clone(),
        message_ts.clone(),
        rx,
    ));

    let result = engine.init(false, Some(tx)).await;

    // Wait for event consumer to finish processing remaining events
    if let Err(e) = event_handle.await {
        warn!(error = %e, "Init event consumer task panicked");
    }

    // Post final summary
    let final_blocks = match result {
        Ok(()) => {
            info!(channel, "Init completed successfully");
            formatter::init_progress(&[
                InitPhaseDisplay {
                    name: "analyze-repo".to_string(),
                    status: PhaseDisplayStatus::Completed,
                    duration: None,
                    cost_usd: None,
                },
                InitPhaseDisplay {
                    name: "setup-project".to_string(),
                    status: PhaseDisplayStatus::Completed,
                    duration: None,
                    cost_usd: None,
                },
            ])
        }
        Err(e) => {
            warn!(channel, error = %e, "Init failed");
            formatter::error(&format!("Init failed: {e}"))
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(&channel, &message_ts, final_blocks)
        .await
    {
        warn!(error = %e, channel, "Failed to post init final update");
    }

    // Clean up running task entry and release repo lock
    let task_key = format!("init:{}", repo_path.display());
    state.running_tasks().remove(&task_key);
    state.repo_locks().unlock(&repo_path);
}

/// Consumes [`InitEvent`]s from the channel and debounces Slack updates.
///
/// Phase transitions trigger immediate updates. High-frequency events
/// are batched, with updates at most every [`UPDATE_DEBOUNCE`] interval.
async fn consume_init_events(
    slack: crate::slack_client::SlackClient,
    channel: String,
    message_ts: String,
    mut rx: mpsc::UnboundedReceiver<InitEvent>,
) {
    let mut display_phases: Vec<InitPhaseDisplay> = Vec::new();
    let mut last_update = Instant::now() - UPDATE_DEBOUNCE;
    let mut pending_update = false;

    while let Some(event) = rx.recv().await {
        let immediate = match event {
            InitEvent::InitStarting { ref phases } => {
                display_phases = phases
                    .iter()
                    .map(|name| InitPhaseDisplay {
                        name: name.clone(),
                        status: PhaseDisplayStatus::Pending,
                        duration: None,
                        cost_usd: None,
                    })
                    .collect();
                true
            }
            InitEvent::PhaseStarting {
                ref name, index, ..
            } => {
                if let Some(p) = display_phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Running;
                } else {
                    display_phases.push(InitPhaseDisplay {
                        name: name.clone(),
                        status: PhaseDisplayStatus::Running,
                        duration: None,
                        cost_usd: None,
                    });
                }
                true
            }
            InitEvent::PhaseCompleted {
                index,
                duration,
                cost_usd,
                ..
            } => {
                if let Some(p) = display_phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Completed;
                    p.duration = Some(duration);
                    p.cost_usd = Some(cost_usd);
                }
                true
            }
            InitEvent::PhaseFailed { index, .. } => {
                if let Some(p) = display_phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Failed;
                }
                true
            }
            InitEvent::StreamText { .. } => false,
            InitEvent::InitFinished { .. } => true,
            _ => false,
        };

        if immediate {
            send_update(&slack, &channel, &message_ts, &display_phases).await;
            last_update = Instant::now();
            pending_update = false;
        } else {
            pending_update = true;
            if last_update.elapsed() >= UPDATE_DEBOUNCE {
                send_update(&slack, &channel, &message_ts, &display_phases).await;
                last_update = Instant::now();
                pending_update = false;
            }
        }
    }

    // Flush any remaining pending update
    if pending_update {
        send_update(&slack, &channel, &message_ts, &display_phases).await;
    }
}

/// Sends a single `chat.update` with the current phase display state.
async fn send_update(
    slack: &crate::slack_client::SlackClient,
    channel: &str,
    ts: &str,
    phases: &[InitPhaseDisplay],
) {
    let blocks = formatter::init_progress(phases);
    if let Err(e) = slack.update_message(channel, ts, blocks).await {
        warn!(error = %e, channel, "Failed to update init progress message");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_format_init_task_key_by_repo_path() {
        let key = format!("init:{}", "/repos/myproject");
        assert_eq!(key, "init:/repos/myproject");
    }

    #[test]
    fn test_should_define_debounce_interval() {
        assert_eq!(UPDATE_DEBOUNCE, Duration::from_secs(3));
    }
}
