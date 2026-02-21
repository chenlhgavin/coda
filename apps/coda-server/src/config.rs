//! Server configuration types and loading.
//!
//! Defines [`ServerConfig`] which is loaded from `~/.coda-server/config.yml`.
//! Contains Slack API tokens and channel-to-repository bindings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::ServerError;

/// Top-level server configuration loaded from `~/.coda-server/config.yml`.
///
/// # Examples
///
/// ```
/// use coda_server::config::ServerConfig;
///
/// let yaml = r#"
/// slack:
///   app_token: "xapp-1-test"
///   bot_token: "xoxb-test"
/// bindings:
///   C123: "/path/to/repo"
/// workspace: "/home/user/workspace"
/// "#;
///
/// let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
/// assert_eq!(config.slack.app_token, "xapp-1-test");
/// assert_eq!(config.bindings.len(), 1);
/// assert_eq!(config.workspace.as_deref(), Some("/home/user/workspace"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Slack API token configuration.
    pub slack: SlackConfig,

    /// Channel-to-repository path bindings (channel_id → repo_path).
    #[serde(default)]
    pub bindings: HashMap<String, String>,

    /// Base workspace directory for cloning GitHub repositories.
    ///
    /// When set, `/coda repos` clones into `<workspace>/github.com/<owner>/<repo>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// Slack API token configuration.
///
/// # Examples
///
/// ```
/// use coda_server::config::SlackConfig;
///
/// let yaml = r#"
/// app_token: "xapp-1-test"
/// bot_token: "xoxb-test"
/// "#;
///
/// let config: SlackConfig = serde_yaml::from_str(yaml).unwrap();
/// assert!(config.app_token.starts_with("xapp-"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// App-level token for Socket Mode (`xapp-...`).
    pub app_token: String,

    /// Bot User OAuth Token for Web API calls (`xoxb-...`).
    pub bot_token: String,
}

/// Returns the default configuration directory path (`~/.coda-server/`).
///
/// # Errors
///
/// Returns `ServerError::Config` if the `HOME` environment variable is not set.
pub fn default_config_dir() -> Result<PathBuf, ServerError> {
    let home = std::env::var("HOME")
        .map_err(|_| ServerError::Config("HOME environment variable not set".into()))?;
    Ok(PathBuf::from(home).join(".coda-server"))
}

/// Returns the default configuration file path (`~/.coda-server/config.yml`).
///
/// # Errors
///
/// Returns `ServerError::Config` if the `HOME` environment variable is not set.
pub fn default_config_path() -> Result<PathBuf, ServerError> {
    Ok(default_config_dir()?.join("config.yml"))
}

impl ServerConfig {
    /// Loads configuration from the given YAML file path.
    ///
    /// Validates that the required Slack tokens are non-empty after loading.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::Config` if the file cannot be read, contains
    /// invalid YAML, or has empty token values.
    pub fn load(path: &Path) -> Result<Self, ServerError> {
        info!(path = %path.display(), "Loading configuration");
        let content = std::fs::read_to_string(path).map_err(|e| {
            ServerError::Config(format!("Cannot read config at {}: {e}", path.display()))
        })?;
        let config: Self = serde_yaml::from_str(&content).map_err(|e| {
            ServerError::Config(format!("Invalid YAML in config at {}: {e}", path.display()))
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Validates that required fields are present and well-formed.
    fn validate(&self) -> Result<(), ServerError> {
        if self.slack.app_token.is_empty() {
            return Err(ServerError::Config(
                "slack.app_token must not be empty".into(),
            ));
        }
        if !self.slack.app_token.starts_with("xapp-") {
            return Err(ServerError::Config(
                "slack.app_token must start with 'xapp-'".into(),
            ));
        }
        if self.slack.bot_token.is_empty() {
            return Err(ServerError::Config(
                "slack.bot_token must not be empty".into(),
            ));
        }
        if !self.slack.bot_token.starts_with("xoxb-") {
            return Err(ServerError::Config(
                "slack.bot_token must start with 'xoxb-'".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_deserialize_full_config() {
        let yaml = r#"
slack:
  app_token: "xapp-1-A123-456"
  bot_token: "xoxb-789-012"
bindings:
  C001: "/home/user/project-a"
  C002: "/home/user/project-b"
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.slack.app_token, "xapp-1-A123-456");
        assert_eq!(config.slack.bot_token, "xoxb-789-012");
        assert_eq!(config.bindings.len(), 2);
        assert_eq!(config.bindings["C001"], "/home/user/project-a");
    }

    #[test]
    fn test_should_deserialize_config_without_bindings() {
        let yaml = r#"
slack:
  app_token: "xapp-1-test"
  bot_token: "xoxb-test"
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert!(config.bindings.is_empty());
    }

    #[test]
    fn test_should_validate_valid_config() {
        let config = ServerConfig {
            slack: SlackConfig {
                app_token: "xapp-1-test".into(),
                bot_token: "xoxb-test".into(),
            },
            bindings: HashMap::new(),
            workspace: None,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_should_reject_empty_app_token() {
        let config = ServerConfig {
            slack: SlackConfig {
                app_token: String::new(),
                bot_token: "xoxb-test".into(),
            },
            bindings: HashMap::new(),
            workspace: None,
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("app_token"));
    }

    #[test]
    fn test_should_reject_invalid_app_token_prefix() {
        let config = ServerConfig {
            slack: SlackConfig {
                app_token: "wrong-prefix".into(),
                bot_token: "xoxb-test".into(),
            },
            bindings: HashMap::new(),
            workspace: None,
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("xapp-"));
    }

    #[test]
    fn test_should_reject_invalid_bot_token_prefix() {
        let config = ServerConfig {
            slack: SlackConfig {
                app_token: "xapp-1-test".into(),
                bot_token: "wrong-prefix".into(),
            },
            bindings: HashMap::new(),
            workspace: None,
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("xoxb-"));
    }

    #[test]
    fn test_should_load_from_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.yml");
        std::fs::write(
            &path,
            r#"
slack:
  app_token: "xapp-1-test"
  bot_token: "xoxb-test"
bindings: {}
"#,
        )
        .expect("write config");

        let config = ServerConfig::load(&path).expect("load");
        assert_eq!(config.slack.app_token, "xapp-1-test");
    }

    #[test]
    fn test_should_error_on_missing_file() {
        let result = ServerConfig::load(Path::new("/nonexistent/config.yml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot read"));
    }

    #[test]
    fn test_should_round_trip_yaml() {
        let config = ServerConfig {
            slack: SlackConfig {
                app_token: "xapp-1-test".into(),
                bot_token: "xoxb-test".into(),
            },
            bindings: HashMap::from([("C001".into(), "/repo".into())]),
            workspace: Some("/home/user/workspace".into()),
        };
        let yaml = serde_yaml::to_string(&config).expect("serialize");
        let deserialized: ServerConfig = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(deserialized.slack.app_token, config.slack.app_token);
        assert_eq!(deserialized.bindings.len(), 1);
        assert_eq!(
            deserialized.workspace.as_deref(),
            Some("/home/user/workspace")
        );
    }

    #[test]
    fn test_should_deserialize_config_with_workspace() {
        let yaml = r#"
slack:
  app_token: "xapp-1-test"
  bot_token: "xoxb-test"
workspace: "/home/user/workspace"
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.workspace.as_deref(), Some("/home/user/workspace"));
    }

    #[test]
    fn test_should_deserialize_config_without_workspace() {
        let yaml = r#"
slack:
  app_token: "xapp-1-test"
  bot_token: "xoxb-test"
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert!(config.workspace.is_none());
    }
}
