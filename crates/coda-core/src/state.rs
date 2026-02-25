//! Feature state types and persistence manager.
//!
//! Defines [`FeatureState`] (persisted to `.coda/<feature>/state.yml`),
//! [`PhaseOutcome`] (complete metrics from a phase execution), and
//! [`StateManager`] which centralizes all state mutations with
//! transition-invariant enforcement and persistence.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Complete state of a feature's execution, persisted to `state.yml`.
///
/// Tracks the progress of a feature through all execution phases,
/// enabling crash recovery by resuming from the last completed phase.
///
/// # Examples
///
/// ```
/// use coda_core::state::{FeatureState, FeatureStatus};
///
/// let yaml = r#"
/// feature:
///   slug: "add-auth"
///   created_at: "2026-02-10T10:30:00Z"
///   updated_at: "2026-02-10T10:30:00Z"
/// status: planned
/// current_phase: 0
/// git:
///   worktree_path: ".trees/add-auth"
///   branch: "feature/add-auth"
///   base_branch: "main"
/// phases: []
/// total:
///   turns: 0
///   cost_usd: 0.0
///   cost:
///     input_tokens: 0
///     output_tokens: 0
///   duration_secs: 0
/// "#;
///
/// let state: FeatureState = serde_yaml::from_str(yaml).unwrap();
/// assert_eq!(state.status, FeatureStatus::Planned);
/// assert_eq!(state.feature.slug, "add-auth");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureState {
    /// Basic feature metadata.
    pub feature: FeatureInfo,

    /// Overall feature execution status.
    pub status: FeatureStatus,

    /// Index of the current phase being executed (0-based).
    ///
    /// When using [`StateManager`], this field is automatically synced
    /// from the derived value (first non-completed phase index) before
    /// each save. Direct writes to this field are discouraged.
    pub current_phase: u32,

    /// Git branch and worktree information.
    pub git: GitInfo,

    /// Records for each execution phase.
    pub phases: Vec<PhaseRecord>,

    /// Pull request information, populated after PR creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrInfo>,

    /// Cumulative statistics across all phases.
    pub total: TotalStats,
}

/// Minimum number of phases: at least 1 dev phase + review + verify + update-docs.
const MIN_PHASE_COUNT: usize = 4;

/// Required terminal quality phases in order. The last 3 phases must always be
/// `review -> verify -> update-docs`, all with [`PhaseKind::Quality`].
const TERMINAL_QUALITY_PHASES: &[&str] = &["review", "verify", "update-docs"];

impl FeatureState {
    /// Returns the total cost already spent across all completed phases.
    ///
    /// Used to compute the remaining budget when resuming an interrupted run,
    /// so the SDK receives the correct reduced budget rather than the full
    /// configured maximum.
    ///
    /// # Examples
    ///
    /// ```
    /// use coda_core::state::*;
    ///
    /// let now = chrono::Utc::now();
    /// let state = FeatureState {
    ///     feature: FeatureInfo {
    ///         slug: "test".to_string(),
    ///         created_at: now,
    ///         updated_at: now,
    ///     },
    ///     status: FeatureStatus::Planned,
    ///     current_phase: 0,
    ///     git: GitInfo {
    ///         worktree_path: ".trees/test".into(),
    ///         branch: "feature/test".to_string(),
    ///         base_branch: "main".to_string(),
    ///     },
    ///     phases: vec![],
    ///     pr: None,
    ///     total: TotalStats::default(),
    /// };
    /// assert!((state.spent_budget() - 0.0).abs() < f64::EPSILON);
    /// ```
    pub fn spent_budget(&self) -> f64 {
        self.phases
            .iter()
            .filter(|p| p.status == PhaseStatus::Completed)
            .map(|p| p.cost_usd)
            .sum()
    }

    /// Migrates legacy state files to the current schema.
    ///
    /// Detects states created before the `update-docs` phase was introduced
    /// and appends missing terminal quality phases. After appending, sorts
    /// all quality phases into the canonical order defined by
    /// [`TERMINAL_QUALITY_PHASES`] so that misordered legacy states are
    /// corrected (e.g., `[verify, review]` → `[review, verify, update-docs]`).
    pub fn migrate(&mut self) {
        // Collect names of all quality phases anywhere in the list.
        let existing_quality_names: Vec<String> = self
            .phases
            .iter()
            .filter(|p| p.kind == PhaseKind::Quality)
            .map(|p| p.name.clone())
            .collect();

        for &expected in TERMINAL_QUALITY_PHASES {
            if !existing_quality_names.iter().any(|n| n == expected) {
                tracing::info!(
                    phase = expected,
                    "Migrating legacy state: appending missing quality phase"
                );
                self.phases.push(PhaseRecord {
                    name: expected.to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::Value::Object(serde_json::Map::new()),
                });
            }
        }

        // Sort quality phases into canonical order. Dev phases keep their
        // positions; all quality phases are collected, sorted by
        // TERMINAL_QUALITY_PHASES index, and placed after all dev phases.
        let dev_phases: Vec<PhaseRecord> = self
            .phases
            .iter()
            .filter(|p| p.kind == PhaseKind::Dev)
            .cloned()
            .collect();

        let mut quality_phases: Vec<PhaseRecord> = self
            .phases
            .iter()
            .filter(|p| p.kind == PhaseKind::Quality)
            .cloned()
            .collect();

        quality_phases.sort_by_key(|p| {
            TERMINAL_QUALITY_PHASES
                .iter()
                .position(|&name| name == p.name)
                .unwrap_or(usize::MAX)
        });

        let mut sorted = dev_phases;
        sorted.extend(quality_phases);
        self.phases = sorted;
    }

    /// Validates structural invariants after deserialization.
    ///
    /// Checks that `phases` has at least [`MIN_PHASE_COUNT`] entries
    /// (1+ dev phases + review + verify + update-docs), `current_phase`
    /// is within bounds, `worktree_path` does not contain parent-directory
    /// references, and no `Dev` phase appears after the first `Quality`
    /// phase.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error description when validation fails.
    pub fn validate(&self) -> Result<(), String> {
        if self.phases.len() < MIN_PHASE_COUNT {
            return Err(format!(
                "expected at least {MIN_PHASE_COUNT} phases \
                 (dev + review + verify + update-docs), found {}",
                self.phases.len(),
            ));
        }

        // Reject dev phases appearing after the first quality phase.
        // This check runs before the terminal quality order check so that
        // interleaved dev/quality states report the more specific error.
        let first_quality_idx = self
            .phases
            .iter()
            .position(|p| p.kind == PhaseKind::Quality);
        if let Some(quality_start) = first_quality_idx {
            for (idx, phase) in self.phases.iter().enumerate().skip(quality_start) {
                if phase.kind == PhaseKind::Dev {
                    return Err(format!(
                        "dev phase '{}' at index {} appears after first quality phase at index {}",
                        phase.name, idx, quality_start,
                    ));
                }
            }
        }

        // Enforce that the last 3 phases are the fixed quality sequence.
        let n = self.phases.len();
        for (offset, expected_name) in TERMINAL_QUALITY_PHASES.iter().enumerate() {
            let idx = n - TERMINAL_QUALITY_PHASES.len() + offset;
            let phase = &self.phases[idx];
            if phase.name != *expected_name || phase.kind != PhaseKind::Quality {
                return Err(format!(
                    "expected terminal quality phase '{}' (Quality) at index {}, \
                     found '{}' ({:?})",
                    expected_name, idx, phase.name, phase.kind,
                ));
            }
        }

        if (self.current_phase as usize) > self.phases.len() {
            return Err(format!(
                "current_phase {} exceeds phases count {}",
                self.current_phase,
                self.phases.len(),
            ));
        }

        // Reject worktree paths that escape the project root via `..`
        for component in self.git.worktree_path.components() {
            if matches!(component, Component::ParentDir) {
                return Err(format!(
                    "worktree_path '{}' contains parent directory traversal",
                    self.git.worktree_path.display(),
                ));
            }
        }

        Ok(())
    }
}

/// Basic metadata identifying a feature.
///
/// The `slug` serves as the unique identifier for the feature
/// across directory names, branch names, and state files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureInfo {
    /// URL-safe feature slug used as unique identifier (e.g., `"add-user-auth"`).
    pub slug: String,

    /// When this feature was created.
    pub created_at: DateTime<Utc>,

    /// When this feature was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Git branch and worktree details for a feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInfo {
    /// Relative path to the git worktree.
    pub worktree_path: PathBuf,

    /// Branch name for this feature.
    pub branch: String,

    /// Base branch this feature was forked from.
    pub base_branch: String,
}

/// Distinguishes development phases (from the design spec) from fixed
/// quality-assurance phases (review, verify).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    /// Development phase derived from the design spec's "Development Phases".
    Dev,
    /// Fixed quality-assurance phase (review or verify).
    Quality,
}

impl Default for PhaseKind {
    /// Defaults to `Dev` so that legacy `state.yml` files without a `kind`
    /// field are treated as development phases.
    fn default() -> Self {
        Self::Dev
    }
}

/// Record of a single execution phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRecord {
    /// Phase name (e.g., `"pub-item-extraction"`, `"review"`, `"verify"`).
    pub name: String,

    /// Whether this is a development or quality-assurance phase.
    #[serde(default)]
    pub kind: PhaseKind,

    /// Current status of this phase.
    pub status: PhaseStatus,

    /// When execution of this phase started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,

    /// When execution of this phase completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,

    /// Number of agent conversation turns used.
    pub turns: u32,

    /// Total cost in USD for this phase.
    pub cost_usd: f64,

    /// Token usage breakdown.
    pub cost: TokenCost,

    /// Wall-clock duration in seconds.
    pub duration_secs: u64,

    /// Phase-specific details (flexible schema).
    pub details: serde_json::Value,
}

/// Token usage breakdown for cost tracking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCost {
    /// Number of input tokens consumed.
    pub input_tokens: u64,

    /// Number of output tokens generated.
    pub output_tokens: u64,
}

/// Pull request information for a completed feature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    /// Full URL to the pull request.
    pub url: String,

    /// PR number in the repository.
    pub number: u32,

    /// PR title.
    pub title: String,
}

/// Cumulative statistics across all phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotalStats {
    /// Total conversation turns across all phases.
    pub turns: u32,

    /// Total cost in USD across all phases.
    pub cost_usd: f64,

    /// Total token usage across all phases.
    pub cost: TokenCost,

    /// Total wall-clock duration in seconds.
    pub duration_secs: u64,
}

/// Overall status of a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FeatureStatus {
    /// Feature has been planned but not started.
    Planned,
    /// Feature is currently being executed.
    InProgress,
    /// Feature has been completed successfully.
    Completed,
    /// Feature execution failed.
    Failed,
    /// Feature's PR has been merged and worktree cleaned up.
    Merged,
}

/// Status of an individual execution phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PhaseStatus {
    /// Phase has not started yet.
    Pending,
    /// Phase is currently executing.
    Running,
    /// Phase completed successfully.
    Completed,
    /// Phase failed.
    Failed,
}

impl std::fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Planned => write!(f, "planned"),
            Self::InProgress => write!(f, "in progress"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Merged => write!(f, "merged"),
        }
    }
}

impl std::fmt::Display for PhaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl Default for TotalStats {
    fn default() -> Self {
        Self {
            turns: 0,
            cost_usd: 0.0,
            cost: TokenCost::default(),
            duration_secs: 0,
        }
    }
}

// ── PhaseOutcome ────────────────────────────────────────────────────

/// Complete outcome of a phase execution.
///
/// Contains all metrics needed to finalize a phase record via
/// [`StateManager::complete_phase`].
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use coda_core::state::PhaseOutcome;
///
/// let outcome = PhaseOutcome {
///     turns: 3,
///     cost_usd: 0.15,
///     input_tokens: 2000,
///     output_tokens: 800,
///     duration: Duration::from_secs(120),
///     details: serde_json::json!({"files_changed": 5}),
/// };
/// assert_eq!(outcome.turns, 3);
/// ```
#[derive(Debug)]
pub struct PhaseOutcome {
    /// Number of agent conversation turns used.
    pub turns: u32,
    /// Total cost in USD for this phase.
    pub cost_usd: f64,
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens generated.
    pub output_tokens: u64,
    /// Wall-clock duration of the phase.
    pub duration: Duration,
    /// Phase-specific details (flexible schema).
    pub details: serde_json::Value,
}

// ── StateManager ────────────────────────────────────────────────────

/// Centralizes feature state persistence and enforces transition invariants.
///
/// All state mutations (phase transitions, feature status changes, total
/// recomputation) flow through this manager, which validates invariants
/// before applying changes. The `current_phase` field is derived from
/// phase statuses rather than tracked independently, eliminating a class
/// of bugs where the counter and statuses diverge after a crash.
///
/// # Invariants
///
/// - **Phase transitions**: `Pending → Running → Completed` or
///   `Pending → Running → Failed`. `Running → Running` is allowed for
///   resuming interrupted phases. Failing is always valid from any state.
/// - **Feature status transitions**: `Planned → InProgress`,
///   `InProgress → Completed | Failed`, `Failed → InProgress` (resume),
///   `Completed → Merged`.
/// - **`current_phase`**: Always derived from phase statuses as the index
///   of the first non-completed phase (or `phases.len()` when all are done).
///   Synced to the [`FeatureState`] field before each save for backward
///   compatibility with existing `state.yml` consumers.
///
/// # Example
///
/// ```
/// use std::path::PathBuf;
/// use coda_core::state::*;
///
/// let now = chrono::Utc::now();
/// let state = FeatureState {
///     feature: FeatureInfo {
///         slug: "test".into(),
///         created_at: now,
///         updated_at: now,
///     },
///     status: FeatureStatus::Planned,
///     current_phase: 0,
///     git: GitInfo {
///         worktree_path: ".trees/test".into(),
///         branch: "feature/test".into(),
///         base_branch: "main".into(),
///     },
///     phases: vec![],
///     pr: None,
///     total: TotalStats::default(),
/// };
/// let mgr = StateManager::new(state, PathBuf::from("/tmp/state.yml"));
/// assert_eq!(mgr.current_phase_index(), 0);
/// ```
#[derive(Debug)]
pub struct StateManager {
    state: FeatureState,
    state_path: PathBuf,
}

impl StateManager {
    /// Creates a new manager wrapping the given state and persistence path.
    pub fn new(state: FeatureState, state_path: PathBuf) -> Self {
        Self { state, state_path }
    }

    /// Returns a read-only reference to the feature state.
    pub fn state(&self) -> &FeatureState {
        &self.state
    }

    /// Returns the path to the persisted state file.
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Derives the current phase index from phase statuses.
    ///
    /// Returns the index of the first non-completed phase, or
    /// `phases.len()` if all phases are completed.
    pub fn current_phase_index(&self) -> usize {
        self.state
            .phases
            .iter()
            .position(|p| p.status != PhaseStatus::Completed)
            .unwrap_or(self.state.phases.len())
    }

    /// Persists the current state to `state.yml`.
    ///
    /// Syncs `current_phase` from the derived value before serializing,
    /// ensuring the persisted counter always matches phase statuses.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::YamlError` on serialization failure or
    /// `CoreError::IoError` on write failure.
    pub fn save(&mut self) -> Result<(), CoreError> {
        self.state.current_phase = self.current_phase_index() as u32;
        let yaml = serde_yaml::to_string(&self.state)?;
        std::fs::write(&self.state_path, yaml).map_err(CoreError::IoError)?;
        tracing::debug!(path = %self.state_path.display(), "State saved");
        Ok(())
    }

    /// Marks a phase as running and persists state.
    ///
    /// Validates the transition: `Pending`, `Running` (resume), or `Failed` (retry)
    /// phases can be moved to `Running`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::StateError` if the phase is `Completed`.
    pub fn mark_phase_running(&mut self, phase_idx: usize) -> Result<(), CoreError> {
        let status = self.state.phases[phase_idx].status;
        match status {
            PhaseStatus::Pending | PhaseStatus::Running | PhaseStatus::Failed => {}
            other => {
                return Err(CoreError::StateError(format!(
                    "Cannot mark phase '{}' as running: current status is {other}",
                    self.state.phases[phase_idx].name,
                )));
            }
        }
        self.state.phases[phase_idx].status = PhaseStatus::Running;
        self.state.phases[phase_idx].started_at = Some(chrono::Utc::now());
        if let Err(e) = self.save() {
            tracing::warn!(error = %e, "Failed to save state when marking phase as running");
        }
        Ok(())
    }

    /// Finalizes a phase as completed with the given outcome.
    ///
    /// Sets all phase-record fields atomically from the [`PhaseOutcome`],
    /// ensuring no caller can forget to set cost or token counts.
    /// Validates that the phase is currently `Running`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::StateError` if the phase is not `Running`.
    pub fn complete_phase(
        &mut self,
        phase_idx: usize,
        outcome: &PhaseOutcome,
    ) -> Result<(), CoreError> {
        let status = self.state.phases[phase_idx].status;
        if status != PhaseStatus::Running {
            return Err(CoreError::StateError(format!(
                "Cannot complete phase '{}': current status is {status} (expected Running)",
                self.state.phases[phase_idx].name,
            )));
        }
        let phase = &mut self.state.phases[phase_idx];
        phase.status = PhaseStatus::Completed;
        phase.completed_at = Some(chrono::Utc::now());
        phase.turns = outcome.turns;
        phase.cost_usd = outcome.cost_usd;
        phase.cost.input_tokens = outcome.input_tokens;
        phase.cost.output_tokens = outcome.output_tokens;
        phase.duration_secs = outcome.duration.as_secs();
        phase.details = outcome.details.clone();
        self.state.feature.updated_at = chrono::Utc::now();

        if let Err(e) = self.save() {
            tracing::warn!(error = %e, "Failed to save state after completing phase");
        }
        Ok(())
    }

    /// Marks a phase as failed.
    ///
    /// Unlike [`mark_phase_running`](Self::mark_phase_running) and
    /// [`complete_phase`](Self::complete_phase), this method accepts
    /// any current status because failures can occur from any state
    /// (including retroactive failure via the circuit breaker after a
    /// spurious zero-cost completion).
    pub fn fail_phase(&mut self, phase_idx: usize) {
        self.state.phases[phase_idx].status = PhaseStatus::Failed;
        self.state.feature.updated_at = chrono::Utc::now();
        if let Err(e) = self.save() {
            tracing::warn!(error = %e, "Failed to save state after marking phase as failed");
        }
    }

    /// Sets the overall feature status with transition validation.
    ///
    /// Valid transitions:
    /// - `Planned → InProgress` (first run)
    /// - `Failed → InProgress` (resume after failure)
    /// - `InProgress → InProgress` (idempotent, resume after crash/cancel)
    /// - `InProgress → Completed` (all phases passed)
    /// - `InProgress → Failed` (a phase failed)
    /// - `Completed → Merged` (PR merged and cleaned up)
    ///
    /// # Errors
    ///
    /// Returns `CoreError::StateError` for invalid transitions.
    pub fn set_feature_status(&mut self, new_status: FeatureStatus) -> Result<(), CoreError> {
        let valid = matches!(
            (self.state.status, new_status),
            (FeatureStatus::Planned, FeatureStatus::InProgress)
                | (FeatureStatus::InProgress, FeatureStatus::Completed)
                | (FeatureStatus::InProgress, FeatureStatus::Failed)
                | (FeatureStatus::InProgress, FeatureStatus::InProgress)
                | (FeatureStatus::Failed, FeatureStatus::InProgress)
                | (FeatureStatus::Completed, FeatureStatus::Merged)
        );
        if !valid {
            return Err(CoreError::StateError(format!(
                "Invalid feature status transition: {} → {new_status}",
                self.state.status,
            )));
        }
        self.state.status = new_status;
        self.state.feature.updated_at = chrono::Utc::now();
        Ok(())
    }

    /// Sets the pull request information.
    pub fn set_pr(&mut self, pr: PrInfo) {
        self.state.pr = Some(pr);
        self.state.feature.updated_at = chrono::Utc::now();
    }

    /// Recomputes cumulative totals from all phase records.
    pub fn update_totals(&mut self) {
        let mut total_turns = 0u32;
        let mut total_cost = 0.0f64;
        let mut total_duration = 0u64;
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;

        for phase in &self.state.phases {
            total_turns += phase.turns;
            total_cost += phase.cost_usd;
            total_duration += phase.duration_secs;
            total_input_tokens += phase.cost.input_tokens;
            total_output_tokens += phase.cost.output_tokens;
        }

        self.state.total.turns = total_turns;
        self.state.total.cost_usd = total_cost;
        self.state.total.duration_secs = total_duration;
        self.state.total.cost.input_tokens = total_input_tokens;
        self.state.total.cost.output_tokens = total_output_tokens;
    }

    /// Returns the total cost already spent across completed phases.
    ///
    /// Delegates to [`FeatureState::spent_budget`].
    pub fn spent_budget(&self) -> f64 {
        self.state.spent_budget()
    }
}

// ── CoreError import (via crate) ────────────────────────────────────

use crate::CoreError;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_phase(name: &str, kind: PhaseKind) -> PhaseRecord {
        PhaseRecord {
            name: name.to_string(),
            kind,
            status: PhaseStatus::Pending,
            started_at: None,
            completed_at: None,
            turns: 0,
            cost_usd: 0.0,
            cost: TokenCost::default(),
            duration_secs: 0,
            details: serde_json::json!({}),
        }
    }

    fn make_completed_phase(name: &str, kind: PhaseKind, cost: f64) -> PhaseRecord {
        PhaseRecord {
            name: name.to_string(),
            kind,
            status: PhaseStatus::Completed,
            started_at: None,
            completed_at: None,
            turns: 1,
            cost_usd: cost,
            cost: TokenCost::default(),
            duration_secs: 60,
            details: serde_json::json!({}),
        }
    }

    fn make_running_phase(name: &str, kind: PhaseKind) -> PhaseRecord {
        PhaseRecord {
            name: name.to_string(),
            kind,
            status: PhaseStatus::Running,
            started_at: Some(chrono::Utc::now()),
            completed_at: None,
            turns: 0,
            cost_usd: 0.0,
            cost: TokenCost::default(),
            duration_secs: 0,
            details: serde_json::json!({}),
        }
    }

    fn make_test_state(phases: Vec<PhaseRecord>) -> FeatureState {
        let now = chrono::Utc::now();
        FeatureState {
            feature: FeatureInfo {
                slug: "test".to_string(),
                created_at: now,
                updated_at: now,
            },
            status: FeatureStatus::Planned,
            current_phase: 0,
            git: GitInfo {
                worktree_path: PathBuf::from(".trees/test"),
                branch: "feature/test".to_string(),
                base_branch: "main".to_string(),
            },
            phases,
            pr: None,
            total: TotalStats::default(),
        }
    }

    fn make_test_manager(phases: Vec<PhaseRecord>) -> StateManager {
        let state = make_test_state(phases);
        StateManager::new(state, PathBuf::from("/tmp/coda-test-state.yml"))
    }

    // ── FeatureState tests ──────────────────────────────────────────

    #[test]
    fn test_should_serialize_feature_status() {
        let status = FeatureStatus::InProgress;
        let yaml = serde_yaml::to_string(&status).unwrap();
        assert!(yaml.contains("in_progress"));
    }

    #[test]
    fn test_should_serialize_phase_status() {
        let status = PhaseStatus::Running;
        let yaml = serde_yaml::to_string(&status).unwrap();
        assert!(yaml.contains("running"));
    }

    #[test]
    fn test_should_round_trip_token_cost() {
        let cost = TokenCost {
            input_tokens: 3000,
            output_tokens: 1500,
        };
        let yaml = serde_yaml::to_string(&cost).unwrap();
        let deserialized: TokenCost = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.input_tokens, 3000);
        assert_eq!(deserialized.output_tokens, 1500);
    }

    #[test]
    fn test_should_round_trip_feature_state() {
        let now = chrono::Utc::now();
        let state = FeatureState {
            feature: FeatureInfo {
                slug: "add-user-auth".to_string(),
                created_at: now,
                updated_at: now,
            },
            status: FeatureStatus::InProgress,
            current_phase: 2,
            git: GitInfo {
                worktree_path: PathBuf::from(".trees/add-user-auth"),
                branch: "feature/add-user-auth".to_string(),
                base_branch: "main".to_string(),
            },
            phases: vec![
                PhaseRecord {
                    name: "auth-types".to_string(),
                    kind: PhaseKind::Dev,
                    status: PhaseStatus::Completed,
                    started_at: Some(now),
                    completed_at: Some(now),
                    turns: 3,
                    cost_usd: 0.12,
                    cost: TokenCost {
                        input_tokens: 3000,
                        output_tokens: 1500,
                    },
                    duration_secs: 300,
                    details: serde_json::json!({"files_created": 4}),
                },
                PhaseRecord {
                    name: "auth-middleware".to_string(),
                    kind: PhaseKind::Dev,
                    status: PhaseStatus::Completed,
                    started_at: Some(now),
                    completed_at: Some(now),
                    turns: 12,
                    cost_usd: 1.85,
                    cost: TokenCost {
                        input_tokens: 25000,
                        output_tokens: 12000,
                    },
                    duration_secs: 5100,
                    details: serde_json::json!({"files_changed": 8}),
                },
                PhaseRecord {
                    name: "review".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Running,
                    started_at: Some(now),
                    completed_at: None,
                    turns: 5,
                    cost_usd: 0.52,
                    cost: TokenCost {
                        input_tokens: 8000,
                        output_tokens: 4000,
                    },
                    duration_secs: 900,
                    details: serde_json::json!({}),
                },
            ],
            pr: Some(PrInfo {
                url: "https://github.com/org/repo/pull/42".to_string(),
                number: 42,
                title: "feat: add user authentication".to_string(),
            }),
            total: TotalStats {
                turns: 20,
                cost_usd: 2.49,
                cost: TokenCost {
                    input_tokens: 36000,
                    output_tokens: 17500,
                },
                duration_secs: 6300,
            },
        };

        let yaml = serde_yaml::to_string(&state).unwrap();
        let deserialized: FeatureState = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(deserialized.feature.slug, "add-user-auth");
        assert_eq!(deserialized.status, FeatureStatus::InProgress);
        assert_eq!(deserialized.current_phase, 2);
        assert_eq!(deserialized.git.branch, "feature/add-user-auth");
        assert_eq!(deserialized.git.base_branch, "main");
        assert_eq!(deserialized.phases.len(), 3);
        assert_eq!(deserialized.phases[0].kind, PhaseKind::Dev);
        assert_eq!(deserialized.phases[0].status, PhaseStatus::Completed);
        assert_eq!(deserialized.phases[1].kind, PhaseKind::Dev);
        assert_eq!(deserialized.phases[1].turns, 12);
        assert_eq!(deserialized.phases[2].kind, PhaseKind::Quality);
        assert_eq!(deserialized.phases[2].status, PhaseStatus::Running);
        assert!(deserialized.pr.is_some());
        assert_eq!(deserialized.pr.as_ref().unwrap().number, 42);
        assert_eq!(deserialized.total.turns, 20);
        assert!((deserialized.total.cost_usd - 2.49).abs() < f64::EPSILON);
        assert_eq!(deserialized.total.cost.input_tokens, 36000);
    }

    #[test]
    fn test_should_create_default_total_stats() {
        let stats = TotalStats::default();
        assert_eq!(stats.turns, 0);
        assert!((stats.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(stats.cost.input_tokens, 0);
        assert_eq!(stats.cost.output_tokens, 0);
        assert_eq!(stats.duration_secs, 0);
    }

    #[test]
    fn test_should_validate_correct_state() {
        let state = make_test_state(vec![
            make_phase("dev-phase-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn test_should_reject_too_few_phases() {
        let state = make_test_state(vec![]);
        let err = state.validate().unwrap_err();
        assert!(err.contains("at least 4 phases"));
    }

    #[test]
    fn test_should_reject_path_traversal_in_worktree() {
        let now = chrono::Utc::now();
        let state = FeatureState {
            feature: FeatureInfo {
                slug: "test".to_string(),
                created_at: now,
                updated_at: now,
            },
            status: FeatureStatus::Planned,
            current_phase: 0,
            git: GitInfo {
                worktree_path: PathBuf::from("../../etc/shadow"),
                branch: "feature/test".to_string(),
                base_branch: "main".to_string(),
            },
            phases: vec![
                make_phase("dev-1", PhaseKind::Dev),
                make_phase("review", PhaseKind::Quality),
                make_phase("verify", PhaseKind::Quality),
                make_phase("update-docs", PhaseKind::Quality),
            ],
            pr: None,
            total: TotalStats::default(),
        };
        let err = state.validate().unwrap_err();
        assert!(err.contains("parent directory traversal"));
    }

    #[test]
    fn test_should_reject_missing_terminal_quality_phases() {
        // 4 phases but missing update-docs (legacy state with extra dev phase)
        let state = make_test_state(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("dev-2", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
        ]);
        let err = state.validate().unwrap_err();
        assert!(err.contains("expected terminal quality phase"));
        assert!(err.contains("review"));
    }

    #[test]
    fn test_should_reject_wrong_quality_phase_order() {
        // Quality phases in wrong order: verify before review
        let state = make_test_state(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("verify", PhaseKind::Quality),
            make_phase("review", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let err = state.validate().unwrap_err();
        assert!(err.contains("expected terminal quality phase"));
        assert!(err.contains("review"));
    }

    #[test]
    fn test_should_reject_dev_phase_after_quality_phase() {
        let state = make_test_state(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("dev-2", PhaseKind::Dev),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let err = state.validate().unwrap_err();
        assert!(err.contains("dev phase 'dev-2' at index 2 appears after first quality phase"));
    }

    #[test]
    fn test_should_migrate_legacy_state_missing_update_docs() {
        // Legacy state: review + verify but no update-docs
        let mut state = make_test_state(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
        ]);
        // Before migration: fails validation (too few phases)
        assert!(state.validate().is_err());

        state.migrate();

        // After migration: update-docs appended, passes validation
        assert_eq!(state.phases.len(), 4);
        assert_eq!(state.phases[3].name, "update-docs");
        assert_eq!(state.phases[3].kind, PhaseKind::Quality);
        assert_eq!(state.phases[3].status, PhaseStatus::Pending);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn test_should_not_duplicate_phases_on_migrate() {
        // Already has all quality phases
        let mut state = make_test_state(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        state.migrate();

        // Should not add duplicates
        assert_eq!(state.phases.len(), 4);
        assert!(state.validate().is_ok());
    }

    #[test]
    fn test_should_migrate_misordered_quality_phases() {
        // Legacy state with quality phases in wrong order
        let mut state = make_test_state(vec![
            make_phase("dev1", PhaseKind::Dev),
            make_phase("verify", PhaseKind::Quality),
            make_phase("dev2", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
        ]);

        state.migrate();

        // After migration: dev phases first, then quality in canonical order
        assert_eq!(state.phases.len(), 5);
        assert_eq!(state.phases[0].name, "dev1");
        assert_eq!(state.phases[0].kind, PhaseKind::Dev);
        assert_eq!(state.phases[1].name, "dev2");
        assert_eq!(state.phases[1].kind, PhaseKind::Dev);
        assert_eq!(state.phases[2].name, "review");
        assert_eq!(state.phases[3].name, "verify");
        assert_eq!(state.phases[4].name, "update-docs");
        assert!(state.validate().is_ok());
    }

    #[test]
    fn test_should_serialize_pr_info_as_none() {
        let now = chrono::Utc::now();
        let state = FeatureState {
            feature: FeatureInfo {
                slug: "test".to_string(),
                created_at: now,
                updated_at: now,
            },
            status: FeatureStatus::Planned,
            current_phase: 0,
            git: GitInfo {
                worktree_path: PathBuf::from(".trees/test"),
                branch: "feature/test".to_string(),
                base_branch: "main".to_string(),
            },
            phases: vec![],
            pr: None,
            total: TotalStats::default(),
        };

        let yaml = serde_yaml::to_string(&state).unwrap();
        // pr field should be omitted entirely when None
        assert!(!yaml.contains("pr:"));

        let deserialized: FeatureState = serde_yaml::from_str(&yaml).unwrap();
        assert!(deserialized.pr.is_none());
        assert_eq!(deserialized.status, FeatureStatus::Planned);
    }

    #[test]
    fn test_should_compute_spent_budget_from_completed_phases() {
        let state = make_test_state(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.00),
            make_completed_phase("dev-2", PhaseKind::Dev, 0.75),
            make_completed_phase("dev-3", PhaseKind::Dev, 0.75),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);

        assert!((state.spent_budget() - 2.50).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_exclude_non_completed_phases_from_spent_budget() {
        let mut running_phase = make_phase("review", PhaseKind::Quality);
        running_phase.status = PhaseStatus::Running;
        running_phase.cost_usd = 0.50;

        let state = make_test_state(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.00),
            running_phase,
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);

        // Only the completed phase counts
        assert!((state.spent_budget() - 1.00).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_return_zero_spent_budget_when_no_completed_phases() {
        let state = make_test_state(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        assert!((state.spent_budget() - 0.0).abs() < f64::EPSILON);
    }

    // ── StateManager tests ──────────────────────────────────────────

    #[test]
    fn test_should_derive_current_phase_all_pending() {
        let mgr = make_test_manager(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        assert_eq!(mgr.current_phase_index(), 0);
    }

    #[test]
    fn test_should_derive_current_phase_with_completed_prefix() {
        let mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.0),
            make_completed_phase("dev-2", PhaseKind::Dev, 0.5),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        assert_eq!(mgr.current_phase_index(), 2);
    }

    #[test]
    fn test_should_derive_current_phase_all_completed() {
        let mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.0),
            make_completed_phase("review", PhaseKind::Quality, 0.5),
            make_completed_phase("verify", PhaseKind::Quality, 0.3),
            make_completed_phase("update-docs", PhaseKind::Quality, 0.2),
        ]);
        assert_eq!(mgr.current_phase_index(), 4);
    }

    #[test]
    fn test_should_derive_current_phase_with_running() {
        let mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.0),
            make_running_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        // Running is not Completed, so current is at that index
        assert_eq!(mgr.current_phase_index(), 1);
    }

    #[test]
    fn test_should_mark_pending_phase_as_running() {
        let mut mgr = make_test_manager(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let result = mgr.mark_phase_running(0);
        assert!(result.is_ok());
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Running);
        assert!(mgr.state().phases[0].started_at.is_some());
    }

    #[test]
    fn test_should_allow_running_to_running_for_resume() {
        let mut mgr = make_test_manager(vec![
            make_running_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let result = mgr.mark_phase_running(0);
        assert!(result.is_ok());
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Running);
    }

    #[test]
    fn test_should_allow_failed_phase_to_running_for_retry() {
        let mut mgr = make_test_manager(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        // Simulate a previous failure
        mgr.fail_phase(0);
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Failed);
        // Retry: mark failed phase as running
        let result = mgr.mark_phase_running(0);
        assert!(result.is_ok());
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Running);
    }

    #[test]
    fn test_should_reject_completed_phase_to_running() {
        let mut mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.0),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let result = mgr.mark_phase_running(0);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Cannot mark phase"));
        assert!(err.contains("completed"));
    }

    #[test]
    fn test_should_complete_running_phase() {
        let mut mgr = make_test_manager(vec![
            make_running_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let outcome = PhaseOutcome {
            turns: 5,
            cost_usd: 1.25,
            input_tokens: 3000,
            output_tokens: 1500,
            duration: Duration::from_secs(120),
            details: serde_json::json!({"files_changed": 3}),
        };
        let result = mgr.complete_phase(0, &outcome);
        assert!(result.is_ok());
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Completed);
        assert_eq!(mgr.state().phases[0].turns, 5);
        assert!((mgr.state().phases[0].cost_usd - 1.25).abs() < f64::EPSILON);
        assert_eq!(mgr.state().phases[0].cost.input_tokens, 3000);
        assert_eq!(mgr.state().phases[0].cost.output_tokens, 1500);
        assert_eq!(mgr.state().phases[0].duration_secs, 120);
        assert!(mgr.state().phases[0].completed_at.is_some());
    }

    #[test]
    fn test_should_reject_completing_pending_phase() {
        let mut mgr = make_test_manager(vec![
            make_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        let outcome = PhaseOutcome {
            turns: 1,
            cost_usd: 0.1,
            input_tokens: 100,
            output_tokens: 50,
            duration: Duration::from_secs(10),
            details: serde_json::json!({}),
        };
        let result = mgr.complete_phase(0, &outcome);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Cannot complete phase"));
        assert!(err.contains("pending"));
    }

    #[test]
    fn test_should_fail_phase_from_running() {
        let mut mgr = make_test_manager(vec![
            make_running_phase("dev-1", PhaseKind::Dev),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        mgr.fail_phase(0);
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Failed);
    }

    #[test]
    fn test_should_fail_phase_from_completed_for_circuit_breaker() {
        let mut mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 0.0),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        // Circuit breaker: retroactively fail a completed phase
        mgr.fail_phase(0);
        assert_eq!(mgr.state().phases[0].status, PhaseStatus::Failed);
    }

    #[test]
    fn test_should_validate_feature_status_planned_to_in_progress() {
        let mut mgr = make_test_manager(vec![]);
        let result = mgr.set_feature_status(FeatureStatus::InProgress);
        assert!(result.is_ok());
        assert_eq!(mgr.state().status, FeatureStatus::InProgress);
    }

    #[test]
    fn test_should_validate_feature_status_in_progress_to_completed() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_feature_status(FeatureStatus::InProgress).unwrap();
        let result = mgr.set_feature_status(FeatureStatus::Completed);
        assert!(result.is_ok());
    }

    #[test]
    fn test_should_validate_feature_status_in_progress_to_failed() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_feature_status(FeatureStatus::InProgress).unwrap();
        let result = mgr.set_feature_status(FeatureStatus::Failed);
        assert!(result.is_ok());
    }

    #[test]
    fn test_should_validate_feature_status_failed_to_in_progress() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_feature_status(FeatureStatus::InProgress).unwrap();
        mgr.set_feature_status(FeatureStatus::Failed).unwrap();
        let result = mgr.set_feature_status(FeatureStatus::InProgress);
        assert!(result.is_ok());
    }

    #[test]
    fn test_should_validate_feature_status_completed_to_merged() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_feature_status(FeatureStatus::InProgress).unwrap();
        mgr.set_feature_status(FeatureStatus::Completed).unwrap();
        let result = mgr.set_feature_status(FeatureStatus::Merged);
        assert!(result.is_ok());
    }

    #[test]
    fn test_should_reject_invalid_feature_status_transition() {
        let mut mgr = make_test_manager(vec![]);
        // Planned → Completed is invalid (must go through InProgress)
        let result = mgr.set_feature_status(FeatureStatus::Completed);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Invalid feature status transition"));
    }

    #[test]
    fn test_should_reject_completed_to_in_progress() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_feature_status(FeatureStatus::InProgress).unwrap();
        mgr.set_feature_status(FeatureStatus::Completed).unwrap();
        let result = mgr.set_feature_status(FeatureStatus::InProgress);
        assert!(result.is_err());
    }

    #[test]
    fn test_should_allow_in_progress_to_in_progress_for_resume() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_feature_status(FeatureStatus::InProgress).unwrap();
        // Simulate resume after crash: state is already InProgress
        let result = mgr.set_feature_status(FeatureStatus::InProgress);
        assert!(result.is_ok());
        assert_eq!(mgr.state().status, FeatureStatus::InProgress);
    }

    #[test]
    fn test_should_update_totals_from_phases() {
        let mut mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.00),
            make_completed_phase("dev-2", PhaseKind::Dev, 0.50),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        mgr.update_totals();
        assert_eq!(mgr.state().total.turns, 2);
        assert!((mgr.state().total.cost_usd - 1.50).abs() < f64::EPSILON);
        assert_eq!(mgr.state().total.duration_secs, 120);
    }

    #[test]
    fn test_should_set_pr_info() {
        let mut mgr = make_test_manager(vec![]);
        mgr.set_pr(PrInfo {
            url: "https://github.com/org/repo/pull/1".to_string(),
            number: 1,
            title: "feat: test".to_string(),
        });
        assert!(mgr.state().pr.is_some());
        assert_eq!(mgr.state().pr.as_ref().unwrap().number, 1);
    }

    #[test]
    fn test_should_delegate_spent_budget() {
        let mgr = make_test_manager(vec![
            make_completed_phase("dev-1", PhaseKind::Dev, 1.25),
            make_completed_phase("dev-2", PhaseKind::Dev, 0.75),
            make_phase("review", PhaseKind::Quality),
            make_phase("verify", PhaseKind::Quality),
            make_phase("update-docs", PhaseKind::Quality),
        ]);
        assert!((mgr.spent_budget() - 2.00).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_save_with_synced_current_phase() {
        let dir = std::env::temp_dir().join("coda-state-manager-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.yml");

        let mut mgr = StateManager::new(
            make_test_state(vec![
                make_completed_phase("dev-1", PhaseKind::Dev, 1.0),
                make_phase("review", PhaseKind::Quality),
                make_phase("verify", PhaseKind::Quality),
                make_phase("update-docs", PhaseKind::Quality),
            ]),
            path.clone(),
        );

        // current_phase starts at 0 (stale), derived value is 1
        assert_eq!(mgr.state().current_phase, 0);
        assert_eq!(mgr.current_phase_index(), 1);

        mgr.save().unwrap();

        // After save, the file should have current_phase = 1 (derived)
        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: FeatureState = serde_yaml::from_str(&content).unwrap();
        assert_eq!(loaded.current_phase, 1);

        // In-memory state is also synced
        assert_eq!(mgr.state().current_phase, 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
