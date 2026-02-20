//! Shared application state for the coda-server.
//!
//! [`AppState`] is the central state container passed (as `Arc<AppState>`) to
//! all handlers. [`BindingStore`] manages channel-to-repository mappings with
//! concurrent access via [`DashMap`] and persistence to the config file.
//!
//! Some `BindingStore` methods (e.g., `get`, `len`, `is_empty`) are part of
//! the planned interface and will be called in Phases 2-4 when commands like
//! `list`, `status`, and `run` need to resolve channel bindings.

use std::collections::HashMap;
use std::path::PathBuf;

use dashmap::DashMap;
use tracing::{debug, info};

use crate::config::ServerConfig;
use crate::error::ServerError;
use crate::slack_client::SlackClient;

/// Shared application state, passed as `Arc<AppState>` to all handlers.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use coda_server::state::{AppState, BindingStore};
/// use coda_server::slack_client::SlackClient;
///
/// let slack = SlackClient::new("xoxb-test".into());
/// let bindings = BindingStore::new(PathBuf::from("/tmp/config.yml"), Default::default());
/// let state = AppState::new(slack, bindings);
/// assert!(state.bindings().get("nonexistent").is_none());
/// ```
#[derive(Debug)]
pub struct AppState {
    slack: SlackClient,
    bindings: BindingStore,
}

impl AppState {
    /// Creates a new application state with the given Slack client and bindings.
    pub fn new(slack: SlackClient, bindings: BindingStore) -> Self {
        Self { slack, bindings }
    }

    /// Returns a reference to the Slack Web API client.
    pub fn slack(&self) -> &SlackClient {
        &self.slack
    }

    /// Returns a reference to the channel-repo binding store.
    pub fn bindings(&self) -> &BindingStore {
        &self.bindings
    }
}

/// Manages channel_id to repo_path mappings.
///
/// Uses [`DashMap`] for lock-free concurrent reads/writes. Persists bindings
/// to `~/.coda-server/config.yml` on every mutation.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use coda_server::state::BindingStore;
///
/// let store = BindingStore::new(PathBuf::from("/tmp/config.yml"), Default::default());
/// assert!(store.get("C123").is_none());
/// ```
pub struct BindingStore {
    config_path: PathBuf,
    bindings: DashMap<String, PathBuf>,
}

#[allow(dead_code)] // `get`, `len`, `is_empty` used in Phases 2-4 for binding resolution
impl BindingStore {
    /// Creates a new binding store with the given config path and initial bindings.
    ///
    /// The `initial` map is typically loaded from the `bindings` section of
    /// [`ServerConfig`](crate::config::ServerConfig).
    pub fn new(config_path: PathBuf, initial: HashMap<String, String>) -> Self {
        let bindings = DashMap::with_capacity(initial.len());
        for (channel_id, repo_path) in initial {
            bindings.insert(channel_id, PathBuf::from(repo_path));
        }
        Self {
            config_path,
            bindings,
        }
    }

    /// Returns the repo path bound to the given channel, if any.
    pub fn get(&self, channel_id: &str) -> Option<PathBuf> {
        self.bindings.get(channel_id).map(|r| r.value().clone())
    }

    /// Binds a channel to a repository path.
    ///
    /// Validates that the path exists and is a directory before binding.
    /// Persists the change to the config file.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::Config` if the path does not exist, is not a
    /// directory, or if persistence fails.
    pub fn set(&self, channel_id: &str, repo_path: PathBuf) -> Result<(), ServerError> {
        if !repo_path.is_dir() {
            return Err(ServerError::Config(format!(
                "Path '{}' does not exist or is not a directory",
                repo_path.display()
            )));
        }
        info!(
            channel_id,
            repo_path = %repo_path.display(),
            "Binding channel to repository"
        );
        self.bindings.insert(channel_id.to_string(), repo_path);
        self.persist()
    }

    /// Removes a channel binding.
    ///
    /// Returns `true` if a binding was removed, `false` if the channel had
    /// no binding. Persists the change to the config file when a binding
    /// was removed.
    ///
    /// # Errors
    ///
    /// Returns `ServerError::Config` if persistence fails.
    pub fn remove(&self, channel_id: &str) -> Result<bool, ServerError> {
        let removed = self.bindings.remove(channel_id).is_some();
        if removed {
            info!(channel_id, "Removed channel binding");
            self.persist()?;
        }
        Ok(removed)
    }

    /// Returns the number of active bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Returns `true` if there are no active bindings.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Persists current bindings to the config file.
    ///
    /// Reads the existing config, updates only the `bindings` section,
    /// and writes it back to preserve other configuration values.
    fn persist(&self) -> Result<(), ServerError> {
        let content = std::fs::read_to_string(&self.config_path).map_err(|e| {
            ServerError::Config(format!(
                "Cannot read config at {}: {e}",
                self.config_path.display()
            ))
        })?;
        let mut config: ServerConfig = serde_yaml::from_str(&content).map_err(|e| {
            ServerError::Config(format!(
                "Invalid config YAML at {}: {e}",
                self.config_path.display()
            ))
        })?;

        config.bindings = self
            .bindings
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().display().to_string()))
            .collect();

        let yaml = serde_yaml::to_string(&config)
            .map_err(|e| ServerError::Config(format!("Cannot serialize config: {e}")))?;
        std::fs::write(&self.config_path, yaml).map_err(|e| {
            ServerError::Config(format!(
                "Cannot write config at {}: {e}",
                self.config_path.display()
            ))
        })?;

        debug!(
            path = %self.config_path.display(),
            count = self.bindings.len(),
            "Persisted bindings to config"
        );
        Ok(())
    }
}

impl std::fmt::Debug for BindingStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingStore")
            .field("config_path", &self.config_path)
            .field("binding_count", &self.bindings.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use super::*;

    fn write_test_config(path: &Path) {
        let config =
            "slack:\n  app_token: \"xapp-1-test\"\n  bot_token: \"xoxb-test\"\nbindings: {}\n";
        std::fs::write(path, config).expect("write test config");
    }

    #[test]
    fn test_should_create_empty_binding_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let store = BindingStore::new(config_path, HashMap::new());
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_should_create_binding_store_with_initial_bindings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let initial = HashMap::from([("C001".into(), "/tmp".into())]);
        let store = BindingStore::new(config_path, initial);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("C001"), Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn test_should_set_and_get_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let repo_path = tmp.path().to_path_buf();
        let store = BindingStore::new(config_path, HashMap::new());
        store.set("C123", repo_path.clone()).expect("set");
        assert_eq!(store.get("C123"), Some(repo_path));
    }

    #[test]
    fn test_should_reject_nonexistent_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let store = BindingStore::new(config_path, HashMap::new());
        let result = store.set("C123", PathBuf::from("/nonexistent/path"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn test_should_remove_existing_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let repo_path = tmp.path().to_path_buf();
        let store = BindingStore::new(config_path, HashMap::new());
        store.set("C123", repo_path).expect("set");

        let removed = store.remove("C123").expect("remove");
        assert!(removed);
        assert!(store.get("C123").is_none());
    }

    #[test]
    fn test_should_return_false_when_removing_nonexistent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let store = BindingStore::new(config_path, HashMap::new());
        let removed = store.remove("C999").expect("remove");
        assert!(!removed);
    }

    #[test]
    fn test_should_persist_binding_to_config_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let repo_path = tmp.path().to_path_buf();
        let store = BindingStore::new(config_path.clone(), HashMap::new());
        store.set("C123", repo_path.clone()).expect("set");

        let content = std::fs::read_to_string(&config_path).expect("read");
        let config: ServerConfig = serde_yaml::from_str(&content).expect("parse");
        assert_eq!(config.bindings.len(), 1);
        assert_eq!(config.bindings["C123"], repo_path.display().to_string());
    }

    #[test]
    fn test_should_return_none_for_unbound_channel() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let store = BindingStore::new(config_path, HashMap::new());
        assert!(store.get("C999").is_none());
    }

    #[test]
    fn test_should_overwrite_existing_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.yml");
        write_test_config(&config_path);

        let store = BindingStore::new(config_path, HashMap::new());
        let path1 = tmp.path().to_path_buf();
        store.set("C123", path1).expect("set first");

        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("mkdir");
        store.set("C123", sub.clone()).expect("set second");

        assert_eq!(store.get("C123"), Some(sub));
    }

    #[test]
    fn test_should_create_app_state() {
        let slack = SlackClient::new("xoxb-test".into());
        let bindings = BindingStore::new(PathBuf::from("/tmp/config.yml"), HashMap::new());
        let state = AppState::new(slack, bindings);
        assert!(state.bindings().is_empty());
    }

    #[test]
    fn test_should_debug_print_binding_store() {
        let store = BindingStore::new(PathBuf::from("/tmp/config.yml"), HashMap::new());
        let debug = format!("{store:?}");
        assert!(debug.contains("BindingStore"));
        assert!(debug.contains("binding_count"));
    }
}
