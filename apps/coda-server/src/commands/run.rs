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
//!
//! Each phase also gets a dedicated thread reply under the progress
//! message, streaming live agent text and tool activity. A completion
//! notification is posted as a new channel message when the run finishes
//! so that Slack delivers a notification to users.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coda_core::{CoreError, RunEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::error::ServerError;
use crate::formatter::{self, PhaseDisplayStatus, RunPhaseDisplay};
use crate::handlers::commands::SlashCommandPayload;
use crate::slack_client::SlackClient;
use crate::state::{AppState, TaskCleanupGuard};

use super::resolve_engine;
use super::streaming::{
    HEARTBEAT_INTERVAL, SLACK_SECTION_CHAR_LIMIT, STREAM_UPDATE_DEBOUNCE, format_tool_activity,
    markdown_to_slack, split_into_chunks,
};

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
#[instrument(skip(state, payload), fields(channel = %payload.channel_id, slug = %feature_slug))]
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

    let config_info = engine.config().resolve_run().to_string();

    // Post initial progress message
    let initial_blocks = formatter::run_progress(&slug, &[], Some(&config_info));
    let resp = state.slack().post_message(&channel, initial_blocks).await?;
    let message_ts = resp.ts;

    // Create cancellation token and store it with the task
    let cancel_token = CancellationToken::new();

    // Spawn background task
    let state_clone = Arc::clone(&state);
    let channel_clone = channel.clone();
    let slug_clone = slug.clone();
    let repo_path_clone = repo_path;
    let cancel_token_clone = cancel_token.clone();

    let handle = tokio::spawn(async move {
        run_feature_task(
            state_clone,
            engine,
            channel_clone,
            message_ts,
            slug_clone,
            repo_path_clone,
            cancel_token_clone,
        )
        .await;
    });

    state.running_tasks().insert(task_key, handle, cancel_token);

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
    cancel_token: CancellationToken,
) {
    // Guard guarantees running-task cleanup even on panic/abort.
    let task_key = format!("run:{}:{slug}", repo_path.display());
    let _guard = TaskCleanupGuard::new(Arc::clone(&state), task_key);

    let config_info = engine.config().resolve_run().to_string();
    let (tx, rx) = mpsc::unbounded_channel();
    let slack = state.slack().clone();

    // Drive event consumption in parallel with engine execution
    let slug_for_events = slug.clone();
    let config_info_for_events = config_info.clone();
    let event_handle = tokio::spawn(consume_run_events(
        slack,
        channel.clone(),
        message_ts.clone(),
        slug_for_events,
        rx,
        config_info_for_events,
    ));

    debug!(slug, "Starting engine.run()");
    let start = Instant::now();
    let result = engine.run(&slug, Some(tx), cancel_token).await;
    let duration_ms = start.elapsed().as_millis();
    debug!(
        duration_ms,
        slug,
        success = result.is_ok(),
        "engine.run() completed"
    );

    // Wait for event consumer to finish processing remaining events.
    // Bounded timeout prevents hanging if a leaked sender clone
    // (e.g. from an orphaned stderr reader task) keeps the channel open.
    let pr_url = match tokio::time::timeout(Duration::from_secs(10), event_handle).await {
        Ok(Ok(url)) => url,
        Ok(Err(e)) => {
            warn!(error = %e, "Run event consumer task panicked");
            None
        }
        Err(_) => {
            warn!(
                slug,
                "Run event consumer did not finish within 10s, proceeding"
            );
            None
        }
    };

    // Build final summary and notification based on outcome
    let (final_blocks, notification_blocks) = match result {
        Ok(ref results) => {
            info!(channel, slug, "Run completed successfully");
            let phases = results_to_phases(results);
            let progress = formatter::run_progress(&slug, &phases, Some(&config_info));
            let notification = formatter::run_completion_notification(
                &slug,
                true,
                &phases,
                pr_url.as_deref(),
                None,
            );
            (progress, notification)
        }
        Err(CoreError::Cancelled) => {
            info!(channel, slug, "Run was cancelled");
            let cancelled = formatter::run_cancelled(&slug, &[]);
            let notification = formatter::run_cancellation_notification(&slug);
            (cancelled, notification)
        }
        Err(ref e) => {
            warn!(channel, slug, error = %e, "Run failed");
            let err_str = e.to_string();
            let failure = formatter::run_failure(&slug, &[], &err_str);
            let notification =
                formatter::run_completion_notification(&slug, false, &[], None, Some(&err_str));
            (failure, notification)
        }
    };

    // Update the progress message with final state
    if let Err(e) = state
        .slack()
        .update_message(&channel, &message_ts, final_blocks)
        .await
    {
        warn!(error = %e, channel, "Failed to post run final update");
    }

    // Post a new channel message as completion notification (triggers Slack notification)
    if let Err(e) = state
        .slack()
        .post_message(&channel, notification_blocks)
        .await
    {
        warn!(error = %e, channel, "Failed to post run completion notification");
    }

    // Cleanup is handled by `_guard` (TaskCleanupGuard) on drop.
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

/// Converts engine [`TaskResult`](coda_core::TaskResult)s into display phases.
fn results_to_phases(results: &[coda_core::TaskResult]) -> Vec<RunPhaseDisplay> {
    results
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
        .collect()
}

/// Builds a final summary message from the run results.
#[cfg(test)]
fn build_run_summary(slug: &str, results: &[coda_core::TaskResult]) -> Vec<serde_json::Value> {
    let phases = results_to_phases(results);
    formatter::run_progress(slug, &phases, None)
}

/// Mutable tracking state for the run event consumer.
struct RunTracker {
    /// Per-phase display state.
    phases: Vec<RunPhaseDisplay>,
    /// Feature slug for rendering.
    slug: String,
    /// PR URL if created.
    pr_url: Option<String>,
    /// Resolved config display string.
    config_info: String,
}

impl RunTracker {
    fn new(slug: String, config_info: String) -> Self {
        Self {
            phases: Vec::new(),
            slug,
            pr_url: None,
            config_info,
        }
    }

    fn to_blocks(&self) -> Vec<serde_json::Value> {
        let mut blocks = formatter::run_progress(&self.slug, &self.phases, Some(&self.config_info));
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

/// Manages per-phase thread replies with live agent output streaming.
///
/// When each phase starts, a new thread reply is posted under the
/// progress message. `AgentTextDelta` and `ToolActivity` events are
/// streamed into the current reply using the same debounced update
/// pattern from the plan command. Each phase gets its own reply to
/// prevent exceeding Slack message limits on long runs.
struct PhaseThreadStreamer {
    slack: SlackClient,
    channel: String,
    /// Progress message ts, used as thread parent.
    thread_ts: String,
    /// Current phase reply being streamed to.
    current_reply_ts: Option<String>,
    /// Name of the current phase.
    current_phase: Option<String>,
    /// Accumulated text for the current phase.
    text: String,
    /// Last time a Slack update was sent for the phase reply.
    last_update: Instant,
    /// Whether there are pending text changes to flush.
    pending: bool,
    /// When the current phase started (for heartbeat elapsed time).
    started: Instant,
}

impl PhaseThreadStreamer {
    fn new(slack: SlackClient, channel: String, thread_ts: String) -> Self {
        Self {
            slack,
            channel,
            thread_ts,
            current_reply_ts: None,
            current_phase: None,
            text: String::new(),
            last_update: Instant::now(),
            pending: false,
            started: Instant::now(),
        }
    }

    /// Starts a new phase: finalizes the previous reply and posts a new
    /// thread reply for the incoming phase.
    async fn start_phase(&mut self, name: &str) {
        self.finalize_current().await;

        self.current_phase = Some(name.to_string());
        self.text.clear();
        self.pending = false;
        self.started = Instant::now();

        let msg = format!("*Phase: `{name}`*\n_Starting..._");
        match self
            .slack
            .post_thread_reply(&self.channel, &self.thread_ts, &msg)
            .await
        {
            Ok(ts) => {
                self.current_reply_ts = Some(ts);
                self.last_update = Instant::now();
            }
            Err(e) => {
                warn!(error = %e, phase = name, "Failed to post phase thread reply");
                self.current_reply_ts = None;
            }
        }
    }

    /// Appends a text delta to the buffer and debounces the Slack update.
    /// When text exceeds `SLACK_SECTION_CHAR_LIMIT`, a sliding window shows
    /// the latest content so the user always sees fresh output.
    async fn on_text_delta(&mut self, delta: &str) {
        self.text.push_str(delta);

        let Some(ref ts) = self.current_reply_ts else {
            return;
        };

        self.pending = true;
        if self.last_update.elapsed() < STREAM_UPDATE_DEBOUNCE {
            return;
        }

        let phase_header = self.phase_header();
        let total_chars = self.text.len();

        let msg = if total_chars <= SLACK_SECTION_CHAR_LIMIT {
            let slack_text = markdown_to_slack(&self.text);
            format!("{phase_header}\n{slack_text}")
        } else {
            let tail_start = tail_char_boundary(&self.text, SLACK_SECTION_CHAR_LIMIT);
            let tail = &self.text[tail_start..];
            let slack_text = markdown_to_slack(tail);
            format!(
                "{phase_header}\n_...({total_chars} chars total, showing last {})..._\n{slack_text}",
                total_chars - tail_start,
            )
        };

        let _ = self
            .slack
            .update_message_text(&self.channel, ts, &msg)
            .await;
        self.last_update = Instant::now();
        self.pending = false;
    }

    /// Appends a tool activity line and updates the reply immediately.
    async fn on_tool_activity(&mut self, tool_name: &str, summary: &str) {
        let Some(ref ts) = self.current_reply_ts else {
            return;
        };

        let activity = format_tool_activity(tool_name, summary);
        let phase_header = self.phase_header();
        let total_chars = self.text.len();
        let display = if total_chars <= SLACK_SECTION_CHAR_LIMIT {
            let slack_text = markdown_to_slack(&self.text);
            format!("{phase_header}\n{slack_text}\n\n{activity}")
        } else {
            let tail_start = tail_char_boundary(&self.text, SLACK_SECTION_CHAR_LIMIT);
            let tail = &self.text[tail_start..];
            let slack_text = markdown_to_slack(tail);
            format!(
                "{phase_header}\n_...({total_chars} chars total, showing last {})..._\n{slack_text}\n\n{activity}",
                total_chars - tail_start,
            )
        };
        let _ = self
            .slack
            .update_message_text(&self.channel, ts, &display)
            .await;
        self.last_update = Instant::now();
        self.pending = false;
    }

    /// Finalizes the current phase reply with a completion stats footer.
    async fn on_phase_completed(&mut self, duration: Duration, turns: u32, cost_usd: f64) {
        let dur = formatter::format_duration(duration.as_secs());
        let footer = format!(
            "_:white_check_mark: Completed \u{2014} {turns} turns \u{00b7} ${cost_usd:.2} \u{00b7} {dur}_"
        );
        self.finalize_with_footer(&footer).await;
    }

    /// Finalizes the current phase reply with a failure footer.
    async fn on_phase_failed(&mut self, error: &str) {
        let footer = format!("_:x: Failed: {error}_");
        self.finalize_with_footer(&footer).await;
    }

    /// Shows a heartbeat indicator during idle periods.
    async fn heartbeat(&mut self) {
        let Some(ref ts) = self.current_reply_ts else {
            return;
        };

        let elapsed = self.started.elapsed().as_secs();
        let phase_header = self.phase_header();
        let total_chars = self.text.len();
        let msg = if self.text.is_empty() {
            format!("{phase_header}\n_:hourglass: working... ({elapsed}s)_")
        } else if total_chars <= SLACK_SECTION_CHAR_LIMIT {
            let slack_text = markdown_to_slack(&self.text);
            format!("{phase_header}\n{slack_text}\n\n_:hourglass: working... ({elapsed}s)_")
        } else {
            let tail_start = tail_char_boundary(&self.text, SLACK_SECTION_CHAR_LIMIT);
            let tail = &self.text[tail_start..];
            let slack_text = markdown_to_slack(tail);
            format!(
                "{phase_header}\n_...({total_chars} chars total, showing last {})..._\n{slack_text}\n\n_:hourglass: working... ({elapsed}s)_",
                total_chars - tail_start,
            )
        };
        let _ = self
            .slack
            .update_message_text(&self.channel, ts, &msg)
            .await;
        self.last_update = Instant::now();
    }

    /// Finalizes the current phase reply, splitting into multiple replies
    /// if the text exceeds the Slack limit.
    async fn finalize_current(&mut self) {
        if self.current_reply_ts.is_none() {
            return;
        }

        if self.text.is_empty() {
            return;
        }

        let phase_header = self.phase_header();
        let slack_text = markdown_to_slack(&self.text);
        let chunks = split_into_chunks(&slack_text, SLACK_SECTION_CHAR_LIMIT);

        // First chunk updates the existing reply
        if let Some(first) = chunks.first() {
            let msg = format!("{phase_header}\n{first}");
            if let Some(ref ts) = self.current_reply_ts {
                let _ = self
                    .slack
                    .update_message_text(&self.channel, ts, &msg)
                    .await;
            }
        }

        // Remaining chunks are posted as new thread replies
        for chunk in chunks.iter().skip(1) {
            let _ = self
                .slack
                .post_thread_reply(&self.channel, &self.thread_ts, chunk)
                .await;
        }

        self.pending = false;
    }

    /// Finalizes the current reply with a footer line appended.
    async fn finalize_with_footer(&mut self, footer: &str) {
        let Some(ref ts) = self.current_reply_ts else {
            return;
        };

        let phase_header = self.phase_header();
        let slack_text = markdown_to_slack(&self.text);

        if slack_text.len() <= SLACK_SECTION_CHAR_LIMIT {
            // Everything fits in one message
            let msg = if self.text.is_empty() {
                format!("{phase_header}\n{footer}")
            } else {
                format!("{phase_header}\n{slack_text}\n\n{footer}")
            };
            let _ = self
                .slack
                .update_message_text(&self.channel, ts, &msg)
                .await;
        } else {
            // Split the text and put footer in the last message
            let chunks = split_into_chunks(&slack_text, SLACK_SECTION_CHAR_LIMIT);

            if let Some(first) = chunks.first() {
                let msg = format!("{phase_header}\n{first}");
                let _ = self
                    .slack
                    .update_message_text(&self.channel, ts, &msg)
                    .await;
            }

            let last_idx = chunks.len().saturating_sub(1);
            for (i, chunk) in chunks.iter().skip(1).enumerate() {
                let msg = if i + 1 == last_idx {
                    format!("{chunk}\n\n{footer}")
                } else {
                    (*chunk).to_string()
                };
                let _ = self
                    .slack
                    .post_thread_reply(&self.channel, &self.thread_ts, &msg)
                    .await;
            }

            // If only one chunk, footer goes right after it
            if chunks.len() == 1 {
                let _ = self
                    .slack
                    .post_thread_reply(&self.channel, &self.thread_ts, footer)
                    .await;
            }
        }

        self.pending = false;
        self.current_reply_ts = None;
        self.current_phase = None;
    }

    /// Returns the phase header line for the current phase.
    fn phase_header(&self) -> String {
        match self.current_phase {
            Some(ref name) => format!("*Phase: `{name}`*"),
            None => "*Phase*".to_string(),
        }
    }
}

/// Returns the byte index for the last `max_bytes` of `text`,
/// aligned to a UTF-8 char boundary.
fn tail_char_boundary(text: &str, max_bytes: usize) -> usize {
    if text.len() <= max_bytes {
        return 0;
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// Consumes [`RunEvent`]s from the channel, debounces Slack progress
/// message updates, and streams per-phase agent output into thread replies.
///
/// Returns the PR URL if one was created, for use in the completion
/// notification.
async fn consume_run_events(
    slack: SlackClient,
    channel: String,
    message_ts: String,
    slug: String,
    mut rx: mpsc::UnboundedReceiver<RunEvent>,
    config_info: String,
) -> Option<String> {
    let mut tracker = RunTracker::new(slug, config_info);
    let mut streamer = PhaseThreadStreamer::new(slack.clone(), channel.clone(), message_ts.clone());
    let mut last_update = Instant::now() - STREAM_UPDATE_DEBOUNCE;
    let mut pending_update = false;

    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break; };
                let immediate = match event {
                    RunEvent::RunStarting { ref phases, .. } => {
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
                        streamer.start_phase(name).await;
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
                        streamer.on_phase_completed(duration, turns, cost_usd).await;
                        true
                    }
                    RunEvent::PhaseFailed { ref error, index, .. } => {
                        if let Some(p) = tracker.phases.get_mut(index) {
                            p.status = PhaseDisplayStatus::Failed;
                        }
                        streamer.on_phase_failed(error).await;
                        true
                    }
                    RunEvent::ReviewRound { .. }
                    | RunEvent::VerifyAttempt { .. }
                    | RunEvent::CheckStarting { .. }
                    | RunEvent::CheckCompleted { .. } => true,
                    RunEvent::CreatingPr => {
                        tracker.phases.push(RunPhaseDisplay {
                            name: "create-pr".to_string(),
                            status: PhaseDisplayStatus::Running,
                            duration: None,
                            turns: None,
                            cost_usd: None,
                        });
                        true
                    }
                    RunEvent::PrCreated { ref url } => {
                        tracker.pr_url = url.clone();
                        if let Some(p) = tracker.phases.last_mut()
                            && p.name == "create-pr"
                        {
                            p.status = PhaseDisplayStatus::Completed;
                        }
                        true
                    }
                    RunEvent::Reconnecting { .. } => true,
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
                    RunEvent::AgentTextDelta { ref text } => {
                        streamer.on_text_delta(text).await;
                        false
                    }
                    RunEvent::ToolActivity {
                        ref tool_name,
                        ref summary,
                    } => {
                        streamer.on_tool_activity(tool_name, summary).await;
                        false
                    }
                    _ => false,
                };

                if immediate {
                    send_update(&slack, &channel, &message_ts, &tracker).await;
                    last_update = Instant::now();
                    pending_update = false;
                } else {
                    pending_update = true;
                    if last_update.elapsed() >= STREAM_UPDATE_DEBOUNCE {
                        send_update(&slack, &channel, &message_ts, &tracker).await;
                        last_update = Instant::now();
                        pending_update = false;
                    }
                }
            }
            // Heartbeat: show progress when the stream is idle
            () = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                streamer.heartbeat().await;
            }
        }
    }

    // Flush any remaining pending progress update
    if pending_update {
        send_update(&slack, &channel, &message_ts, &tracker).await;
    }

    // Finalize any remaining phase thread reply
    streamer.finalize_current().await;

    tracker.pr_url
}

/// Sends a single `chat.update` with the current run tracker state.
async fn send_update(slack: &SlackClient, channel: &str, ts: &str, tracker: &RunTracker) {
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
    fn test_should_create_run_tracker() {
        let tracker = RunTracker::new("add-auth".to_string(), String::new());
        assert!(tracker.phases.is_empty());
        assert!(tracker.pr_url.is_none());
    }

    #[test]
    fn test_should_render_tracker_blocks_without_pr() {
        let tracker = RunTracker::new("add-auth".to_string(), String::new());
        let blocks = tracker.to_blocks();
        // Should produce at least a header + section
        assert!(blocks.len() >= 2);
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("add-auth"));
    }

    #[test]
    fn test_should_render_tracker_blocks_with_pr() {
        let mut tracker = RunTracker::new("add-auth".to_string(), String::new());
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

    #[test]
    fn test_should_create_phase_thread_streamer() {
        let slack = SlackClient::new("xoxb-test".into()).unwrap();
        let streamer = PhaseThreadStreamer::new(slack, "C123".to_string(), "1234.5678".to_string());
        assert!(streamer.current_reply_ts.is_none());
        assert!(streamer.current_phase.is_none());
        assert!(streamer.text.is_empty());
        assert!(!streamer.pending);
    }

    #[test]
    fn test_should_format_phase_header_with_name() {
        let slack = SlackClient::new("xoxb-test".into()).unwrap();
        let mut streamer =
            PhaseThreadStreamer::new(slack, "C123".to_string(), "1234.5678".to_string());
        streamer.current_phase = Some("implement".to_string());
        assert_eq!(streamer.phase_header(), "*Phase: `implement`*");
    }

    #[test]
    fn test_should_format_phase_header_without_name() {
        let slack = SlackClient::new("xoxb-test".into()).unwrap();
        let streamer = PhaseThreadStreamer::new(slack, "C123".to_string(), "1234.5678".to_string());
        assert_eq!(streamer.phase_header(), "*Phase*");
    }

    #[test]
    fn test_should_convert_results_to_phases() {
        let results = vec![
            coda_core::TaskResult {
                task: coda_core::Task::DevPhase {
                    name: "setup".to_string(),
                    feature_slug: "add-auth".to_string(),
                },
                status: coda_core::TaskStatus::Completed,
                turns: 3,
                cost_usd: 0.25,
                duration: Duration::from_secs(60),
                artifacts: vec![],
            },
            coda_core::TaskResult {
                task: coda_core::Task::Review {
                    feature_slug: "add-auth".to_string(),
                },
                status: coda_core::TaskStatus::Failed {
                    error: "issues found".to_string(),
                },
                turns: 2,
                cost_usd: 0.10,
                duration: Duration::from_secs(30),
                artifacts: vec![],
            },
        ];
        let phases = results_to_phases(&results);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].name, "setup");
        assert_eq!(phases[0].status, PhaseDisplayStatus::Completed);
        assert_eq!(phases[1].name, "review");
        assert_eq!(phases[1].status, PhaseDisplayStatus::Failed);
    }

    #[test]
    fn test_tail_char_boundary_short_text() {
        assert_eq!(tail_char_boundary("hello", 10), 0);
        assert_eq!(tail_char_boundary("hello", 5), 0);
    }

    #[test]
    fn test_tail_char_boundary_truncates() {
        let text = "abcdefghij"; // 10 bytes
        assert_eq!(tail_char_boundary(text, 5), 5);
        assert_eq!(&text[5..], "fghij");
    }

    #[test]
    fn test_tail_char_boundary_utf8_alignment() {
        // "héllo" — 'é' is 2 bytes (0xC3 0xA9), total = 6 bytes
        let text = "héllo";
        // Requesting last 4 bytes: would start at byte 2, which is inside 'é'
        // Should align forward to byte 3 ('l')
        let start = tail_char_boundary(text, 4);
        assert!(text.is_char_boundary(start));
        assert_eq!(&text[start..], "llo");
    }
}
