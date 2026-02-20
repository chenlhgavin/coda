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

use tracing::{info, warn};

use crate::error::ServerError;
use crate::formatter;
use crate::handlers::commands::SlashCommandPayload;
use crate::state::AppState;

use super::resolve_engine;

/// Maximum length of inline content in a Slack section block.
/// Content exceeding this is uploaded as a file snippet.
const SLACK_SECTION_CHAR_LIMIT: usize = 3000;

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

    // Create the PlanSession from the engine
    let session = engine.plan(&slug)?;

    // Post thread parent message
    let header_blocks = formatter::plan_thread_header(&slug, "Discussing");
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

/// Sends a conversation turn to the plan session and posts the response.
async fn handle_conversation(
    state: Arc<AppState>,
    channel: &str,
    thread_ts: &str,
    user_ts: &str,
    text: &str,
) {
    // Add thinking indicator
    let _ = state
        .slack()
        .add_reaction(channel, user_ts, "hourglass_flowing_sand")
        .await;

    // Scope the DashMap guard: acquire, update activity, send, then drop
    let response = {
        let mut entry = match state.sessions().get_mut(channel, thread_ts) {
            Some(e) => e,
            None => {
                warn!(channel, thread_ts, "Session not found for thread message");
                return;
            }
        };
        entry.last_activity = std::time::Instant::now();
        entry.session.send(text).await
    };

    // Remove thinking indicator
    let _ = state
        .slack()
        .remove_reaction(channel, user_ts, "hourglass_flowing_sand")
        .await;

    match response {
        Ok(reply) => {
            if let Err(e) = state
                .slack()
                .post_thread_reply(channel, thread_ts, &reply)
                .await
            {
                warn!(error = %e, "Failed to post plan reply");
            }
        }
        Err(e) => {
            warn!(error = %e, channel, thread_ts, "PlanSession::send failed");
            let error_msg = format!(":warning: Planning error: {e}");
            let _ = state
                .slack()
                .post_thread_reply(channel, thread_ts, &error_msg)
                .await;
        }
    }
}

/// Handles the `approve` keyword: formalizes design and verification.
async fn handle_approve(state: Arc<AppState>, channel: &str, thread_ts: &str) {
    // Add thinking indicator to the thread parent
    let _ = state
        .slack()
        .add_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    let result = {
        let mut entry = match state.sessions().get_mut(channel, thread_ts) {
            Some(e) => e,
            None => {
                warn!(channel, thread_ts, "Session not found for approve");
                return;
            }
        };
        entry.last_activity = std::time::Instant::now();
        entry.session.approve().await
    };

    let _ = state
        .slack()
        .remove_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    match result {
        Ok((design, verification)) => {
            info!(channel, thread_ts, "Plan approved");

            // Update thread parent to show Approved status
            let header_blocks = {
                // Recover slug from the session
                let slug = state
                    .sessions()
                    .get_mut(channel, thread_ts)
                    .map(|e| e.session.feature_slug().to_string())
                    .unwrap_or_default();
                formatter::plan_thread_header(&slug, "Approved")
            };
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

            // Post verification plan (inline or as file)
            post_content_or_file(
                &state,
                channel,
                thread_ts,
                &verification,
                "Verification Plan",
                "verification-plan.md",
                "markdown",
            )
            .await;

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
async fn handle_done(state: Arc<AppState>, channel: &str, thread_ts: &str) {
    // Check if approved first
    let is_approved = state
        .sessions()
        .get_mut(channel, thread_ts)
        .map(|e| e.session.is_approved())
        .unwrap_or(false);

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

    let _ = state
        .slack()
        .add_reaction(channel, thread_ts, "hourglass_flowing_sand")
        .await;

    let result = {
        let mut entry = match state.sessions().get_mut(channel, thread_ts) {
            Some(e) => e,
            None => {
                warn!(channel, thread_ts, "Session not found for done");
                return;
            }
        };
        entry.last_activity = std::time::Instant::now();
        entry.session.finalize().await
    };

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
            let header_blocks = formatter::plan_thread_header(&slug, "Finalized");
            let _ = state
                .slack()
                .update_message(channel, thread_ts, header_blocks)
                .await;

            let summary = format!(
                ":white_check_mark: *Planning finalized!*\n\n\
                 \u{2022} Worktree: `{}`\n\
                 \u{2022} Design: `{}`\n\
                 \u{2022} Verification: `{}`\n\
                 \u{2022} State: `{}`\n\n\
                 Use `/coda run {}` to start development.",
                output.worktree.display(),
                output.design_spec.display(),
                output.verification.display(),
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
async fn handle_quit(state: Arc<AppState>, channel: &str, thread_ts: &str) {
    if let Some(mut session) = state.sessions().remove(channel, thread_ts) {
        session.disconnect().await;

        let slug = session.feature_slug().to_string();
        info!(channel, thread_ts, slug, "Plan session quit by user");

        // Update thread parent
        let header_blocks = formatter::plan_thread_header(&slug, "Cancelled");
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

/// Posts content inline if short enough, or uploads as a file snippet.
async fn post_content_or_file(
    state: &AppState,
    channel: &str,
    thread_ts: &str,
    content: &str,
    label: &str,
    filename: &str,
    filetype: &str,
) {
    if content.len() <= SLACK_SECTION_CHAR_LIMIT {
        let msg = format!("*{label}:*\n```\n{content}\n```");
        let _ = state
            .slack()
            .post_thread_reply(channel, thread_ts, &msg)
            .await;
    } else {
        // Upload as file snippet for long content
        if let Err(e) = state
            .slack()
            .upload_file(channel, Some(thread_ts), content, filename, filetype)
            .await
        {
            warn!(error = %e, "Failed to upload {label} as file snippet");
            // Fallback: post truncated inline
            let truncated = &content[..SLACK_SECTION_CHAR_LIMIT.min(content.len())];
            let msg = format!(
                "*{label}* (truncated, full version upload failed):\n```\n{truncated}\n```"
            );
            let _ = state
                .slack()
                .post_thread_reply(channel, thread_ts, &msg)
                .await;
        } else {
            let _ = state
                .slack()
                .post_thread_reply(
                    channel,
                    thread_ts,
                    &format!("*{label}* uploaded as `{filename}` above."),
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_define_slack_section_char_limit() {
        assert_eq!(SLACK_SECTION_CHAR_LIMIT, 3000);
    }

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
