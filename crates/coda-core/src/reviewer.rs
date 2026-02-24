//! Reviewer types for agent-driven code review.
//!
//! The review logic is integrated into the [`Runner`](crate::runner::Runner)'s
//! review phase. This module provides the [`ReviewResult`] type that
//! aggregates findings across multiple review rounds.

pub use crate::runner::ReviewSummary;

/// Result of an agent-driven code review cycle.
///
/// Contains the aggregated review findings and resolution status
/// across all review rounds performed during a `coda run`.
///
/// # Examples
///
/// ```
/// use coda_core::reviewer::ReviewResult;
/// use coda_core::ReviewSummary;
///
/// let result = ReviewResult {
///     summary: ReviewSummary {
///         rounds: 2,
///         issues_found: 3,
///         issues_fix_attempted: 3,
///     },
///     all_resolved: true,
/// };
///
/// assert!(result.all_resolved);
/// assert_eq!(result.summary.rounds, 2);
/// ```
#[derive(Debug)]
pub struct ReviewResult {
    /// Review summary with round/issue counts.
    pub summary: ReviewSummary,
    /// Whether all critical/major issues were resolved.
    pub all_resolved: bool,
}
