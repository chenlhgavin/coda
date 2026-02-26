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

use coda_core::{CoreError, InitEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::error::ServerError;
use crate::formatter::{self, InitPhaseDisplay, PhaseDisplayStatus};
use crate::handlers::commands::SlashCommandPayload;
use crate::state::{AppState, TaskCleanupGuard};

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
#[instrument(skip(state, payload), fields(channel = %payload.channel_id))]
pub async fn handle_init(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    force: bool,
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

    // Create cancellation token and store it with the task
    let cancel_token = CancellationToken::new();

    // Spawn background task
    let state_clone = Arc::clone(&state);
    let channel_clone = channel.clone();
    let repo_path_clone = repo_path.clone();
    let cancel_token_clone = cancel_token.clone();

    let handle = tokio::spawn(async move {
        run_init_task(
            state_clone,
            engine,
            channel_clone,
            message_ts,
            repo_path_clone,
            force,
            cancel_token_clone,
        )
        .await;
    });

    state.running_tasks().insert(task_key, handle, cancel_token);

    Ok(())
}

/// Runs the init pipeline, consuming events and updating the Slack message.
async fn run_init_task(
    state: Arc<AppState>,
    engine: coda_core::Engine,
    channel: String,
    message_ts: String,
    repo_path: std::path::PathBuf,
    force: bool,
    cancel_token: CancellationToken,
) {
    // Guard guarantees running-task + repo-lock cleanup even on panic/abort.
    let task_key = format!("init:{}", repo_path.display());
    let _guard =
        TaskCleanupGuard::new(Arc::clone(&state), task_key).with_repo_unlock(repo_path.clone());

    let (tx, rx) = mpsc::unbounded_channel();
    let slack = state.slack().clone();

    // Drive event consumption in parallel with engine execution
    let event_handle = tokio::spawn(consume_init_events(
        slack,
        channel.clone(),
        message_ts.clone(),
        rx,
    ));

    debug!("Starting engine.init()");
    let start = Instant::now();
    let result = engine.init(false, force, Some(tx), cancel_token).await;
    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        success = result.is_ok(),
        "engine.init() completed"
    );

    // Wait for event consumer to finish and retrieve accumulated phase state.
    // Bounded timeout prevents hanging if a leaked sender clone keeps the channel open.
    let display_phases = match tokio::time::timeout(Duration::from_secs(10), event_handle).await {
        Ok(Ok(phases)) => phases,
        Ok(Err(e)) => {
            warn!(error = %e, "Init event consumer task panicked");
            Vec::new()
        }
        Err(_) => {
            warn!("Init event consumer did not finish within 10s, proceeding");
            Vec::new()
        }
    };

    // Post final summary, reusing the phase data accumulated by the consumer
    let final_blocks = match result {
        Ok(()) => {
            info!(channel, "Init completed successfully");
            formatter::init_success(&display_phases)
        }
        Err(CoreError::Cancelled) => {
            info!(channel, "Init was cancelled");
            formatter::init_cancelled(&display_phases)
        }
        Err(e) => {
            warn!(channel, error = %e, "Init failed");
            formatter::init_failure(&display_phases, &e.to_string())
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(&channel, &message_ts, final_blocks)
        .await
    {
        warn!(error = %e, channel, "Failed to post init final update");
    }

    // Cleanup is handled by `_guard` (TaskCleanupGuard) on drop.
}

/// Consumes [`InitEvent`]s from the channel and debounces Slack updates.
///
/// Phase transitions trigger immediate updates. High-frequency events
/// are batched, with updates at most every [`UPDATE_DEBOUNCE`] interval.
///
/// Returns the accumulated phase display state so the caller can build
/// a final summary message with complete duration/cost data.
async fn consume_init_events(
    slack: crate::slack_client::SlackClient,
    channel: String,
    message_ts: String,
    mut rx: mpsc::UnboundedReceiver<InitEvent>,
) -> Vec<InitPhaseDisplay> {
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
                        started_at: None,
                    })
                    .collect();
                true
            }
            InitEvent::PhaseStarting {
                ref name, index, ..
            } => {
                let now = Instant::now();
                if let Some(p) = display_phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Running;
                    p.started_at = Some(now);
                } else {
                    display_phases.push(InitPhaseDisplay {
                        name: name.clone(),
                        status: PhaseDisplayStatus::Running,
                        duration: None,
                        cost_usd: None,
                        started_at: Some(now),
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
                    p.started_at = None;
                }
                true
            }
            InitEvent::PhaseFailed { index, .. } => {
                if let Some(p) = display_phases.get_mut(index) {
                    // Record elapsed time so the failure message shows duration
                    if let Some(started) = p.started_at {
                        p.duration = Some(started.elapsed());
                    }
                    p.status = PhaseDisplayStatus::Failed;
                    p.started_at = None;
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

    display_phases
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
