//! Reviewer types for agent-driven code review.
//!
//! The review logic is implemented by
//! [`ReviewPhaseExecutor`](crate::phases::review::ReviewPhaseExecutor).
//! This module provides the [`ReviewResult`] type that aggregates
//! findings across multiple review rounds, and the [`ReviewSeverity`]
//! enum that replaces stringly-typed severity fields.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use crate::phases::ReviewSummary;

/// Severity level of a code review issue.
///
/// Used by [`ReviewIssue`](crate::codex::ReviewIssue) to classify the
/// impact of issues found during review. Only [`Critical`](Self::Critical)
/// and [`Major`](Self::Major) issues are acted upon; lower severities are
/// filtered out during parsing.
///
/// # Examples
///
/// ```
/// use coda_core::reviewer::ReviewSeverity;
///
/// let severity: ReviewSeverity = "critical".parse().unwrap();
/// assert_eq!(severity, ReviewSeverity::Critical);
/// assert!(severity.is_actionable());
/// assert_eq!(severity.to_string(), "critical");
///
/// // Round-trip through serde
/// let json = serde_json::to_string(&severity).unwrap();
/// assert_eq!(json, "\"critical\"");
/// let parsed: ReviewSeverity = serde_json::from_str(&json).unwrap();
/// assert_eq!(parsed, severity);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSeverity {
    /// Must-fix: blocks merge or introduces a correctness/security bug.
    Critical,
    /// Should-fix: significant design flaw, missing error handling, etc.
    Major,
    /// Nice-to-fix: style inconsistency, minor improvement opportunity.
    Minor,
    /// Informational observation, no action required.
    Info,
}

impl ReviewSeverity {
    /// Returns `true` if this severity level triggers a fix attempt.
    ///
    /// Only [`Critical`](Self::Critical) and [`Major`](Self::Major) issues
    /// are considered actionable.
    ///
    /// # Examples
    ///
    /// ```
    /// use coda_core::reviewer::ReviewSeverity;
    ///
    /// assert!(ReviewSeverity::Critical.is_actionable());
    /// assert!(ReviewSeverity::Major.is_actionable());
    /// assert!(!ReviewSeverity::Minor.is_actionable());
    /// assert!(!ReviewSeverity::Info.is_actionable());
    /// ```
    pub fn is_actionable(self) -> bool {
        matches!(self, Self::Critical | Self::Major)
    }
}

impl std::fmt::Display for ReviewSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Critical => write!(f, "critical"),
            Self::Major => write!(f, "major"),
            Self::Minor => write!(f, "minor"),
            Self::Info => write!(f, "info"),
        }
    }
}

impl FromStr for ReviewSeverity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "critical" => Ok(Self::Critical),
            "major" => Ok(Self::Major),
            "minor" => Ok(Self::Minor),
            "info" | "informational" => Ok(Self::Info),
            other => Err(format!("unknown review severity: {other}")),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    // ── ReviewSeverity ──────────────────────────────────────────────

    #[test]
    fn test_should_serialize_review_severity_lowercase() {
        assert_eq!(
            serde_json::to_string(&ReviewSeverity::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewSeverity::Major).unwrap(),
            "\"major\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewSeverity::Minor).unwrap(),
            "\"minor\""
        );
        assert_eq!(
            serde_json::to_string(&ReviewSeverity::Info).unwrap(),
            "\"info\""
        );
    }

    #[test]
    fn test_should_deserialize_review_severity_lowercase() {
        assert_eq!(
            serde_json::from_str::<ReviewSeverity>("\"critical\"").unwrap(),
            ReviewSeverity::Critical
        );
        assert_eq!(
            serde_json::from_str::<ReviewSeverity>("\"major\"").unwrap(),
            ReviewSeverity::Major
        );
        assert_eq!(
            serde_json::from_str::<ReviewSeverity>("\"minor\"").unwrap(),
            ReviewSeverity::Minor
        );
        assert_eq!(
            serde_json::from_str::<ReviewSeverity>("\"info\"").unwrap(),
            ReviewSeverity::Info
        );
    }

    #[test]
    fn test_should_round_trip_review_severity_serde() {
        for severity in [
            ReviewSeverity::Critical,
            ReviewSeverity::Major,
            ReviewSeverity::Minor,
            ReviewSeverity::Info,
        ] {
            let json = serde_json::to_string(&severity).unwrap();
            let parsed: ReviewSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, severity);
        }
    }

    #[test]
    fn test_should_display_review_severity_lowercase() {
        assert_eq!(ReviewSeverity::Critical.to_string(), "critical");
        assert_eq!(ReviewSeverity::Major.to_string(), "major");
        assert_eq!(ReviewSeverity::Minor.to_string(), "minor");
        assert_eq!(ReviewSeverity::Info.to_string(), "info");
    }

    #[test]
    fn test_should_parse_review_severity_from_str_case_insensitive() {
        assert_eq!(
            "Critical".parse::<ReviewSeverity>().unwrap(),
            ReviewSeverity::Critical
        );
        assert_eq!(
            "MAJOR".parse::<ReviewSeverity>().unwrap(),
            ReviewSeverity::Major
        );
        assert_eq!(
            "minor".parse::<ReviewSeverity>().unwrap(),
            ReviewSeverity::Minor
        );
        assert_eq!(
            "INFO".parse::<ReviewSeverity>().unwrap(),
            ReviewSeverity::Info
        );
        assert_eq!(
            "informational".parse::<ReviewSeverity>().unwrap(),
            ReviewSeverity::Info
        );
    }

    #[test]
    fn test_should_reject_invalid_severity() {
        assert!("unknown".parse::<ReviewSeverity>().is_err());
    }

    #[test]
    fn test_should_display_and_from_str_roundtrip_for_severity() {
        for severity in [
            ReviewSeverity::Critical,
            ReviewSeverity::Major,
            ReviewSeverity::Minor,
            ReviewSeverity::Info,
        ] {
            let displayed = severity.to_string();
            let parsed: ReviewSeverity = displayed.parse().unwrap();
            assert_eq!(parsed, severity);
        }
    }

    #[test]
    fn test_should_identify_actionable_severities() {
        assert!(ReviewSeverity::Critical.is_actionable());
        assert!(ReviewSeverity::Major.is_actionable());
        assert!(!ReviewSeverity::Minor.is_actionable());
        assert!(!ReviewSeverity::Info.is_actionable());
    }
}
