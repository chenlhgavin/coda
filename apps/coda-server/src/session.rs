//! DashMap-based plan session manager with idle reaping.
//!
//! [`SessionManager`] tracks active [`PlanSession`](coda_core::PlanSession)
//! instances keyed by `(channel_id, thread_ts)`. A background reaper task
//! disconnects sessions that have been idle longer than a configurable
//! threshold (default 30 minutes).

use std::time::{Duration, Instant};

use coda_core::PlanSession;
use dashmap::DashMap;
use tracing::{debug, info};

/// Tracks active plan sessions keyed by `(channel_id, thread_ts)`.
///
/// Uses [`DashMap`] for lock-free concurrent access. Each session
/// records its last activity timestamp so the background reaper can
/// disconnect idle sessions.
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
}

/// An active plan session with its last activity timestamp.
pub(crate) struct ActiveSession {
    /// The underlying planning session from coda-core.
    pub session: PlanSession,
    /// Timestamp of the last user interaction with this session.
    pub last_activity: Instant,
}

impl std::fmt::Debug for ActiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveSession")
            .field("session", &self.session)
            .field("idle_secs", &self.last_activity.elapsed().as_secs())
            .finish()
    }
}

impl SessionManager {
    /// Creates a new empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    /// Inserts a new active session for the given channel and thread.
    ///
    /// If a session already existed for this key, the old one is replaced
    /// (the caller should disconnect it first).
    pub fn insert(&self, channel: String, thread_ts: String, session: PlanSession) {
        debug!(channel, thread_ts, "Inserting plan session");
        self.sessions.insert(
            (channel, thread_ts),
            ActiveSession {
                session,
                last_activity: Instant::now(),
            },
        );
    }

    /// Returns a mutable reference to the active session for the given
    /// channel and thread, if one exists.
    ///
    /// The caller receives a [`dashmap::mapref::one::RefMut`] guard that
    /// automatically releases on drop.
    pub(crate) fn get_mut(
        &self,
        channel: &str,
        thread_ts: &str,
    ) -> Option<dashmap::mapref::one::RefMut<'_, (String, String), ActiveSession>> {
        self.sessions
            .get_mut(&(channel.to_string(), thread_ts.to_string()))
    }

    /// Returns `true` if a session exists for the given channel and thread.
    pub fn contains(&self, channel: &str, thread_ts: &str) -> bool {
        self.sessions
            .contains_key(&(channel.to_string(), thread_ts.to_string()))
    }

    /// Removes and returns the session for the given channel and thread.
    pub fn remove(&self, channel: &str, thread_ts: &str) -> Option<PlanSession> {
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
            if let Some((_, mut active)) = self.sessions.remove(&key) {
                info!(
                    channel = key.0,
                    thread_ts = key.1,
                    "Disconnecting plan session"
                );
                active.session.disconnect().await;
            }
        }

        count
    }

    /// Disconnects and removes sessions that have been idle longer than
    /// `max_idle`. Returns the count of reaped sessions.
    ///
    /// This method collects idle session keys first, then removes them
    /// one by one to avoid holding the map lock during async disconnect.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize {
        // Collect keys of idle sessions
        let idle_keys: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().last_activity.elapsed() > max_idle)
            .map(|entry| entry.key().clone())
            .collect();

        let count = idle_keys.len();
        for key in idle_keys {
            if let Some((_, mut active)) = self.sessions.remove(&key) {
                info!(
                    channel = key.0,
                    thread_ts = key.1,
                    idle_secs = active.last_activity.elapsed().as_secs(),
                    "Reaping idle plan session"
                );
                active.session.disconnect().await;
            }
        }

        if count > 0 {
            info!(count, "Reaped idle plan sessions");
        }

        count
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
    async fn test_should_reap_idle_returns_zero_when_empty() {
        let manager = SessionManager::new();
        let count = manager.reap_idle(Duration::from_secs(1800)).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_should_disconnect_all_returns_zero_when_empty() {
        let manager = SessionManager::new();
        let count = manager.disconnect_all().await;
        assert_eq!(count, 0);
    }
}
