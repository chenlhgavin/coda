//! DashMap-based plan session manager with idle reaping.
//!
//! [`SessionManager`] tracks active [`PlanSession`](coda_core::PlanSession)
//! instances keyed by `(channel_id, thread_ts)`. A background reaper task
//! disconnects sessions that have been idle longer than a configurable
//! threshold (default 60 minutes). When sessions are reaped, the caller
//! receives the affected thread keys so it can notify users via Slack.

use std::sync::Arc;
use std::time::{Duration, Instant};

use coda_core::PlanSession;
use dashmap::DashMap;
use tracing::{debug, info};

/// How long expired thread keys are remembered for reply-after-expiry detection.
const EXPIRED_THREAD_TTL: Duration = Duration::from_secs(7200);

/// Tracks active plan sessions keyed by `(channel_id, thread_ts)`.
///
/// Uses [`DashMap`] for lock-free concurrent access. Each session
/// records its last activity timestamp so the background reaper can
/// disconnect idle sessions. Recently expired thread keys are remembered
/// so that late replies can receive a helpful error instead of silence.
///
/// # Examples
///
/// ```
/// use coda_server::session::SessionManager;
///
/// let manager = SessionManager::new();
/// assert_eq!(manager.len(), 0);
/// ```
#[derive(Debug)]
pub struct SessionManager {
    sessions: DashMap<(String, String), ActiveSession>,
    expired_threads: DashMap<(String, String), Instant>,
}

/// An active plan session with its last activity timestamp.
///
/// The `PlanSession` is wrapped in `Arc<tokio::sync::Mutex<_>>` so that
/// async operations (which may run for minutes) can hold the session mutex
/// without holding the DashMap shard lock. This prevents one long-running
/// planning turn from blocking all other session lookups, the idle reaper,
/// and graceful shutdown.
pub(crate) struct ActiveSession {
    /// The underlying planning session from coda-core, wrapped in an
    /// async-aware mutex so callers can hold it across `.await` points
    /// without holding the DashMap shard lock.
    pub session: Arc<tokio::sync::Mutex<PlanSession>>,
    /// Timestamp of the last user interaction with this session.
    pub last_activity: Instant,
    /// Whether a pre-timeout warning has already been sent for this session.
    pub warned: bool,
}

impl std::fmt::Debug for ActiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let session_debug: String = match self.session.try_lock() {
            Ok(s) => format!("{s:?}"),
            Err(_) => "<locked>".to_string(),
        };
        f.debug_struct("ActiveSession")
            .field("session", &session_debug)
            .field("idle_secs", &self.last_activity.elapsed().as_secs())
            .field("warned", &self.warned)
            .finish()
    }
}

impl SessionManager {
    /// Creates a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            expired_threads: DashMap::new(),
        }
    }

    /// Inserts a new active session for the given channel and thread.
    ///
    /// If a session already existed for this key, the old one is replaced
    /// (the caller should disconnect it first). Also removes the key from
    /// the expired threads cache if present (session was re-created).
    pub fn insert(&self, channel: String, thread_ts: String, session: PlanSession) {
        debug!(channel, thread_ts, "Inserting plan session");
        self.expired_threads
            .remove(&(channel.clone(), thread_ts.clone()));
        self.sessions.insert(
            (channel, thread_ts),
            ActiveSession {
                session: Arc::new(tokio::sync::Mutex::new(session)),
                last_activity: Instant::now(),
                warned: false,
            },
        );
    }

    /// Returns a clone of the session `Arc` and updates activity metadata.
    ///
    /// Briefly holds the DashMap shard lock to clone the `Arc` and reset
    /// `last_activity` / `warned`. The shard lock is released when this
    /// method returns — the caller can then lock the `tokio::sync::Mutex`
    /// for arbitrarily long async work without blocking other sessions
    /// or the idle reaper.
    pub(crate) fn acquire(
        &self,
        channel: &str,
        thread_ts: &str,
    ) -> Option<Arc<tokio::sync::Mutex<PlanSession>>> {
        let mut entry = self
            .sessions
            .get_mut(&(channel.to_string(), thread_ts.to_string()))?;
        entry.last_activity = Instant::now();
        entry.warned = false;
        Some(Arc::clone(&entry.session))
    }

    /// Returns `true` if a session exists for the given channel and thread.
    pub fn contains(&self, channel: &str, thread_ts: &str) -> bool {
        self.sessions
            .contains_key(&(channel.to_string(), thread_ts.to_string()))
    }

    /// Removes and returns the session `Arc` for the given channel and thread.
    ///
    /// The caller receives the `Arc<tokio::sync::Mutex<PlanSession>>` and
    /// should lock it to disconnect or perform final operations.
    pub fn remove(
        &self,
        channel: &str,
        thread_ts: &str,
    ) -> Option<Arc<tokio::sync::Mutex<PlanSession>>> {
        self.sessions
            .remove(&(channel.to_string(), thread_ts.to_string()))
            .map(|(_, active)| active.session)
    }

    /// Returns the number of active sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns `true` if there are no active sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Disconnects and removes ALL active sessions. Returns the count of
    /// disconnected sessions.
    ///
    /// Used during graceful shutdown to clean up `ClaudeClient` subprocesses
    /// and background tasks before the runtime exits.
    pub async fn disconnect_all(&self) -> usize {
        let all_keys: Vec<(String, String)> = self
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        let count = all_keys.len();
        for key in all_keys {
            if let Some((_, active)) = self.sessions.remove(&key) {
                info!(
                    channel = key.0,
                    thread_ts = key.1,
                    "Disconnecting plan session"
                );
                let mut session = active.session.lock().await;
                session.disconnect().await;
            }
        }

        count
    }

    /// Disconnects and removes sessions that have been idle longer than
    /// `max_idle`. Returns the list of reaped `(channel, thread_ts)` keys
    /// so callers can notify users.
    ///
    /// Reaped keys are added to the expired threads cache so that late
    /// replies can be detected with [`was_expired`](Self::was_expired).
    ///
    /// This method collects idle session keys first, then removes them
    /// one by one to avoid holding the map lock during async disconnect.
    pub async fn reap_idle(&self, max_idle: Duration) -> Vec<(String, String)> {
        // Collect keys of idle sessions
        let idle_keys: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().last_activity.elapsed() > max_idle)
            .map(|entry| entry.key().clone())
            .collect();

        let mut reaped = Vec::with_capacity(idle_keys.len());
        for key in idle_keys {
            if let Some((_, active)) = self.sessions.remove(&key) {
                info!(
                    channel = key.0,
                    thread_ts = key.1,
                    idle_secs = active.last_activity.elapsed().as_secs(),
                    "Reaping idle plan session"
                );
                let mut session = active.session.lock().await;
                session.disconnect().await;
                self.expired_threads.insert(key.clone(), Instant::now());
                reaped.push(key);
            }
        }

        if !reaped.is_empty() {
            info!(count = reaped.len(), "Reaped idle plan sessions");
        }

        // Purge stale entries from the expired threads cache
        self.expired_threads
            .retain(|_, inserted| inserted.elapsed() < EXPIRED_THREAD_TTL);

        reaped
    }

    /// Collects sessions approaching the idle threshold and marks them as
    /// warned. Returns `(channel, thread_ts, remaining_secs)` for each
    /// session that should receive a pre-timeout warning.
    ///
    /// A session is only returned once; subsequent calls will not include
    /// sessions that have already been warned unless new activity resets
    /// the warning flag.
    pub fn warn_approaching(
        &self,
        max_idle: Duration,
        warn_before: Duration,
    ) -> Vec<(String, String, u64)> {
        let warn_threshold = max_idle.saturating_sub(warn_before);
        let mut warnings = Vec::new();

        for mut entry in self.sessions.iter_mut() {
            let idle = entry.value().last_activity.elapsed();
            if idle >= warn_threshold && idle < max_idle && !entry.value().warned {
                let remaining = max_idle.saturating_sub(idle).as_secs();
                warnings.push((entry.key().0.clone(), entry.key().1.clone(), remaining));
                entry.value_mut().warned = true;
            }
        }

        warnings
    }

    /// Returns `true` if the given thread had a plan session that was
    /// recently expired by the reaper (within [`EXPIRED_THREAD_TTL`]).
    pub fn was_expired(&self, channel: &str, thread_ts: &str) -> bool {
        self.expired_threads
            .contains_key(&(channel.to_string(), thread_ts.to_string()))
    }

    /// Removes a thread from the expired cache (e.g., after notifying
    /// the user once about the expiry on a late reply).
    pub fn clear_expired(&self, channel: &str, thread_ts: &str) {
        self.expired_threads
            .remove(&(channel.to_string(), thread_ts.to_string()));
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_should_create_empty_session_manager() {
        let manager = SessionManager::new();
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn test_should_default_session_manager() {
        let manager = SessionManager::default();
        assert!(manager.is_empty());
    }

    #[test]
    fn test_should_report_contains_false_for_missing_session() {
        let manager = SessionManager::new();
        assert!(!manager.contains("C123", "1234567890.123456"));
    }

    #[test]
    fn test_should_remove_returns_none_for_missing_session() {
        let manager = SessionManager::new();
        assert!(manager.remove("C123", "1234567890.123456").is_none());
    }

    #[test]
    fn test_should_debug_print_session_manager() {
        let manager = SessionManager::new();
        let debug = format!("{manager:?}");
        assert!(debug.contains("SessionManager"));
    }

    #[tokio::test]
    async fn test_should_reap_idle_returns_empty_when_no_sessions() {
        let manager = SessionManager::new();
        let reaped = manager.reap_idle(Duration::from_secs(1800)).await;
        assert!(reaped.is_empty());
    }

    #[tokio::test]
    async fn test_should_disconnect_all_returns_zero_when_empty() {
        let manager = SessionManager::new();
        let count = manager.disconnect_all().await;
        assert_eq!(count, 0);
    }

    #[test]
    fn test_should_report_was_expired_false_initially() {
        let manager = SessionManager::new();
        assert!(!manager.was_expired("C123", "1234567890.123456"));
    }

    #[test]
    fn test_should_clear_expired_thread() {
        let manager = SessionManager::new();
        manager
            .expired_threads
            .insert(("C123".into(), "ts1".into()), Instant::now());
        assert!(manager.was_expired("C123", "ts1"));
        manager.clear_expired("C123", "ts1");
        assert!(!manager.was_expired("C123", "ts1"));
    }

    #[test]
    fn test_should_warn_approaching_returns_empty_when_no_sessions() {
        let manager = SessionManager::new();
        let warnings =
            manager.warn_approaching(Duration::from_secs(3600), Duration::from_secs(300));
        assert!(warnings.is_empty());
    }
}
