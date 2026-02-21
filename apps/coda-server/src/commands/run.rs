//! `/coda run <slug>` command handler with live progress updates.
//!
//! Spawns a background tokio task that runs `Engine::run()`, subscribes
//! to [`RunEvent`]s via an mpsc channel, and debounces Slack message
//! updates to respect rate limits (~1 `chat.update` per second).
//!
//! Phase transitions (`PhaseStarting`, `PhaseCompleted`, `PhaseFailed`)
//! and sub-events (`ReviewRound`, `VerifyAttempt`, `CreatingPr`,
//! `PrCreated`) trigger an immediate `chat.update`. High-frequency events
//! (`AgentTextDelta`, `TurnCompleted`, `ToolActivity`) are batched with
//! updates at most every 3 seconds.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coda_core::RunEvent;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::error::ServerError;
use crate::formatter::{self, PhaseDisplayStatus, RunPhaseDisplay};
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

use super::resolve_engine;

/// Minimum interval between debounced `chat.update` calls (3 seconds).
const UPDATE_DEBOUNCE: Duration = Duration::from_secs(3);

/// Handles `/coda run <feature_slug>`.
///
/// Resolves the channel binding, checks for duplicate runs of the same
/// slug, posts an initial progress message, then spawns a background
/// task that drives the run pipeline and updates the Slack message in
/// real time.
///
/// # Errors
///
/// Returns `ServerError` if the initial Slack message post fails or
/// the engine cannot be created. Errors during the background run
/// are reported by updating the Slack message.
pub async fn handle_run(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    feature_slug: &str,
) -> Result<(), ServerError> {
    let channel = payload.channel_id.clone();
    let slug = feature_slug.to_string();

    let Some((repo_path, engine)) = resolve_engine(state.as_ref(), &channel).await? else {
        return Ok(());
    };

    // Duplicate prevention — keyed by repo path + slug
    let task_key = format!("run:{}:{slug}", repo_path.display());
    if state.running_tasks().is_running(&task_key) {
        let blocks = formatter::error(&format!("A run for `{slug}` is already in progress."));
        state.slack().post_message(&channel, blocks).await?;
        return Ok(());
    }

    info!(channel, feature_slug = slug, "Starting run command");

    // Post initial progress message
    let initial_blocks = formatter::run_progress(&slug, &[]);
    let resp = state.slack().post_message(&channel, initial_blocks).await?;
    let message_ts = resp.ts;

    // Spawn background task
    let state_clone = Arc::clone(&state);
    let channel_clone = channel.clone();
    let slug_clone = slug.clone();
    let repo_path_clone = repo_path;

    let handle = tokio::spawn(async move {
        run_feature_task(
            state_clone,
            engine,
            channel_clone,
            message_ts,
            slug_clone,
            repo_path_clone,
        )
        .await;
    });

    state.running_tasks().insert(task_key, handle);

    Ok(())
}

/// Runs the feature development pipeline, consuming events and updating Slack.
async fn run_feature_task(
    state: Arc<AppState>,
    engine: coda_core::Engine,
    channel: String,
    message_ts: String,
    slug: String,
    repo_path: std::path::PathBuf,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let slack = state.slack().clone();

    // Drive event consumption in parallel with engine execution
    let slug_for_events = slug.clone();
    let event_handle = tokio::spawn(consume_run_events(
        slack,
        channel.clone(),
        message_ts.clone(),
        slug_for_events,
        rx,
    ));

    let result = engine.run(&slug, Some(tx)).await;

    // Wait for event consumer to finish processing remaining events
    if let Err(e) = event_handle.await {
        warn!(error = %e, "Run event consumer task panicked");
    }

    // Post final summary
    let final_blocks = match result {
        Ok(results) => {
            info!(channel, slug, "Run completed successfully");
            build_run_summary(&slug, &results)
        }
        Err(e) => {
            warn!(channel, slug, error = %e, "Run failed");
            formatter::error(&format!("Run `{slug}` failed: {e}"))
        }
    };

    if let Err(e) = state
        .slack()
        .update_message(&channel, &message_ts, final_blocks)
        .await
    {
        warn!(error = %e, channel, "Failed to post run final update");
    }

    // Clean up running task entry
    let task_key = format!("run:{}:{slug}", repo_path.display());
    state.running_tasks().remove(&task_key);
}

/// Extracts a display name from a [`Task`](coda_core::Task) variant.
fn task_display_name(task: &coda_core::Task) -> String {
    match task {
        coda_core::Task::Init => "init".to_string(),
        coda_core::Task::Plan { feature_slug } => format!("plan:{feature_slug}"),
        coda_core::Task::DevPhase { name, .. } => name.clone(),
        coda_core::Task::Review { .. } => "review".to_string(),
        coda_core::Task::Verify { .. } => "verify".to_string(),
        coda_core::Task::CreatePr { .. } => "create-pr".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Builds a final summary message from the run results.
fn build_run_summary(slug: &str, results: &[coda_core::TaskResult]) -> Vec<serde_json::Value> {
    let phases: Vec<RunPhaseDisplay> = results
        .iter()
        .map(|r| {
            let status = match r.status {
                coda_core::TaskStatus::Completed => PhaseDisplayStatus::Completed,
                coda_core::TaskStatus::Failed { .. } => PhaseDisplayStatus::Failed,
                _ => PhaseDisplayStatus::Pending,
            };
            RunPhaseDisplay {
                name: task_display_name(&r.task),
                status,
                duration: Some(r.duration),
                turns: Some(r.turns),
                cost_usd: Some(r.cost_usd),
            }
        })
        .collect();

    formatter::run_progress(slug, &phases)
}

/// Mutable tracking state for the run event consumer.
struct RunTracker {
    /// Per-phase display state.
    phases: Vec<RunPhaseDisplay>,
    /// Feature slug for rendering.
    slug: String,
    /// PR URL if created.
    pr_url: Option<String>,
}

impl RunTracker {
    fn new(slug: String) -> Self {
        Self {
            phases: Vec::new(),
            slug,
            pr_url: None,
        }
    }

    fn to_blocks(&self) -> Vec<serde_json::Value> {
        let mut blocks = formatter::run_progress(&self.slug, &self.phases);
        if let Some(ref url) = self.pr_url {
            blocks.push(serde_json::json!({
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(":link: *PR:* <{url}>")
                }
            }));
        }
        blocks
    }
}

/// Consumes [`RunEvent`]s from the channel and debounces Slack updates.
///
/// Phase transitions and sub-events trigger immediate updates.
/// High-frequency events are batched, with updates at most every
/// [`UPDATE_DEBOUNCE`] interval.
async fn consume_run_events(
    slack: crate::slack_client::SlackClient,
    channel: String,
    message_ts: String,
    slug: String,
    mut rx: mpsc::UnboundedReceiver<RunEvent>,
) {
    let mut tracker = RunTracker::new(slug);
    let mut last_update = Instant::now() - UPDATE_DEBOUNCE;
    let mut pending_update = false;

    while let Some(event) = rx.recv().await {
        let immediate = match event {
            RunEvent::RunStarting { ref phases } => {
                tracker.phases = phases
                    .iter()
                    .map(|name| RunPhaseDisplay {
                        name: name.clone(),
                        status: PhaseDisplayStatus::Pending,
                        duration: None,
                        turns: None,
                        cost_usd: None,
                    })
                    .collect();
                true
            }
            RunEvent::PhaseStarting {
                ref name, index, ..
            } => {
                if let Some(p) = tracker.phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Running;
                } else {
                    tracker.phases.push(RunPhaseDisplay {
                        name: name.clone(),
                        status: PhaseDisplayStatus::Running,
                        duration: None,
                        turns: None,
                        cost_usd: None,
                    });
                }
                true
            }
            RunEvent::PhaseCompleted {
                index,
                duration,
                turns,
                cost_usd,
                ..
            } => {
                if let Some(p) = tracker.phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Completed;
                    p.duration = Some(duration);
                    p.turns = Some(turns);
                    p.cost_usd = Some(cost_usd);
                }
                true
            }
            RunEvent::PhaseFailed { index, .. } => {
                if let Some(p) = tracker.phases.get_mut(index) {
                    p.status = PhaseDisplayStatus::Failed;
                }
                true
            }
            RunEvent::ReviewRound { .. } | RunEvent::VerifyAttempt { .. } => true,
            RunEvent::CreatingPr => true,
            RunEvent::PrCreated { ref url } => {
                tracker.pr_url = url.clone();
                true
            }
            RunEvent::RunFinished { .. } => true,
            RunEvent::TurnCompleted { current_turn } => {
                // Update turn count on the currently running phase
                for p in &mut tracker.phases {
                    if p.status == PhaseDisplayStatus::Running {
                        p.turns = Some(current_turn);
                    }
                }
                false
            }
            RunEvent::AgentTextDelta { .. } | RunEvent::ToolActivity { .. } => false,
            _ => false,
        };

        if immediate {
            send_update(&slack, &channel, &message_ts, &tracker).await;
            last_update = Instant::now();
            pending_update = false;
        } else {
            pending_update = true;
            if last_update.elapsed() >= UPDATE_DEBOUNCE {
                send_update(&slack, &channel, &message_ts, &tracker).await;
                last_update = Instant::now();
                pending_update = false;
            }
        }
    }

    // Flush any remaining pending update
    if pending_update {
        send_update(&slack, &channel, &message_ts, &tracker).await;
    }
}

/// Sends a single `chat.update` with the current run tracker state.
async fn send_update(
    slack: &crate::slack_client::SlackClient,
    channel: &str,
    ts: &str,
    tracker: &RunTracker,
) {
    let blocks = tracker.to_blocks();
    if let Err(e) = slack.update_message(channel, ts, blocks).await {
        warn!(error = %e, channel, "Failed to update run progress message");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_format_run_task_key_with_repo_path() {
        let key = format!("run:{}:{}", "/repos/myproject", "add-auth");
        assert_eq!(key, "run:/repos/myproject:add-auth");
    }

    #[test]
    fn test_should_define_debounce_interval() {
        assert_eq!(UPDATE_DEBOUNCE, Duration::from_secs(3));
    }

    #[test]
    fn test_should_create_run_tracker() {
        let tracker = RunTracker::new("add-auth".to_string());
        assert!(tracker.phases.is_empty());
        assert!(tracker.pr_url.is_none());
    }

    #[test]
    fn test_should_render_tracker_blocks_without_pr() {
        let tracker = RunTracker::new("add-auth".to_string());
        let blocks = tracker.to_blocks();
        // Should produce at least a header + section
        assert!(blocks.len() >= 2);
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("add-auth"));
    }

    #[test]
    fn test_should_render_tracker_blocks_with_pr() {
        let mut tracker = RunTracker::new("add-auth".to_string());
        tracker.pr_url = Some("https://github.com/org/repo/pull/42".to_string());
        let blocks = tracker.to_blocks();
        let last = blocks.last().unwrap_or(&serde_json::Value::Null);
        let text = last["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("PR:"));
        assert!(text.contains("pull/42"));
    }

    #[test]
    fn test_should_extract_task_display_name_for_dev_phase() {
        let task = coda_core::Task::DevPhase {
            name: "setup-types".to_string(),
            feature_slug: "add-auth".to_string(),
        };
        assert_eq!(task_display_name(&task), "setup-types");
    }

    #[test]
    fn test_should_extract_task_display_name_for_review() {
        let task = coda_core::Task::Review {
            feature_slug: "add-auth".to_string(),
        };
        assert_eq!(task_display_name(&task), "review");
    }

    #[test]
    fn test_should_extract_task_display_name_for_verify() {
        let task = coda_core::Task::Verify {
            feature_slug: "add-auth".to_string(),
        };
        assert_eq!(task_display_name(&task), "verify");
    }

    #[test]
    fn test_should_extract_task_display_name_for_create_pr() {
        let task = coda_core::Task::CreatePr {
            feature_slug: "add-auth".to_string(),
        };
        assert_eq!(task_display_name(&task), "create-pr");
    }

    #[test]
    fn test_should_build_run_summary_from_results() {
        let results = vec![coda_core::TaskResult {
            task: coda_core::Task::DevPhase {
                name: "setup".to_string(),
                feature_slug: "add-auth".to_string(),
            },
            status: coda_core::TaskStatus::Completed,
            turns: 3,
            cost_usd: 0.25,
            duration: Duration::from_secs(60),
            artifacts: vec![],
        }];
        let blocks = build_run_summary("add-auth", &results);
        assert!(!blocks.is_empty());
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("add-auth"));
    }

    #[test]
    fn test_should_build_run_summary_with_failed_task() {
        let results = vec![coda_core::TaskResult {
            task: coda_core::Task::Verify {
                feature_slug: "add-auth".to_string(),
            },
            status: coda_core::TaskStatus::Failed {
                error: "tests failed".to_string(),
            },
            turns: 2,
            cost_usd: 0.10,
            duration: Duration::from_secs(30),
            artifacts: vec![],
        }];
        let blocks = build_run_summary("add-auth", &results);
        assert!(!blocks.is_empty());
    }
}
