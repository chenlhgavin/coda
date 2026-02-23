//! Command implementations for CODA slash commands.
//!
//! Each submodule implements one or more related commands:
//! - [`init`] — `/coda init` with live progress updates
//! - [`plan`] — `/coda plan` with interactive Slack thread sessions
//! - [`query`] — `/coda help`, `/coda list`, `/coda status`, `/coda clean`
//! - [`repos`] — `/coda repos` and `/coda switch`
//! - [`run`] — `/coda run` with live progress updates
//!
//! The shared [`resolve_engine`] helper resolves a channel binding and
//! creates an [`Engine`](coda_core::Engine) for use in command handlers.

use std::path::PathBuf;

use coda_core::Engine;
use tracing::{debug, instrument};

use crate::error::ServerError;
use crate::formatter;
use crate::state::AppState;

pub mod init;
pub mod plan;
pub mod query;
pub mod repos;
pub mod run;
pub mod streaming;

/// Resolves the channel binding and creates an Engine for the bound repository.
///
/// If the channel has no binding, posts a user-friendly error message to the
/// channel and returns `Ok(None)`. If the engine cannot be created, returns
/// the error for the caller to propagate.
///
/// # Errors
///
/// Returns `ServerError` if the Slack API call fails (when posting the
/// "no binding" error) or if `Engine::new()` fails.
#[instrument(skip(state), fields(channel = %channel_id))]
pub(crate) async fn resolve_engine(
    state: &AppState,
    channel_id: &str,
) -> Result<Option<(PathBuf, Engine)>, ServerError> {
    let Some(repo_path) = state.bindings().get(channel_id) else {
        let blocks = formatter::error(
            "No repository bound to this channel. Use `/coda repos` to clone and bind a repository first.",
        );
        state.slack().post_message(channel_id, blocks).await?;
        return Ok(None);
    };

    let engine = Engine::new(repo_path.clone()).await?;
    debug!(repo_path = %repo_path.display(), "Engine created for channel");
    Ok(Some((repo_path, engine)))
}
