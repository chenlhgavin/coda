//! Task types for CODA execution.
//!
//! Defines `Task`, `TaskResult`, and `TaskStatus` which represent the
//! units of work in CODA's execution pipeline.

use std::path::PathBuf;
use std::time::Duration;

/// A unit of work in CODA's execution pipeline.
///
/// Each CLI command or execution phase maps to a `Task` variant.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Task {
    /// Initialize a repository as a CODA project.
    Init,

    /// Plan a new feature interactively.
    Plan {
        /// URL-safe feature slug (e.g., `"add-user-auth"`).
        feature_slug: String,
    },

    /// Set up the directory structure and scaffold code.
    Setup {
        /// URL-safe feature slug.
        feature_slug: String,
    },

    /// Implement the feature according to the design spec.
    Implement {
        /// URL-safe feature slug.
        feature_slug: String,
    },

    /// Write and run tests for the feature.
    Test {
        /// URL-safe feature slug.
        feature_slug: String,
    },

    /// Review the implemented code for issues.
    Review {
        /// URL-safe feature slug.
        feature_slug: String,
    },

    /// Verify the implementation against the verification plan.
    Verify {
        /// URL-safe feature slug.
        feature_slug: String,
    },

    /// Create a pull request after all phases complete.
    CreatePr {
        /// URL-safe feature slug.
        feature_slug: String,
    },
}

/// Result of executing a task, extracted from SDK's `ResultMessage`.
///
/// Contains execution metrics (turns, cost, duration) alongside the
/// task identity and outcome status.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// use coda_core::task::{Task, TaskResult, TaskStatus};
///
/// let result = TaskResult {
///     task: Task::Setup { feature_slug: "add-auth".to_string() },
///     status: TaskStatus::Completed,
///     turns: 3,
///     cost_usd: 0.12,
///     duration: Duration::from_secs(300),
///     artifacts: vec![],
/// };
///
/// assert_eq!(result.turns, 3);
/// assert!(matches!(result.status, TaskStatus::Completed));
/// ```
#[derive(Debug)]
pub struct TaskResult {
    /// The task that was executed.
    pub task: Task,

    /// Whether the task completed successfully or failed.
    pub status: TaskStatus,

    /// Number of agent conversation turns used.
    pub turns: u32,

    /// Total cost in USD from `ResultMessage.total_cost_usd`.
    pub cost_usd: f64,

    /// Wall-clock duration from `ResultMessage.duration_ms`.
    pub duration: Duration,

    /// Paths to files created or modified by this task.
    pub artifacts: Vec<PathBuf>,
}

/// Outcome status of a task execution.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TaskStatus {
    /// Task completed successfully.
    Completed,

    /// Task failed with an error message.
    Failed {
        /// Description of what went wrong.
        error: String,
    },
}
