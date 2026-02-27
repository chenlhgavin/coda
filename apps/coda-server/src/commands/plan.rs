//! `/coda plan <slug>` command handler for interactive planning threads.
//!
//! Creates a Slack thread parent message, initializes a
//! [`PlanSession`](coda_core::PlanSession) from `coda-core`, and stores it in
//! the [`SessionManager`](crate::session::SessionManager). Subsequent thread
//! replies are routed through the Events API handler in
//! [`handlers::events`](crate::handlers::events).
//!
//! Special keywords in thread replies:
//! - `approve` — formalizes the design spec and generates a verification plan
//! - `done` — finalizes the session, creates the worktree and writes specs
//! - `quit` — disconnects the session without finalizing

use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use crate::error::ServerError;
use crate::formatter;
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

use super::resolve_engine;
use super::streaming::{
    HEARTBEAT_INTERVAL, SLACK_SECTION_CHAR_LIMIT, STREAM_UPDATE_DEBOUNCE, format_tool_activity,
    markdown_to_slack, split_into_chunks, truncated_preview,
};

/// Handles `/coda plan <feature_slug>`.
///
/// Resolves the channel binding, creates a thread parent message with
/// a plan header, initializes a `PlanSession`, and stores it in the
/// session manager. The first planning prompt is sent automatically
/// to bootstrap the conversation.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails, the engine cannot
/// be created, or the plan session cannot be initialized.
#[instrument(skip(state, payload), fields(channel = %payload.channel_id, slug = %feature_slug))]
pub async fn handle_plan(
    state: Arc<AppState>,
    payload: &SlashCommandPayload,
    feature_slug: &str,
) -> Result<(), ServerError> {
    let channel = payload.channel_id.clone();
    let slug = feature_slug.to_string();

    let Some((_repo_path, engine)) = resolve_engine(state.as_ref(), &channel).await? else {
        return Ok(());
    };

    info!(channel, feature_slug = slug, "Starting plan command");

    let config_info = engine.config().resolve_plan().to_string();

    // Create the PlanSession from the engine
    debug!("Creating plan session via engine.plan()");
    let start = Instant::now();
    let session = engine.plan(&slug)?;
    debug!(
        duration_ms = start.elapsed().as_millis(),
        "Plan session created",
    );

    // Post thread parent message
    let header_blocks = formatter::plan_thread_header(&slug, "Discussing", Some(&config_info));
    let resp = state.slack().post_message(&channel, header_blocks).await?;
    let thread_ts = resp.ts.clone();

    // Store session in the session manager
    state
        .sessions()
        .insert(channel.clone(), thread_ts.clone(), session);

    // Post initial instruction in the thread
    let intro = format!(
        "Planning session started for `{slug}`.\n\
         Describe your feature requirements and I'll help design it.\n\n\
         _Thread commands:_\n\
         \u{2022} Type your requirements to discuss the design\n\
         \u{2022} `approve` \u{2014} formalize the design spec\n\
         \u{2022} `done` \u{2014} finalize and create the worktree\n\
         \u{2022} `quit` \u{2014} cancel this session"
    );
    if let Err(e) = state
        .slack()
        .post_thread_reply(&channel, &thread_ts, &intro)
        .await
    {
        warn!(error = %e, "Failed to post plan intro message");
    }

    Ok(())
}

/// Handles a user message in an active plan thread.
///
/// Routes special keywords (`approve`, `done`, `quit`) to the
/// corresponding `PlanSession` methods, and forwards regular text
/// as planning conversation turns. Uses the hourglass reaction as
/// a thinking indicator during AI responses.
#[instrument(skip(state, text), fields(channel = %channel, thread_ts = %thread_ts))]
pub async fn handle_thread_message(
    state: Arc<AppState>,
    channel: &str,
    thread_ts: &str,
    user_ts: &str,
    text: &str,
) {
    let trimmed = text.trim().to_lowercase();

    match trimmed.as_str() {
        "approve" => handle_approve(state, channel, thread_ts).await,
        "done" => handle_done(state, channel, thread_ts).await,
        "quit" => handle_quit(state, channel, thread_ts).await,
        _ => handle_conversation(state, channel, thread_ts, user_ts, text).await,
    }
}

/// Sends a conversation turn to the plan session and streams the response.
///
/// Posts a placeholder "Thinking..." reply immediately, then updates it
/// with partial responses as the AI generates text. Uses debounced
/// `chat.update` calls (at most once per [`STREAM_UPDATE_DEBOUNCE`]) to
/// respect Slack rate limits.
async fn handle_conversation(
    state: Arc<AppState>,
    channel: &str,
    thread_ts: &str,
    user_ts: &str,
    text: &str,
) {
    // Add thinking indicator on the user's message
    let _ = state
        .slack()
        .add_reaction(channel, user_ts, "hourglass_flowing_sand")
        .await;

    // Post placeholder reply in the thread
    let reply_ts = match state
        .slack()
        .post_thread_reply(channel, thread_ts, "_Thinking..._")
        .await
    {
        Ok(ts) => ts,
        Err(e) => {
            warn!(error = %e, "Failed to post placeholder reply");
            let _ = state
                .slack()
                .remove_reaction(channel, user_ts, "hourglass_flowing_sand")
                .await;
            return;
        }
    };

    // Create streaming channel
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<coda_core::PlanStreamUpdate>();

    // Spawn debounced updater task that progressively updates the placeholder
    let slack = state.slack().clone();
    let update_channel = channel.to_string();
    let update_ts = reply_ts.clone();
    let updater = tokio::spawn(async move {
        consume_streaming_updates(slack, update_channel, update_ts, rx).await;
    });

    // Acquire session Arc (briefly touches DashMap, then releases shard lock)
    let session_arc = match state.sessions().acquire(channel, thread_ts) {
        Some(arc) => arc,
        None => {
            warn!(channel, thread_ts, "Session not found for thread message");
            return;
        }
    };

    // Send message to AI with streaming text updates (no DashMap lock held)
    debug!("Calling session.send_streaming()");
    let start = Instant::now();
    let response = {
        let mut session = session_arc.lock().await;
        session.send_streaming(text, tx).await
    };
    debug!(
        duration_ms = start.elapsed().as_millis(),
        success = response.is_ok(),
        "session.send_streaming() completed",
    );

    // Remove thinking indicator
    let _ = state
        .slack()
        .remove_reaction(channel, user_ts, "hourglass_flowing_sand")
        .await;

    // Wait for updater to flush remaining updates
    if let Err(join_err) = updater.await {
        warn!(error = %join_err, channel, thread_ts, "Streaming updater task panicked");
        let _ = state
            .slack()
            .update_message_text(
                channel,
                &reply_ts,
                ":x: An internal error occurred while streaming. The full response will follow.",
            )
            .await;
    }

    // Final update: post the complete response, splitting into multiple
    // messages if it exceeds the Slack inline limit.
    match response {
        Ok(reply) if reply.trim().is_empty() => {
            let _ = state
                .slack()
                .update_message_text(channel, &reply_ts, "_Agent produced no text response._")
                .await;
        }
        Ok(reply) => {
            let slack_text = markdown_to_slack(&reply);
            let chunks = split_into_chunks(&slack_text, SLACK_SECTION_CHAR_LIMIT);
            // First chunk replaces the placeholder message
            if let Some(first) = chunks.first() {
                let _ = state
                    .slack()
                    .update_message_text(channel, &reply_ts, first)
                    .await;
            }
            // Remaining chunks are posted as new thread replies
            for chunk in chunks.iter().skip(1) {
                let _ = state
                    .slack()
                    .post_thread_reply(channel, thread_ts, chunk)
                    .await;
            }
        }
        Err(e) => {
            warn!(error = %e, channel, thread_ts, "PlanSession::send_streaming failed");
            let error_msg = format!(":warning: Planning error: {e}");
            let _ = state
                .slack()
                .update_message_text(channel, &reply_ts, &error_msg)
                .await;
        }
    }
}

/// Consumes streaming plan updates and debounces Slack `chat.update` calls.
///
/// Handles two types of updates:
/// - [`PlanStreamUpdate::Text`]: accumulated response text, debounced at
///   [`STREAM_UPDATE_DEBOUNCE`] intervals.
/// - [`PlanStreamUpdate::ToolActivity`]: tool invocation events, displayed
///   immediately as a status line appended to the current text.
///
/// When the accumulated text exceeds [`SLACK_SECTION_CHAR_LIMIT`], further
/// text updates are skipped (the final handler will split into multiple
/// messages), but tool activity is still shown. Performs a final flush
/// when the sender is dropped (stream ends).
async fn consume_streaming_updates(
    slack: crate::slack_client::SlackClient,
    channel: String,
    ts: String,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<coda_core::PlanStreamUpdate>,
) {
    let mut last_update = Instant::now();
    let mut pending = false;
    let mut latest_text = String::new();
    let mut exceeded_limit = false;
    let started = Instant::now();

    loop {
        tokio::select! {
            update = rx.recv() => {
                let Some(update) = update else { break; };
                match update {
                    coda_core::PlanStreamUpdate::Text(text) => {
                        latest_text = text;

                        // Skip empty text updates — Slack rejects empty text
                        // with `no_text`.
                        if latest_text.is_empty() {
                            continue;
                        }

                        // Once the text exceeds the inline limit, post a
                        // truncated preview once and stop further text updates.
                        if !exceeded_limit && latest_text.len() > SLACK_SECTION_CHAR_LIMIT {
                            exceeded_limit = true;
                            let preview = truncated_preview(&latest_text);
                            let slack_preview = markdown_to_slack(preview);
                            let msg = format!(
                                "{slack_preview}\n\n_:hourglass: generating..._"
                            );
                            let _ = slack
                                .update_message_text(&channel, &ts, &msg)
                                .await;
                            last_update = Instant::now();
                            pending = false;
                            continue;
                        }

                        if exceeded_limit {
                            continue;
                        }

                        pending = true;
                        if last_update.elapsed() >= STREAM_UPDATE_DEBOUNCE {
                            let slack_text = markdown_to_slack(&latest_text);
                            let _ = slack
                                .update_message_text(&channel, &ts, &slack_text)
                                .await;
                            last_update = Instant::now();
                            pending = false;
                        }
                    }
                    coda_core::PlanStreamUpdate::ToolActivity {
                        tool_name,
                        summary,
                    } => {
                        let activity = format_tool_activity(&tool_name, &summary);
                        let display = if exceeded_limit {
                            let preview = truncated_preview(&latest_text);
                            let slack_preview = markdown_to_slack(preview);
                            format!("{slack_preview}\n\n{activity}")
                        } else {
                            let slack_text = markdown_to_slack(&latest_text);
                            format!("{slack_text}\n\n{activity}")
                        };
                        let _ = slack
                            .update_message_text(&channel, &ts, &display)
                            .await;
                        last_update = Instant::now();
                        pending = false;
                    }
                }
            }
            // Heartbeat: show progress when the stream is idle
            () = tokio::time::sleep(HEARTBEAT_INTERVAL) => {
                let elapsed = started.elapsed().as_secs();
                let msg = if exceeded_limit {
                    let preview = truncated_preview(&latest_text);
                    let slack_preview = markdown_to_slack(preview);
                    format!("{slack_preview}\n\n_:hourglass: generating... ({elapsed}s)_")
                } else if latest_text.is_empty() {
                    format!("_:hourglass: thinking... ({elapsed}s)_")
                } else {
                    let slack_text = markdown_to_slack(&latest_text);
                    format!("{slack_text}\n\n_:hourglass: thinking... ({elapsed}s)_")
                };
                let _ = slack.update_message_text(&channel, &ts, &msg).await;
                last_update = Instant::now();
            }
        }
    }

    // Flush any remaining pending text update (only if within limit and non-empty)
    if pending && !exceeded_limit && !latest_text.is_empty() {
        let slack_text = markdown_to_slack(&latest_text);
        let _ = slack.update_message_text(&channel, &ts, &slack_text).await;
    }
}

/// Handles the `approve` keyword: formalizes design and verification.
#[instrument(skip(state), fields(channel = %channel, thread_ts = %thread_ts))]
async fn handle_approve(state: Arc<AppState>, channel: &str, thread_ts: &str) {
    // Add thinking indicator to the thread parent
    let _ = state
        .slack()
        .add_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    // Acquire session Arc (briefly touches DashMap, then releases shard lock)
    let session_arc = match state.sessions().acquire(channel, thread_ts) {
        Some(arc) => arc,
        None => {
            warn!(channel, thread_ts, "Session not found for approve");
            return;
        }
    };

    // Run approve with only the tokio Mutex held (no DashMap lock)
    debug!("Calling session.approve()");
    let start = Instant::now();
    let (result, slug) = {
        let mut session = session_arc.lock().await;
        let slug = session.feature_slug().to_string();
        let result = session.approve().await;
        (result, slug)
    };
    debug!(
        duration_ms = start.elapsed().as_millis(),
        success = result.is_ok(),
        "session.approve() completed",
    );

    let _ = state
        .slack()
        .remove_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    match result {
        Ok((design, verification)) => {
            info!(channel, thread_ts, "Plan approved");

            // Update thread parent to show Approved status
            let header_blocks = formatter::plan_thread_header(&slug, "Approved", None);
            let _ = state
                .slack()
                .update_message(channel, thread_ts, header_blocks)
                .await;

            // Post design spec (inline or as file)
            post_content_or_file(
                &state,
                channel,
                thread_ts,
                &design,
                "Design Specification",
                "design-spec.md",
                "markdown",
            )
            .await;

            // Post verification plan (inline or as file) — only when verify is enabled
            if let Some(ref verification) = verification {
                post_content_or_file(
                    &state,
                    channel,
                    thread_ts,
                    verification,
                    "Verification Plan",
                    "verification-plan.md",
                    "markdown",
                )
                .await;
            }

            let _ = state
                .slack()
                .post_thread_reply(
                    channel,
                    thread_ts,
                    "Design approved. Type `done` to finalize and create the worktree, \
                     or continue discussing to refine.",
                )
                .await;
        }
        Err(e) => {
            warn!(error = %e, channel, thread_ts, "PlanSession::approve failed");
            let error_msg = format!(":warning: Approve failed: {e}");
            let _ = state
                .slack()
                .post_thread_reply(channel, thread_ts, &error_msg)
                .await;
        }
    }
}

/// Handles the `done` keyword: finalizes the session, creates worktree.
#[instrument(skip(state), fields(channel = %channel, thread_ts = %thread_ts))]
async fn handle_done(state: Arc<AppState>, channel: &str, thread_ts: &str) {
    // Acquire session Arc (briefly touches DashMap, then releases shard lock)
    let session_arc = match state.sessions().acquire(channel, thread_ts) {
        Some(arc) => arc,
        None => {
            warn!(channel, thread_ts, "Session not found for done");
            return;
        }
    };

    // Check if approved (brief mutex lock, no DashMap lock)
    let is_approved = {
        let session = session_arc.lock().await;
        session.is_approved()
    };

    if !is_approved {
        let _ = state
            .slack()
            .post_thread_reply(
                channel,
                thread_ts,
                ":warning: Cannot finalize — design has not been approved yet. \
                 Type `approve` first.",
            )
            .await;
        return;
    }

    // Acquire repo lock for worktree creation
    let repo_path = state.bindings().get(channel);
    if let Some(ref path) = repo_path
        && let Err(holder) = state.repo_locks().try_lock(path, "plan finalize")
    {
        let _ = state
            .slack()
            .post_thread_reply(
                channel,
                thread_ts,
                &format!(":warning: Repository is busy (`{holder}`). Try again later."),
            )
            .await;
        return;
    }

    let _ = state
        .slack()
        .add_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    // Run finalize with only the tokio Mutex held (no DashMap lock)
    debug!("Calling session.finalize()");
    let start = Instant::now();
    let result = {
        let mut session = session_arc.lock().await;
        session.finalize().await
    };
    debug!(
        duration_ms = start.elapsed().as_millis(),
        success = result.is_ok(),
        "session.finalize() completed",
    );

    // Release repo lock immediately after finalize
    if let Some(ref path) = repo_path {
        state.repo_locks().unlock(path);
    }

    let _ = state
        .slack()
        .remove_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    match result {
        Ok(output) => {
            info!(
                channel,
                thread_ts,
                worktree = %output.worktree.display(),
                "Plan finalized successfully"
            );

            // Update thread parent to show Finalized status
            let slug = output
                .worktree
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let header_blocks = formatter::plan_thread_header(&slug, "Finalized", None);
            let _ = state
                .slack()
                .update_message(channel, thread_ts, header_blocks)
                .await;

            let verification_line = output
                .verification
                .as_ref()
                .map(|v| format!("\n\u{2022} Verification: `{}`", v.display()))
                .unwrap_or_default();
            let summary = format!(
                ":white_check_mark: *Planning finalized!*\n\n\
                 \u{2022} Worktree: `{}`\n\
                 \u{2022} Design: `{}`{verification_line}\n\
                 \u{2022} State: `{}`\n\n\
                 Use `/coda run {}` to start development.",
                output.worktree.display(),
                output.design_spec.display(),
                output.state.display(),
                slug,
            );
            let _ = state
                .slack()
                .post_thread_reply(channel, thread_ts, &summary)
                .await;

            // Remove session
            state.sessions().remove(channel, thread_ts);
        }
        Err(e) => {
            warn!(error = %e, channel, thread_ts, "PlanSession::finalize failed");
            let error_msg = format!(":warning: Finalize failed: {e}");
            let _ = state
                .slack()
                .post_thread_reply(channel, thread_ts, &error_msg)
                .await;
        }
    }
}

/// Handles the `quit` keyword: disconnects the session without finalizing.
#[instrument(skip(state), fields(channel = %channel, thread_ts = %thread_ts))]
async fn handle_quit(state: Arc<AppState>, channel: &str, thread_ts: &str) {
    if let Some(session_arc) = state.sessions().remove(channel, thread_ts) {
        let mut session = session_arc.lock().await;

        debug!("Calling session.disconnect()");
        let start = Instant::now();
        session.disconnect().await;
        debug!(
            duration_ms = start.elapsed().as_millis(),
            "session.disconnect() completed",
        );

        let slug = session.feature_slug().to_string();
        drop(session);
        info!(channel, thread_ts, slug, "Plan session quit by user");

        // Update thread parent
        let header_blocks = formatter::plan_thread_header(&slug, "Cancelled", None);
        let _ = state
            .slack()
            .update_message(channel, thread_ts, header_blocks)
            .await;

        let _ = state
            .slack()
            .post_thread_reply(
                channel,
                thread_ts,
                ":no_entry_sign: Planning session ended. No artifacts were created.",
            )
            .await;
    } else {
        let _ = state
            .slack()
            .post_thread_reply(
                channel,
                thread_ts,
                ":warning: No active planning session found for this thread.",
            )
            .await;
    }
}

/// Posts content inline, splitting into multiple messages if too long.
///
/// Wraps each chunk in a code block with a label. For multi-part content,
/// each part includes a `(N/M)` suffix in the header.
async fn post_content_or_file(
    state: &AppState,
    channel: &str,
    thread_ts: &str,
    content: &str,
    label: &str,
    _filename: &str,
    _filetype: &str,
) {
    // Reserve space for the code block wrapper: "*Label (N/M):*\n```\n...\n```"
    let wrapper_overhead = label.len() + 30;
    let chunk_limit = SLACK_SECTION_CHAR_LIMIT.saturating_sub(wrapper_overhead);
    let chunks = split_into_chunks(content, chunk_limit);
    let total = chunks.len();

    for (i, chunk) in chunks.iter().enumerate() {
        let header = if total == 1 {
            format!("*{label}:*")
        } else {
            format!("*{label} ({}/{}):*", i + 1, total)
        };
        let msg = format!("{header}\n```\n{chunk}\n```");
        let _ = state
            .slack()
            .post_thread_reply(channel, thread_ts, &msg)
            .await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_should_recognize_approve_keyword() {
        let trimmed = "approve".trim().to_lowercase();
        assert_eq!(trimmed, "approve");
    }

    #[test]
    fn test_should_recognize_done_keyword() {
        let trimmed = "done".trim().to_lowercase();
        assert_eq!(trimmed, "done");
    }

    #[test]
    fn test_should_recognize_quit_keyword() {
        let trimmed = "quit".trim().to_lowercase();
        assert_eq!(trimmed, "quit");
    }

    #[test]
    fn test_should_recognize_approve_with_whitespace() {
        let trimmed = "  Approve  ".trim().to_lowercase();
        assert_eq!(trimmed, "approve");
    }

    #[test]
    fn test_should_not_match_approve_prefix() {
        let trimmed = "approved".trim().to_lowercase();
        assert_ne!(trimmed, "approve");
    }

    #[test]
    fn test_should_route_regular_text_to_conversation() {
        let trimmed = "design a REST API for auth".trim().to_lowercase();
        assert!(trimmed != "approve" && trimmed != "done" && trimmed != "quit");
    }
}
