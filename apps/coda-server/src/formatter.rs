//! Block Kit message builders for Slack responses.
//!
//! Pure functions that build `serde_json::Value` arrays representing
//! Slack Block Kit blocks. Each function produces a complete message
//! suitable for `chat.postMessage` or `chat.update`.

use std::path::Path;
use std::time::Duration;

use coda_core::CleanedWorktree;
use coda_core::state::{FeatureState, FeatureStatus, PhaseStatus};
use serde::{Deserialize, Serialize};

/// Action ID for the clean confirm button, used by both the formatter
/// and the interaction handler for routing.
pub const CLEAN_CONFIRM_ACTION: &str = "coda_clean_confirm";

/// Serializable representation of a clean candidate for button payloads.
///
/// Encoded as JSON in the clean confirm button's `value` field and
/// decoded by the interaction handler to reconstruct [`CleanedWorktree`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CleanTarget {
    /// Feature slug.
    pub slug: String,
    /// Git branch name.
    pub branch: String,
    /// PR number if found.
    pub pr_number: Option<u32>,
    /// PR state (e.g., "MERGED", "CLOSED").
    pub pr_state: String,
}

impl From<&CleanedWorktree> for CleanTarget {
    fn from(w: &CleanedWorktree) -> Self {
        Self {
            slug: w.slug.clone(),
            branch: w.branch.clone(),
            pr_number: w.pr_number,
            pr_state: w.pr_state.clone(),
        }
    }
}

impl From<CleanTarget> for CleanedWorktree {
    fn from(t: CleanTarget) -> Self {
        Self {
            slug: t.slug,
            branch: t.branch,
            pr_number: t.pr_number,
            pr_state: t.pr_state,
        }
    }
}

/// Display state for a single init phase in the progress message.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_server::formatter::{InitPhaseDisplay, PhaseDisplayStatus};
///
/// let phase = InitPhaseDisplay {
///     name: "analyze-repo".to_string(),
///     status: PhaseDisplayStatus::Completed,
///     duration: Some(Duration::from_secs(30)),
///     cost_usd: Some(0.12),
/// };
/// assert_eq!(phase.name, "analyze-repo");
/// ```
#[derive(Debug, Clone)]
pub struct InitPhaseDisplay {
    /// Phase name (e.g., `"analyze-repo"`, `"setup-project"`).
    pub name: String,
    /// Current display status.
    pub status: PhaseDisplayStatus,
    /// Duration if completed or failed.
    pub duration: Option<Duration>,
    /// Cost in USD if completed.
    pub cost_usd: Option<f64>,
}

/// Display state for a single run phase in the progress message.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_server::formatter::{RunPhaseDisplay, PhaseDisplayStatus};
///
/// let phase = RunPhaseDisplay {
///     name: "implement".to_string(),
///     status: PhaseDisplayStatus::Running,
///     duration: None,
///     turns: Some(3),
///     cost_usd: None,
/// };
/// assert_eq!(phase.name, "implement");
/// ```
#[derive(Debug, Clone)]
pub struct RunPhaseDisplay {
    /// Phase name (e.g., `"setup"`, `"implement"`, `"review"`).
    pub name: String,
    /// Current display status.
    pub status: PhaseDisplayStatus,
    /// Duration if completed or failed.
    pub duration: Option<Duration>,
    /// Number of agent turns used so far.
    pub turns: Option<u32>,
    /// Cost in USD if completed.
    pub cost_usd: Option<f64>,
}

/// Display status for a phase in progress messages.
///
/// # Examples
///
/// ```
/// use coda_server::formatter::PhaseDisplayStatus;
///
/// let status = PhaseDisplayStatus::Running;
/// assert!(matches!(status, PhaseDisplayStatus::Running));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseDisplayStatus {
    /// Phase has not started yet.
    Pending,
    /// Phase is currently executing.
    Running,
    /// Phase completed successfully.
    Completed,
    /// Phase failed with an error.
    Failed,
}

/// Builds a Block Kit progress message for an init operation.
///
/// Shows a header and a list of phases with status icons, durations,
/// and costs. Updated in-place via `chat.update` as phases progress.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_server::formatter::{self, InitPhaseDisplay, PhaseDisplayStatus};
///
/// let phases = vec![
///     InitPhaseDisplay {
///         name: "analyze-repo".to_string(),
///         status: PhaseDisplayStatus::Completed,
///         duration: Some(Duration::from_secs(30)),
///         cost_usd: Some(0.12),
///     },
///     InitPhaseDisplay {
///         name: "setup-project".to_string(),
///         status: PhaseDisplayStatus::Running,
///         duration: None,
///         cost_usd: None,
///     },
/// ];
/// let blocks = formatter::init_progress(&phases);
/// assert!(!blocks.is_empty());
/// ```
pub fn init_progress(phases: &[InitPhaseDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("CODA Init")];

    let mut lines = Vec::with_capacity(phases.len());
    for phase in phases {
        let icon = display_status_icon(phase.status);
        let mut line = format!("{icon} `{}`", phase.name);

        if let Some(duration) = phase.duration {
            let dur_str = format_duration(duration.as_secs());
            line.push_str(&format!(" \u{2014} {dur_str}"));
        }
        if let Some(cost) = phase.cost_usd {
            line.push_str(&format!(" \u{00b7} ${cost:.2}"));
        }

        lines.push(line);
    }

    blocks.push(section(&lines.join("\n")));
    blocks
}

/// Builds a Block Kit progress message for a run operation.
///
/// Shows a header with the feature slug, phase list with status icons,
/// turn counts, durations, and costs. Updated in-place via `chat.update`.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_server::formatter::{self, RunPhaseDisplay, PhaseDisplayStatus};
///
/// let phases = vec![
///     RunPhaseDisplay {
///         name: "setup".to_string(),
///         status: PhaseDisplayStatus::Completed,
///         duration: Some(Duration::from_secs(60)),
///         turns: Some(3),
///         cost_usd: Some(0.25),
///     },
///     RunPhaseDisplay {
///         name: "implement".to_string(),
///         status: PhaseDisplayStatus::Running,
///         duration: None,
///         turns: Some(7),
///         cost_usd: None,
///     },
/// ];
/// let blocks = formatter::run_progress("add-auth", &phases);
/// assert!(!blocks.is_empty());
/// ```
pub fn run_progress(feature_slug: &str, phases: &[RunPhaseDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Run: `{feature_slug}`"))];

    let mut lines = Vec::with_capacity(phases.len());
    for phase in phases {
        let icon = display_status_icon(phase.status);
        let mut line = format!("{icon} `{}`", phase.name);

        match phase.status {
            PhaseDisplayStatus::Running => {
                if let Some(turns) = phase.turns {
                    line.push_str(&format!(" \u{2014} {turns} turns"));
                }
            }
            PhaseDisplayStatus::Completed => {
                let mut stats = Vec::new();
                if let Some(turns) = phase.turns {
                    stats.push(format!("{turns} turns"));
                }
                if let Some(cost) = phase.cost_usd {
                    stats.push(format!("${cost:.2}"));
                }
                if let Some(duration) = phase.duration {
                    stats.push(format_duration(duration.as_secs()));
                }
                if !stats.is_empty() {
                    line.push_str(&format!(" \u{2014} {}", stats.join(" \u{00b7} ")));
                }
            }
            PhaseDisplayStatus::Failed => {
                if let Some(duration) = phase.duration {
                    line.push_str(&format!(
                        " \u{2014} failed after {}",
                        format_duration(duration.as_secs())
                    ));
                }
            }
            PhaseDisplayStatus::Pending => {}
        }

        lines.push(line);
    }

    blocks.push(section(&lines.join("\n")));
    blocks
}

/// Builds a Block Kit message listing all features in a repository.
///
/// Shows each feature with a status icon, slug, status text, branch,
/// and phase progress indicator.
///
/// # Examples
///
/// ```no_run
/// # use std::path::Path;
/// # use coda_server::formatter;
/// // let blocks = formatter::feature_list(Path::new("/repo"), &features);
/// ```
pub fn feature_list(repo_path: &Path, features: &[FeatureState]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Features in `{}`", repo_path.display()))];

    let mut lines = Vec::with_capacity(features.len());
    for f in features {
        let icon = status_icon(f.status);
        let progress = format_phase_progress(f);
        lines.push(format!(
            "{icon} `{}` \u{2014} {} \u{2014} `{}`{progress}",
            f.feature.slug, f.status, f.git.branch,
        ));
    }
    blocks.push(section(&lines.join("\n")));
    blocks.push(context(&format!("_{} feature(s) total_", features.len())));

    blocks
}

/// Builds a Block Kit message for an empty feature list.
///
/// # Examples
///
/// ```no_run
/// # use std::path::Path;
/// # use coda_server::formatter;
/// // let blocks = formatter::empty_feature_list(Path::new("/repo"));
/// ```
pub fn empty_feature_list(repo_path: &Path) -> Vec<serde_json::Value> {
    vec![section(&format!(
        "No features found in `{}`. Use `/coda plan <slug>` to start planning.",
        repo_path.display()
    ))]
}

/// Builds a detailed Block Kit message for a single feature's status.
///
/// Includes status, branch info, phase breakdown with per-phase stats,
/// cumulative totals, and PR link if available.
///
/// # Examples
///
/// ```no_run
/// # use coda_server::formatter;
/// // let blocks = formatter::feature_status(&state);
/// ```
pub fn feature_status(state: &FeatureState) -> Vec<serde_json::Value> {
    let mut blocks = Vec::new();
    let slug = &state.feature.slug;

    blocks.push(header(&format!("Feature: `{slug}`")));

    let icon = status_icon(state.status);
    let mut info_lines = vec![
        format!("*Status:* {icon} {}", state.status),
        format!(
            "*Branch:* `{}` (from `{}`)",
            state.git.branch, state.git.base_branch
        ),
        format!(
            "*Created:* {}",
            state.feature.created_at.format("%Y-%m-%d %H:%M UTC")
        ),
    ];

    if let Some(ref pr) = state.pr {
        info_lines.push(format!(
            "*PR:* <{}|#{} \u{2014} {}>",
            pr.url, pr.number, pr.title
        ));
    }

    blocks.push(section(&info_lines.join("\n")));

    if !state.phases.is_empty() {
        blocks.push(divider());

        let mut phase_lines = Vec::with_capacity(state.phases.len());
        for phase in &state.phases {
            let phase_icon = phase_status_icon(phase.status);
            let mut line = format!("{phase_icon} `{}`", phase.name);

            if phase.status != PhaseStatus::Pending {
                let stats = format_phase_stats(phase.turns, phase.cost_usd, phase.duration_secs);
                line.push_str(&format!(" \u{2014} {stats}"));
            }

            phase_lines.push(line);
        }

        blocks.push(section(&format!("*Phases:*\n{}", phase_lines.join("\n"))));
    }

    if state.total.turns > 0 {
        blocks.push(divider());
        let total_stats = format_phase_stats(
            state.total.turns,
            state.total.cost_usd,
            state.total.duration_secs,
        );
        blocks.push(context(&format!("*Totals:* {total_stats}")));
    }

    blocks
}

/// Builds a Block Kit message showing cleanable worktree candidates
/// with a confirm button.
///
/// The candidates are serialized into the button's `value` field so
/// the interaction handler can reconstruct them without re-scanning.
///
/// # Examples
///
/// ```no_run
/// # use coda_server::formatter;
/// // let blocks = formatter::clean_candidates(&candidates);
/// ```
pub fn clean_candidates(candidates: &[CleanedWorktree]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("Cleanable Worktrees")];

    let mut lines = Vec::with_capacity(candidates.len());
    for c in candidates {
        let pr_info = c
            .pr_number
            .map(|n| format!(" \u{2014} PR #{n}"))
            .unwrap_or_default();
        lines.push(format!(
            ":wastebasket: `{}` \u{2014} `{}`{pr_info} {}",
            c.slug, c.branch, c.pr_state,
        ));
    }
    blocks.push(section(&lines.join("\n")));
    blocks.push(context(&format!(
        "_{} worktree(s) can be cleaned._",
        candidates.len()
    )));

    // Encode candidates in the button value for the interaction handler
    let targets: Vec<CleanTarget> = candidates.iter().map(CleanTarget::from).collect();
    // Serialization of simple structs will not fail in practice
    let value = serde_json::to_string(&targets).unwrap_or_default();

    blocks.push(serde_json::json!({
        "type": "actions",
        "elements": [{
            "type": "button",
            "text": { "type": "plain_text", "text": ":wastebasket: Confirm Clean" },
            "style": "danger",
            "action_id": CLEAN_CONFIRM_ACTION,
            "value": value
        }]
    }));

    blocks
}

/// Builds a Block Kit message showing clean results after worktree removal.
///
/// # Examples
///
/// ```no_run
/// # use coda_server::formatter;
/// // let blocks = formatter::clean_result(&removed);
/// ```
pub fn clean_result(removed: &[CleanedWorktree]) -> Vec<serde_json::Value> {
    let mut lines = Vec::with_capacity(removed.len());
    for c in removed {
        lines.push(format!(
            ":white_check_mark: Removed `{}` (`{}`)",
            c.slug, c.branch,
        ));
    }

    vec![section(&format!(
        "*Cleaned {} worktree(s):*\n{}",
        removed.len(),
        lines.join("\n"),
    ))]
}

/// Builds a Block Kit message for when there are no cleanable worktrees.
///
/// # Examples
///
/// ```no_run
/// # use coda_server::formatter;
/// // let blocks = formatter::no_cleanable_worktrees();
/// ```
pub fn no_cleanable_worktrees() -> Vec<serde_json::Value> {
    vec![section(
        "No merged or closed worktrees to clean. All worktrees have active PRs.",
    )]
}

/// Builds a Block Kit error message with a warning icon.
///
/// # Examples
///
/// ```no_run
/// # use coda_server::formatter;
/// // let blocks = formatter::error("Something went wrong");
/// ```
pub fn error(message: &str) -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": format!(":warning: {message}")
        }
    })]
}

// ---------------------------------------------------------------------------
// Block Kit primitives
// ---------------------------------------------------------------------------

fn header(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "header",
        "text": { "type": "plain_text", "text": text }
    })
}

fn section(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": text }
    })
}

fn context(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "context",
        "elements": [{ "type": "mrkdwn", "text": text }]
    })
}

fn divider() -> serde_json::Value {
    serde_json::json!({ "type": "divider" })
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn status_icon(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Planned => ":clipboard:",
        FeatureStatus::InProgress => ":arrows_counterclockwise:",
        FeatureStatus::Completed => ":white_check_mark:",
        FeatureStatus::Failed => ":x:",
        FeatureStatus::Merged => ":twisted_rightwards_arrows:",
        _ => ":grey_question:",
    }
}

fn phase_status_icon(status: PhaseStatus) -> &'static str {
    match status {
        PhaseStatus::Pending => ":hourglass_flowing_sand:",
        PhaseStatus::Running => ":arrows_counterclockwise:",
        PhaseStatus::Completed => ":white_check_mark:",
        PhaseStatus::Failed => ":x:",
        _ => ":grey_question:",
    }
}

fn display_status_icon(status: PhaseDisplayStatus) -> &'static str {
    match status {
        PhaseDisplayStatus::Pending => ":hourglass_flowing_sand:",
        PhaseDisplayStatus::Running => ":arrows_counterclockwise:",
        PhaseDisplayStatus::Completed => ":white_check_mark:",
        PhaseDisplayStatus::Failed => ":x:",
    }
}

fn format_phase_progress(state: &FeatureState) -> String {
    if state.phases.is_empty() {
        return String::new();
    }
    let completed = state
        .phases
        .iter()
        .filter(|p| p.status == PhaseStatus::Completed)
        .count();
    format!(" \u{2014} phase {completed}/{}", state.phases.len())
}

fn format_phase_stats(turns: u32, cost_usd: f64, duration_secs: u64) -> String {
    let duration = format_duration(duration_secs);
    format!("{turns} turns \u{00b7} ${cost_usd:.2} \u{00b7} {duration}")
}

fn format_duration(secs: u64) -> String {
    if secs == 0 {
        return "\u{2014}".to_string();
    }
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        let mins = secs / 60;
        let remainder = secs % 60;
        if remainder == 0 {
            return format!("{mins}m");
        }
        return format!("{mins}m{remainder}s");
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    format!("{hours}h{mins}m")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use coda_core::CleanedWorktree;
    use coda_core::state::{
        FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseKind, PhaseRecord, PhaseStatus,
        PrInfo, TokenCost, TotalStats,
    };

    use super::*;

    fn make_feature(slug: &str, status: FeatureStatus) -> FeatureState {
        let now = chrono::Utc::now();
        FeatureState {
            feature: FeatureInfo {
                slug: slug.to_string(),
                created_at: now,
                updated_at: now,
            },
            status,
            current_phase: 0,
            git: GitInfo {
                worktree_path: PathBuf::from(format!(".trees/{slug}")),
                branch: format!("feature/{slug}"),
                base_branch: "main".to_string(),
            },
            phases: vec![
                PhaseRecord {
                    name: "dev".to_string(),
                    kind: PhaseKind::Dev,
                    status: PhaseStatus::Completed,
                    started_at: None,
                    completed_at: None,
                    turns: 5,
                    cost_usd: 0.50,
                    cost: TokenCost::default(),
                    duration_secs: 300,
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
            total: TotalStats {
                turns: 5,
                cost_usd: 0.50,
                cost: TokenCost::default(),
                duration_secs: 300,
            },
        }
    }

    fn make_candidate(slug: &str, pr_number: u32, state: &str) -> CleanedWorktree {
        CleanedWorktree {
            slug: slug.to_string(),
            branch: format!("feature/{slug}"),
            pr_number: Some(pr_number),
            pr_state: state.to_string(),
        }
    }

    #[test]
    fn test_should_build_error_block_with_warning() {
        let blocks = error("Something went wrong");
        assert_eq!(blocks.len(), 1);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":warning:"));
        assert!(text.contains("Something went wrong"));
    }

    #[test]
    fn test_should_build_feature_list_with_header_and_summary() {
        let features = vec![
            make_feature("add-auth", FeatureStatus::InProgress),
            make_feature("fix-login", FeatureStatus::Completed),
        ];
        let blocks = feature_list(Path::new("/repo"), &features);

        // Header + section + context = 3 blocks
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "header");

        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("add-auth"));
        assert!(text.contains("fix-login"));

        let summary = blocks[2]["elements"][0]["text"].as_str().unwrap_or("");
        assert!(summary.contains("2 feature(s) total"));
    }

    #[test]
    fn test_should_build_empty_feature_list() {
        let blocks = empty_feature_list(Path::new("/repo"));
        assert_eq!(blocks.len(), 1);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("No features found"));
        assert!(text.contains("/coda plan"));
    }

    #[test]
    fn test_should_build_feature_status_with_phases() {
        let state = make_feature("add-auth", FeatureStatus::InProgress);
        let blocks = feature_status(&state);

        // Header + info + divider + phases + divider + totals = 6
        assert!(blocks.len() >= 4);
        assert_eq!(blocks[0]["type"], "header");

        let info_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(info_text.contains("In Progress") || info_text.contains("in progress"));
        assert!(info_text.contains("feature/add-auth"));
    }

    #[test]
    fn test_should_build_feature_status_with_pr_link() {
        let mut state = make_feature("add-auth", FeatureStatus::Completed);
        state.pr = Some(PrInfo {
            url: "https://github.com/org/repo/pull/42".to_string(),
            number: 42,
            title: "feat: add auth".to_string(),
        });

        let blocks = feature_status(&state);
        let info_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(info_text.contains("PR:"));
        assert!(info_text.contains("#42"));
    }

    #[test]
    fn test_should_build_clean_candidates_with_button() {
        let candidates = vec![
            make_candidate("add-auth", 42, "MERGED"),
            make_candidate("fix-login", 38, "CLOSED"),
        ];
        let blocks = clean_candidates(&candidates);

        // Header + section + context + actions = 4
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0]["type"], "header");
        assert_eq!(blocks[3]["type"], "actions");

        let action_id = blocks[3]["elements"][0]["action_id"].as_str().unwrap_or("");
        assert_eq!(action_id, CLEAN_CONFIRM_ACTION);

        // Button value should be parseable JSON
        let value = blocks[3]["elements"][0]["value"].as_str().unwrap_or("");
        let targets: Vec<CleanTarget> = serde_json::from_str(value).unwrap();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].slug, "add-auth");
    }

    #[test]
    fn test_should_build_clean_result() {
        let removed = vec![make_candidate("add-auth", 42, "MERGED")];
        let blocks = clean_result(&removed);

        assert_eq!(blocks.len(), 1);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("Cleaned 1 worktree(s)"));
        assert!(text.contains("add-auth"));
    }

    #[test]
    fn test_should_build_no_cleanable_message() {
        let blocks = no_cleanable_worktrees();
        assert_eq!(blocks.len(), 1);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("No merged or closed"));
    }

    #[test]
    fn test_should_round_trip_clean_target() {
        let original = CleanTarget {
            slug: "add-auth".to_string(),
            branch: "feature/add-auth".to_string(),
            pr_number: Some(42),
            pr_state: "MERGED".to_string(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let decoded: CleanTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.slug, "add-auth");
        assert_eq!(decoded.pr_number, Some(42));
    }

    #[test]
    fn test_should_convert_clean_target_to_cleaned_worktree() {
        let target = CleanTarget {
            slug: "add-auth".to_string(),
            branch: "feature/add-auth".to_string(),
            pr_number: Some(42),
            pr_state: "MERGED".to_string(),
        };
        let worktree: CleanedWorktree = target.into();
        assert_eq!(worktree.slug, "add-auth");
        assert_eq!(worktree.branch, "feature/add-auth");
        assert_eq!(worktree.pr_number, Some(42));
    }

    #[test]
    fn test_should_convert_cleaned_worktree_to_clean_target() {
        let worktree = make_candidate("fix-login", 38, "CLOSED");
        let target = CleanTarget::from(&worktree);
        assert_eq!(target.slug, "fix-login");
        assert_eq!(target.pr_state, "CLOSED");
    }

    #[test]
    fn test_should_format_duration_seconds() {
        assert_eq!(format_duration(0), "\u{2014}");
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_should_format_duration_minutes() {
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(90), "1m30s");
        assert_eq!(format_duration(3599), "59m59s");
    }

    #[test]
    fn test_should_format_duration_hours() {
        assert_eq!(format_duration(3600), "1h0m");
        assert_eq!(format_duration(5400), "1h30m");
        assert_eq!(format_duration(7200), "2h0m");
    }

    #[test]
    fn test_should_show_status_icons() {
        assert_eq!(status_icon(FeatureStatus::Planned), ":clipboard:");
        assert_eq!(
            status_icon(FeatureStatus::InProgress),
            ":arrows_counterclockwise:"
        );
        assert_eq!(status_icon(FeatureStatus::Completed), ":white_check_mark:");
        assert_eq!(status_icon(FeatureStatus::Failed), ":x:");
        assert_eq!(
            status_icon(FeatureStatus::Merged),
            ":twisted_rightwards_arrows:"
        );
    }

    #[test]
    fn test_should_show_phase_status_icons() {
        assert_eq!(
            phase_status_icon(PhaseStatus::Pending),
            ":hourglass_flowing_sand:"
        );
        assert_eq!(
            phase_status_icon(PhaseStatus::Running),
            ":arrows_counterclockwise:"
        );
        assert_eq!(
            phase_status_icon(PhaseStatus::Completed),
            ":white_check_mark:"
        );
        assert_eq!(phase_status_icon(PhaseStatus::Failed), ":x:");
    }

    #[test]
    fn test_should_format_phase_progress() {
        let state = make_feature("test", FeatureStatus::InProgress);
        let progress = format_phase_progress(&state);
        assert!(progress.contains("1/3")); // 1 completed out of 3
    }

    #[test]
    fn test_should_format_phase_progress_empty_phases() {
        let mut state = make_feature("test", FeatureStatus::Planned);
        state.phases.clear();
        let progress = format_phase_progress(&state);
        assert!(progress.is_empty());
    }

    #[test]
    fn test_should_format_phase_stats() {
        let stats = format_phase_stats(5, 0.50, 300);
        assert!(stats.contains("5 turns"));
        assert!(stats.contains("$0.50"));
        assert!(stats.contains("5m"));
    }

    #[test]
    fn test_should_show_feature_list_with_status_icons() {
        let features = vec![
            make_feature("planned-feat", FeatureStatus::Planned),
            make_feature("active-feat", FeatureStatus::InProgress),
        ];
        let blocks = feature_list(Path::new("/repo"), &features);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":clipboard:"));
        assert!(text.contains(":arrows_counterclockwise:"));
    }

    #[test]
    fn test_should_build_init_progress_with_all_pending() {
        let phases = vec![
            InitPhaseDisplay {
                name: "analyze-repo".to_string(),
                status: PhaseDisplayStatus::Pending,
                duration: None,
                cost_usd: None,
            },
            InitPhaseDisplay {
                name: "setup-project".to_string(),
                status: PhaseDisplayStatus::Pending,
                duration: None,
                cost_usd: None,
            },
        ];
        let blocks = init_progress(&phases);

        assert_eq!(blocks.len(), 2); // header + section
        assert_eq!(blocks[0]["type"], "header");
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("analyze-repo"));
        assert!(text.contains("setup-project"));
        assert!(text.contains(":hourglass_flowing_sand:"));
    }

    #[test]
    fn test_should_build_init_progress_with_completed_phase() {
        let phases = vec![
            InitPhaseDisplay {
                name: "analyze-repo".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(45)),
                cost_usd: Some(0.15),
            },
            InitPhaseDisplay {
                name: "setup-project".to_string(),
                status: PhaseDisplayStatus::Running,
                duration: None,
                cost_usd: None,
            },
        ];
        let blocks = init_progress(&phases);

        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":white_check_mark:"));
        assert!(text.contains("45s"));
        assert!(text.contains("$0.15"));
        assert!(text.contains(":arrows_counterclockwise:"));
    }

    #[test]
    fn test_should_build_init_progress_with_failed_phase() {
        let phases = vec![InitPhaseDisplay {
            name: "analyze-repo".to_string(),
            status: PhaseDisplayStatus::Failed,
            duration: Some(Duration::from_secs(10)),
            cost_usd: None,
        }];
        let blocks = init_progress(&phases);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":x:"));
        assert!(text.contains("10s"));
    }

    #[test]
    fn test_should_build_run_progress_with_slug_in_header() {
        let phases = vec![RunPhaseDisplay {
            name: "setup".to_string(),
            status: PhaseDisplayStatus::Pending,
            duration: None,
            turns: None,
            cost_usd: None,
        }];
        let blocks = run_progress("add-auth", &phases);

        assert_eq!(blocks[0]["type"], "header");
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("add-auth"));
    }

    #[test]
    fn test_should_build_run_progress_with_running_turns() {
        let phases = vec![
            RunPhaseDisplay {
                name: "setup".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(60)),
                turns: Some(3),
                cost_usd: Some(0.25),
            },
            RunPhaseDisplay {
                name: "implement".to_string(),
                status: PhaseDisplayStatus::Running,
                duration: None,
                turns: Some(7),
                cost_usd: None,
            },
        ];
        let blocks = run_progress("add-auth", &phases);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("3 turns"));
        assert!(text.contains("$0.25"));
        assert!(text.contains("1m"));
        assert!(text.contains("7 turns"));
    }

    #[test]
    fn test_should_build_run_progress_with_failed_phase() {
        let phases = vec![RunPhaseDisplay {
            name: "verify".to_string(),
            status: PhaseDisplayStatus::Failed,
            duration: Some(Duration::from_secs(120)),
            turns: Some(5),
            cost_usd: None,
        }];
        let blocks = run_progress("add-auth", &phases);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":x:"));
        assert!(text.contains("failed after 2m"));
    }

    #[test]
    fn test_should_show_display_status_icons() {
        assert_eq!(
            display_status_icon(PhaseDisplayStatus::Pending),
            ":hourglass_flowing_sand:"
        );
        assert_eq!(
            display_status_icon(PhaseDisplayStatus::Running),
            ":arrows_counterclockwise:"
        );
        assert_eq!(
            display_status_icon(PhaseDisplayStatus::Completed),
            ":white_check_mark:"
        );
        assert_eq!(display_status_icon(PhaseDisplayStatus::Failed), ":x:");
    }
}
