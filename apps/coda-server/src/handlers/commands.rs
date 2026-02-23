//! Slash command parsing and dispatch.
//!
//! Parses the text from `/coda <text>` into a [`CodaCommand`] enum and
//! routes to the appropriate command handler. Errors are reported back
//! to the user as Slack messages.

use std::sync::Arc;

use serde::Deserialize;
use tracing::{info, instrument, warn};

use crate::commands;
use crate::error::ServerError;
use crate::state::AppState;

/// Parsed subcommand from `/coda <text>`.
///
/// # Examples
///
/// ```
/// use coda_server::handlers::commands::CodaCommand;
///
/// let cmd = CodaCommand::parse("repos").unwrap();
/// assert!(matches!(cmd, CodaCommand::Repos));
///
/// let cmd = CodaCommand::parse("").unwrap();
/// assert!(matches!(cmd, CodaCommand::Help));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodaCommand {
    /// Initialize the bound repository as a CODA project.
    Init {
        /// Whether to reinitialize (update config and regenerate docs).
        force: bool,
    },

    /// Start an interactive planning session for a feature.
    Plan {
        /// Feature slug identifier.
        feature_slug: String,
    },

    /// Execute a feature development run.
    Run {
        /// Feature slug identifier.
        feature_slug: String,
    },

    /// List all features in the bound repository.
    List,

    /// Show status of a specific feature.
    Status {
        /// Feature slug identifier.
        feature_slug: String,
    },

    /// Clean up merged worktrees.
    Clean,

    /// List GitHub repos and select one to clone and bind.
    Repos,

    /// Switch the bound repository to a different branch.
    Switch {
        /// Target branch name.
        branch: String,
    },

    /// Show available commands.
    Help,
}

impl CodaCommand {
    /// Parses the text portion of a `/coda` slash command into a command.
    ///
    /// Empty text or unrecognized subcommands default to `Help`.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::Dispatch` if a required argument is missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use coda_server::handlers::commands::CodaCommand;
    ///
    /// assert!(matches!(CodaCommand::parse("help"), Ok(CodaCommand::Help)));
    /// assert!(matches!(CodaCommand::parse("list"), Ok(CodaCommand::List)));
    /// assert!(matches!(CodaCommand::parse("repos"), Ok(CodaCommand::Repos)));
    /// ```
    pub fn parse(text: &str) -> Result<Self, ServerError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Self::Help);
        }

        let mut parts = text.splitn(2, ' ');
        let subcommand = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        match subcommand {
            "init" => {
                let force = rest == "--force" || rest == "-f";
                Ok(Self::Init { force })
            }
            "plan" => {
                if rest.is_empty() {
                    return Err(ServerError::Dispatch(
                        "Usage: `/coda plan <feature-slug>` — provide a feature slug".into(),
                    ));
                }
                Ok(Self::Plan {
                    feature_slug: rest.to_string(),
                })
            }
            "run" => {
                if rest.is_empty() {
                    return Err(ServerError::Dispatch(
                        "Usage: `/coda run <feature-slug>` — provide a feature slug".into(),
                    ));
                }
                Ok(Self::Run {
                    feature_slug: rest.to_string(),
                })
            }
            "list" => Ok(Self::List),
            "status" => {
                if rest.is_empty() {
                    return Err(ServerError::Dispatch(
                        "Usage: `/coda status <feature-slug>` — provide a feature slug".into(),
                    ));
                }
                Ok(Self::Status {
                    feature_slug: rest.to_string(),
                })
            }
            "clean" => Ok(Self::Clean),
            "repos" => Ok(Self::Repos),
            "switch" => {
                if rest.is_empty() {
                    return Err(ServerError::Dispatch(
                        "Usage: `/coda switch <branch>` — provide a branch name".into(),
                    ));
                }
                Ok(Self::Switch {
                    branch: rest.to_string(),
                })
            }
            "help" => Ok(Self::Help),
            _ => Ok(Self::Help),
        }
    }
}

/// Slash command payload from Slack Socket Mode.
///
/// Contains metadata about the command invocation including the channel,
/// user, and the raw command text. All fields must be present for serde
/// deserialization from the Slack API payload even if not directly read
/// by all command handlers.
#[derive(Debug, Clone, Deserialize)]
pub struct SlashCommandPayload {
    /// The slash command name (e.g., `/coda`).
    #[allow(dead_code)] // Required for serde deserialization from Slack API
    pub command: String,

    /// The text after the command (e.g., `"bind /path/to/repo"`).
    #[serde(default)]
    pub text: String,

    /// Channel where the command was invoked.
    pub channel_id: String,

    /// Human-readable channel name.
    #[serde(default)]
    #[allow(dead_code)] // Required for serde deserialization from Slack API
    pub channel_name: String,

    /// ID of the user who invoked the command.
    #[allow(dead_code)] // Required for serde deserialization from Slack API
    pub user_id: String,

    /// Username of the invoker.
    #[serde(default)]
    pub user_name: String,
}

/// Handles a slash command envelope payload.
///
/// Parses the payload, extracts the command text, and routes to the
/// appropriate command handler. Errors are posted back to the channel.
#[instrument(skip(state, payload), fields(channel, user, command_text))]
pub async fn handle_slash_command(state: Arc<AppState>, payload: serde_json::Value) {
    let cmd_payload: SlashCommandPayload = match serde_json::from_value(payload) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse slash command payload");
            return;
        }
    };

    // Record parsed fields onto the current span for downstream correlation
    let span = tracing::Span::current();
    span.record("channel", cmd_payload.channel_id.as_str());
    span.record("user", cmd_payload.user_name.as_str());
    span.record("command_text", cmd_payload.text.as_str());

    info!(
        user = cmd_payload.user_name,
        channel = cmd_payload.channel_id,
        text = cmd_payload.text,
        "Received slash command"
    );

    let command = match CodaCommand::parse(&cmd_payload.text) {
        Ok(cmd) => cmd,
        Err(e) => {
            post_error(&state, &cmd_payload.channel_id, &e.to_string()).await;
            return;
        }
    };

    let result = match command {
        CodaCommand::Init { force } => {
            commands::init::handle_init(Arc::clone(&state), &cmd_payload, force).await
        }
        CodaCommand::Plan { feature_slug } => {
            commands::plan::handle_plan(Arc::clone(&state), &cmd_payload, &feature_slug).await
        }
        CodaCommand::Run { feature_slug } => {
            commands::run::handle_run(Arc::clone(&state), &cmd_payload, &feature_slug).await
        }
        CodaCommand::Help => commands::query::handle_help(Arc::clone(&state), &cmd_payload).await,
        CodaCommand::List => commands::query::handle_list(Arc::clone(&state), &cmd_payload).await,
        CodaCommand::Status { feature_slug } => {
            commands::query::handle_status(Arc::clone(&state), &cmd_payload, &feature_slug).await
        }
        CodaCommand::Clean => commands::query::handle_clean(Arc::clone(&state), &cmd_payload).await,
        CodaCommand::Repos => commands::repos::handle_repos(Arc::clone(&state), &cmd_payload).await,
        CodaCommand::Switch { branch } => {
            commands::repos::handle_switch(Arc::clone(&state), &cmd_payload, &branch).await
        }
    };

    if let Err(e) = result {
        warn!(error = %e, "Command handler failed");
        post_error(&state, &cmd_payload.channel_id, &e.to_string()).await;
    }
}

/// Posts an error message to a channel.
async fn post_error(state: &AppState, channel: &str, message: &str) {
    let blocks = vec![serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": format!(":warning: {message}")
        }
    })];

    if let Err(e) = state.slack().post_message(channel, blocks).await {
        warn!(error = %e, channel, "Failed to post error message to Slack");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_parse_init() {
        let cmd = CodaCommand::parse("init").expect("parse");
        assert_eq!(cmd, CodaCommand::Init { force: false });
    }

    #[test]
    fn test_should_parse_init_force() {
        let cmd = CodaCommand::parse("init --force").expect("parse");
        assert_eq!(cmd, CodaCommand::Init { force: true });
    }

    #[test]
    fn test_should_parse_init_force_short() {
        let cmd = CodaCommand::parse("init -f").expect("parse");
        assert_eq!(cmd, CodaCommand::Init { force: true });
    }

    #[test]
    fn test_should_parse_plan_with_slug() {
        let cmd = CodaCommand::parse("plan add-auth").expect("parse");
        assert_eq!(
            cmd,
            CodaCommand::Plan {
                feature_slug: "add-auth".into()
            }
        );
    }

    #[test]
    fn test_should_error_on_plan_without_slug() {
        let result = CodaCommand::parse("plan");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("feature-slug"));
    }

    #[test]
    fn test_should_parse_run_with_slug() {
        let cmd = CodaCommand::parse("run add-auth").expect("parse");
        assert_eq!(
            cmd,
            CodaCommand::Run {
                feature_slug: "add-auth".into()
            }
        );
    }

    #[test]
    fn test_should_error_on_run_without_slug() {
        let result = CodaCommand::parse("run");
        assert!(result.is_err());
    }

    #[test]
    fn test_should_parse_list() {
        let cmd = CodaCommand::parse("list").expect("parse");
        assert_eq!(cmd, CodaCommand::List);
    }

    #[test]
    fn test_should_parse_status_with_slug() {
        let cmd = CodaCommand::parse("status add-auth").expect("parse");
        assert_eq!(
            cmd,
            CodaCommand::Status {
                feature_slug: "add-auth".into()
            }
        );
    }

    #[test]
    fn test_should_error_on_status_without_slug() {
        let result = CodaCommand::parse("status");
        assert!(result.is_err());
    }

    #[test]
    fn test_should_parse_clean() {
        let cmd = CodaCommand::parse("clean").expect("parse");
        assert_eq!(cmd, CodaCommand::Clean);
    }

    #[test]
    fn test_should_parse_help() {
        let cmd = CodaCommand::parse("help").expect("parse");
        assert_eq!(cmd, CodaCommand::Help);
    }

    #[test]
    fn test_should_parse_empty_as_help() {
        let cmd = CodaCommand::parse("").expect("parse");
        assert_eq!(cmd, CodaCommand::Help);
    }

    #[test]
    fn test_should_parse_whitespace_as_help() {
        let cmd = CodaCommand::parse("   ").expect("parse");
        assert_eq!(cmd, CodaCommand::Help);
    }

    #[test]
    fn test_should_parse_unknown_as_help() {
        let cmd = CodaCommand::parse("foobar").expect("parse");
        assert_eq!(cmd, CodaCommand::Help);
    }

    #[test]
    fn test_should_trim_whitespace() {
        let cmd = CodaCommand::parse("  list  ").expect("parse");
        assert_eq!(cmd, CodaCommand::List);
    }

    #[test]
    fn test_should_deserialize_slash_command_payload() {
        let json = serde_json::json!({
            "command": "/coda",
            "text": "help",
            "channel_id": "C123",
            "channel_name": "general",
            "user_id": "U123",
            "user_name": "testuser"
        });
        let payload: SlashCommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(payload.command, "/coda");
        assert_eq!(payload.text, "help");
        assert_eq!(payload.channel_id, "C123");
    }

    #[test]
    fn test_should_parse_repos() {
        let cmd = CodaCommand::parse("repos").expect("parse");
        assert_eq!(cmd, CodaCommand::Repos);
    }

    #[test]
    fn test_should_parse_repos_ignoring_extra_text() {
        let cmd = CodaCommand::parse("repos my-org").expect("parse");
        assert_eq!(cmd, CodaCommand::Repos);
    }

    #[test]
    fn test_should_parse_switch_with_branch() {
        let cmd = CodaCommand::parse("switch main").expect("parse");
        assert_eq!(
            cmd,
            CodaCommand::Switch {
                branch: "main".into()
            }
        );
    }

    #[test]
    fn test_should_error_on_switch_without_branch() {
        let result = CodaCommand::parse("switch");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("branch"));
    }

    #[test]
    fn test_should_deserialize_minimal_payload() {
        let json = serde_json::json!({
            "command": "/coda",
            "channel_id": "C123",
            "user_id": "U123"
        });
        let payload: SlashCommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(payload.text, "");
        assert_eq!(payload.channel_name, "");
    }
}
