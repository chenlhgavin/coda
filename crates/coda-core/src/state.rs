//! Feature state types for tracking execution progress.
//!
//! Defines `FeatureState` which is persisted to `.coda/<feature>/state.yml`
//! and supports resuming interrupted runs.

use std::path::{Component, PathBuf};

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

/// Minimum number of phases: at least 1 dev phase + review + verify.
const MIN_PHASE_COUNT: usize = 3;

impl FeatureState {
    /// Validates structural invariants after deserialization.
    ///
    /// Checks that `phases` has at least [`MIN_PHASE_COUNT`] entries
    /// (1+ dev phases + review + verify), `current_phase` is within bounds,
    /// and `worktree_path` does not contain parent-directory references.
    ///
    /// # Errors
    ///
    /// Returns a human-readable error description when validation fails.
    pub fn validate(&self) -> Result<(), String> {
        if self.phases.len() < MIN_PHASE_COUNT {
            return Err(format!(
                "expected at least {MIN_PHASE_COUNT} phases (dev + review + verify), found {}",
                self.phases.len(),
            ));
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// Status of an individual execution phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

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
            phases: vec![
                PhaseRecord {
                    name: "dev-phase-1".to_string(),
                    kind: PhaseKind::Dev,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "review".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "verify".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
            ],
            pr: None,
            total: TotalStats::default(),
        };
        assert!(state.validate().is_ok());
    }

    #[test]
    fn test_should_reject_too_few_phases() {
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
            phases: vec![], // wrong: need at least 3
            pr: None,
            total: TotalStats::default(),
        };
        let err = state.validate().unwrap_err();
        assert!(err.contains("at least 3 phases"));
    }

    #[test]
    fn test_should_reject_path_traversal_in_worktree() {
        let now = chrono::Utc::now();
        let make_phase = |name: &str, kind: PhaseKind| PhaseRecord {
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
        };
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
            ],
            pr: None,
            total: TotalStats::default(),
        };
        let err = state.validate().unwrap_err();
        assert!(err.contains("parent directory traversal"));
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
}
