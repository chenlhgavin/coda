//! Shared application state for the coda-server.
//!
//! [`AppState`] is the central state container passed (as `Arc<AppState>`) to
//! all handlers. [`BindingStore`] manages channel-to-repository mappings with
//! concurrent access via [`DashMap`] and persistence to the config file.
//! [`RunningTasks`] tracks in-flight init/run tasks to prevent duplicates.
//! [`SessionManager`](crate::session::SessionManager) tracks active plan sessions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dashmap::DashMap;
use tracing::{debug, info};

use crate::config::ServerConfig;
use crate::error::ServerError;
use crate::session::SessionManager;
use crate::slack_client::SlackClient;

/// Shared application state, passed as `Arc<AppState>` to all handlers.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use coda_server::state::{AppState, BindingStore, RepoLocks, RunningTasks};
/// use coda_server::session::SessionManager;
/// use coda_server::slack_client::SlackClient;
///
/// let slack = SlackClient::new("xoxb-test".into());
/// let bindings = BindingStore::new(PathBuf::from("/tmp/config.yml"), Default::default());
/// let running = RunningTasks::new();
/// let sessions = SessionManager::new();
/// let repo_locks = RepoLocks::new();
/// let state = AppState::new(slack, bindings, running, sessions, None, repo_locks);
/// assert!(state.bindings().get("nonexistent").is_none());
/// ```
#[derive(Debug)]
pub struct AppState {
    slack: SlackClient,
    bindings: BindingStore,
    running_tasks: RunningTasks,
    sessions: SessionManager,
    workspace: Option<PathBuf>,
    repo_locks: RepoLocks,
}

impl AppState {
    /// Creates a new application state with the given components.
    pub fn new(
        slack: SlackClient,
        bindings: BindingStore,
        running_tasks: RunningTasks,
        sessions: SessionManager,
        workspace: Option<PathBuf>,
        repo_locks: RepoLocks,
    ) -> Self {
        Self {
            slack,
            bindings,
            running_tasks,
            sessions,
            workspace,
            repo_locks,
        }
    }

    /// Returns a reference to the Slack Web API client.
    pub fn slack(&self) -> &SlackClient {
        &self.slack
    }

    /// Returns a reference to the channel-repo binding store.
    pub fn bindings(&self) -> &BindingStore {
        &self.bindings
    }

    /// Returns a reference to the running task tracker.
    pub fn running_tasks(&self) -> &RunningTasks {
        &self.running_tasks
    }

    /// Returns a reference to the plan session manager.
    pub fn sessions(&self) -> &SessionManager {
        &self.sessions
    }

    /// Returns the workspace directory path, if configured.
    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    /// Returns a reference to the repository lock manager.
    pub fn repo_locks(&self) -> &RepoLocks {
        &self.repo_locks
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

    /// Returns the number of active bindings.
    #[allow(dead_code)] // Used in tests; standard collection API
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Returns `true` if there are no active bindings.
    #[allow(dead_code)] // Used in tests; standard collection API
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

/// Tracks in-flight init/run tasks to prevent duplicate executions.
///
/// Keys follow the convention `"init:{channel_id}"` or `"run:{slug}"`.
/// Each entry stores a [`tokio::task::JoinHandle`] for the spawned task,
/// allowing status checks and cleanup when the task completes.
///
/// # Examples
///
/// ```
/// use coda_server::state::RunningTasks;
///
/// let tasks = RunningTasks::new();
/// assert!(!tasks.is_running("run:add-auth"));
/// ```
#[derive(Debug)]
pub struct RunningTasks {
    tasks: DashMap<String, tokio::task::JoinHandle<()>>,
}

impl RunningTasks {
    /// Creates an empty running task tracker.
    pub fn new() -> Self {
        Self {
            tasks: DashMap::new(),
        }
    }

    /// Returns `true` if a task with the given key is currently running.
    ///
    /// Also cleans up finished tasks: if the stored handle is finished,
    /// it is removed and `false` is returned.
    pub fn is_running(&self, key: &str) -> bool {
        if let Some(entry) = self.tasks.get(key) {
            if entry.value().is_finished() {
                drop(entry);
                self.tasks.remove(key);
                return false;
            }
            return true;
        }
        false
    }

    /// Inserts a running task with the given key.
    ///
    /// If a previous task existed under this key, it is replaced (the old
    /// handle is dropped but the task itself continues running).
    pub fn insert(&self, key: String, handle: tokio::task::JoinHandle<()>) {
        self.tasks.insert(key, handle);
    }

    /// Removes a task entry by key.
    pub fn remove(&self, key: &str) {
        self.tasks.remove(key);
    }
}

impl Default for RunningTasks {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages per-repository locks to prevent concurrent mutating operations.
///
/// Multiple channels can bind to the same repository directory. This lock
/// ensures that mutating operations (`init`, `switch`, `clean`, `plan finalize`)
/// do not run concurrently on the same repository.
///
/// Read-only operations (`list`, `status`) and worktree-isolated operations
/// (`run`) do not require locking.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use coda_server::state::RepoLocks;
///
/// let locks = RepoLocks::new();
/// let repo = PathBuf::from("/repos/myproject");
/// assert!(locks.try_lock(&repo, "init").is_ok());
/// assert!(locks.try_lock(&repo, "switch main").is_err());
/// locks.unlock(&repo);
/// assert!(locks.try_lock(&repo, "switch main").is_ok());
/// ```
#[derive(Debug)]
pub struct RepoLocks {
    locks: DashMap<PathBuf, String>,
}

impl RepoLocks {
    /// Creates a new, empty repository lock manager.
    pub fn new() -> Self {
        Self {
            locks: DashMap::new(),
        }
    }

    /// Attempts to acquire a lock on a repository path.
    ///
    /// Returns `Ok(())` if the lock was acquired, or `Err(description)` with
    /// the current lock holder's description if the repository is already locked.
    pub fn try_lock(&self, repo_path: &Path, description: &str) -> Result<(), String> {
        use dashmap::mapref::entry::Entry;
        match self.locks.entry(repo_path.to_path_buf()) {
            Entry::Occupied(e) => Err(e.get().clone()),
            Entry::Vacant(e) => {
                e.insert(description.to_string());
                Ok(())
            }
        }
    }

    /// Releases the lock on a repository path.
    pub fn unlock(&self, repo_path: &Path) {
        self.locks.remove(repo_path);
    }
}

impl Default for RepoLocks {
    fn default() -> Self {
        Self::new()
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
        let running = RunningTasks::new();
        let sessions = SessionManager::new();
        let repo_locks = RepoLocks::new();
        let state = AppState::new(slack, bindings, running, sessions, None, repo_locks);
        assert!(state.bindings().is_empty());
        assert!(state.sessions().is_empty());
        assert!(state.workspace().is_none());
    }

    #[test]
    fn test_should_create_app_state_with_workspace() {
        let slack = SlackClient::new("xoxb-test".into());
        let bindings = BindingStore::new(PathBuf::from("/tmp/config.yml"), HashMap::new());
        let running = RunningTasks::new();
        let sessions = SessionManager::new();
        let repo_locks = RepoLocks::new();
        let workspace = Some(PathBuf::from("/workspace"));
        let state = AppState::new(slack, bindings, running, sessions, workspace, repo_locks);
        assert_eq!(state.workspace(), Some(Path::new("/workspace")));
    }

    #[test]
    fn test_should_debug_print_binding_store() {
        let store = BindingStore::new(PathBuf::from("/tmp/config.yml"), HashMap::new());
        let debug = format!("{store:?}");
        assert!(debug.contains("BindingStore"));
        assert!(debug.contains("binding_count"));
    }

    #[test]
    fn test_should_create_empty_running_tasks() {
        let tasks = RunningTasks::new();
        assert!(!tasks.is_running("run:add-auth"));
    }

    #[test]
    fn test_should_track_running_task() {
        let tasks = RunningTasks::new();
        // Keep runtime alive so the spawned task is not canceled
        let _rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let handle = _rt.spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        tasks.insert("run:add-auth".to_string(), handle);
        assert!(tasks.is_running("run:add-auth"));
    }

    #[test]
    fn test_should_report_not_running_after_remove() {
        let tasks = RunningTasks::new();
        // Keep runtime alive so the spawned task is not canceled
        let _rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let handle = _rt.spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        tasks.insert("run:add-auth".to_string(), handle);
        tasks.remove("run:add-auth");
        assert!(!tasks.is_running("run:add-auth"));
    }

    #[test]
    fn test_should_clean_up_finished_task() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let tasks = RunningTasks::new();
        // Spawn a task that completes immediately
        let handle = rt.spawn(async {});
        // Wait for it to finish
        rt.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(10)).await });
        tasks.insert("run:done".to_string(), handle);
        // is_running should detect it's finished and clean up
        assert!(!tasks.is_running("run:done"));
    }

    #[test]
    fn test_should_default_running_tasks() {
        let tasks = RunningTasks::default();
        assert!(!tasks.is_running("anything"));
    }

    #[test]
    fn test_should_acquire_repo_lock() {
        let locks = RepoLocks::new();
        let repo = PathBuf::from("/repos/myproject");
        assert!(locks.try_lock(&repo, "init").is_ok());
    }

    #[test]
    fn test_should_reject_concurrent_repo_lock() {
        let locks = RepoLocks::new();
        let repo = PathBuf::from("/repos/myproject");
        locks.try_lock(&repo, "init").expect("first lock");
        let err = locks.try_lock(&repo, "switch main").unwrap_err();
        assert_eq!(err, "init");
    }

    #[test]
    fn test_should_release_repo_lock() {
        let locks = RepoLocks::new();
        let repo = PathBuf::from("/repos/myproject");
        locks.try_lock(&repo, "init").expect("lock");
        locks.unlock(&repo);
        assert!(locks.try_lock(&repo, "switch main").is_ok());
    }

    #[test]
    fn test_should_allow_different_repos_concurrently() {
        let locks = RepoLocks::new();
        let repo1 = PathBuf::from("/repos/project-a");
        let repo2 = PathBuf::from("/repos/project-b");
        assert!(locks.try_lock(&repo1, "init").is_ok());
        assert!(locks.try_lock(&repo2, "init").is_ok());
    }

    #[test]
    fn test_should_default_repo_locks() {
        let locks = RepoLocks::default();
        assert!(locks.try_lock(&PathBuf::from("/repo"), "test").is_ok());
    }
}
