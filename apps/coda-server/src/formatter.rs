//! Block Kit message builders for Slack responses.
//!
//! Pure functions that build `serde_json::Value` arrays representing
//! Slack Block Kit blocks. Each function produces a complete message
//! suitable for `chat.postMessage` or `chat.update`.

use std::path::Path;
use std::time::{Duration, Instant};

use coda_core::state::{FeatureState, FeatureStatus, PhaseStatus};
use coda_core::{CleanedWorktree, PrState};
use serde::{Deserialize, Serialize};

/// Action ID for the clean confirm button, used by both the formatter
/// and the interaction handler for routing.
pub const CLEAN_CONFIRM_ACTION: &str = "coda_clean_confirm";

/// Action ID for the repo select dropdown, used by both the formatter
/// and the interaction handler for routing.
pub const REPO_SELECT_ACTION: &str = "coda_repo_select";

/// Action ID for the config key select dropdown.
pub const CONFIG_KEY_SELECT_ACTION: &str = "coda_config_key_select";

/// Action ID for the config value select dropdown.
pub const CONFIG_VALUE_SELECT_ACTION: &str = "coda_config_value_select";

/// Action ID for the init start button (proceed with current config).
pub const INIT_START_ACTION: &str = "coda_init_start";

/// Action ID for the init modify button (enter config modification flow).
pub const INIT_MODIFY_ACTION: &str = "coda_init_modify";

/// Action ID for the init operation select dropdown.
pub const INIT_OP_SELECT_ACTION: &str = "coda_init_op_select";

/// Action ID for the init backend select dropdown.
pub const INIT_BACKEND_SELECT_ACTION: &str = "coda_init_backend_select";

/// Action ID for the init model select dropdown.
pub const INIT_MODEL_SELECT_ACTION: &str = "coda_init_model_select";

/// Action ID for the init effort select dropdown.
pub const INIT_EFFORT_SELECT_ACTION: &str = "coda_init_effort_select";

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
    /// PR lifecycle state.
    pub pr_state: PrState,
}

impl From<&CleanedWorktree> for CleanTarget {
    fn from(w: &CleanedWorktree) -> Self {
        Self {
            slug: w.slug.clone(),
            branch: w.branch.clone(),
            pr_number: w.pr_number,
            pr_state: w.pr_state,
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

/// A GitHub repository entry for the select menu.
///
/// # Examples
///
/// ```
/// use coda_server::formatter::RepoListEntry;
///
/// let entry = RepoListEntry {
///     name_with_owner: "org/repo".to_string(),
///     description: Some("A cool project".to_string()),
/// };
/// assert_eq!(entry.name_with_owner, "org/repo");
/// ```
#[derive(Debug, Clone)]
pub struct RepoListEntry {
    /// Full repository name in `owner/repo` format.
    pub name_with_owner: String,
    /// Optional repository description.
    pub description: Option<String>,
}

/// Maximum length for a Slack `static_select` option text/description (75 chars).
const SELECT_OPTION_TEXT_MAX: usize = 75;

/// Builds a Block Kit message with a `static_select` dropdown listing repos.
///
/// The dropdown uses [`REPO_SELECT_ACTION`] as its `action_id`. Each option's
/// value is the `nameWithOwner` string. Descriptions are truncated to 75
/// characters per Slack's limit.
///
/// # Examples
///
/// ```
/// use coda_server::formatter::{self, RepoListEntry};
///
/// let repos = vec![
///     RepoListEntry {
///         name_with_owner: "org/repo-a".to_string(),
///         description: Some("First repo".to_string()),
///     },
///     RepoListEntry {
///         name_with_owner: "org/repo-b".to_string(),
///         description: None,
///     },
/// ];
/// let blocks = formatter::repo_select_menu(&repos);
/// assert!(blocks.len() >= 2); // header + actions
/// ```
pub fn repo_select_menu(repos: &[RepoListEntry]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("GitHub Repositories")];

    let options: Vec<serde_json::Value> = repos
        .iter()
        .map(|repo| {
            // Slack option text is plain_text with max 75 chars
            let label = truncate_str(&repo.name_with_owner, SELECT_OPTION_TEXT_MAX);
            let mut option = serde_json::json!({
                "text": {
                    "type": "plain_text",
                    "text": label,
                },
                "value": &repo.name_with_owner,
            });
            // Only add description if non-empty (Slack rejects empty plain_text)
            if let Some(ref desc) = repo.description {
                let trimmed = desc.trim();
                if !trimmed.is_empty() {
                    let truncated = truncate_str(trimmed, SELECT_OPTION_TEXT_MAX);
                    option["description"] = serde_json::json!({
                        "type": "plain_text",
                        "text": truncated,
                    });
                }
            }
            option
        })
        .collect();

    blocks.push(serde_json::json!({
        "type": "actions",
        "elements": [{
            "type": "static_select",
            "placeholder": {
                "type": "plain_text",
                "text": "Select a repository...",
            },
            "action_id": REPO_SELECT_ACTION,
            "options": options,
        }],
    }));

    blocks.push(context(&format!("_{} repo(s) found_", repos.len())));

    blocks
}

/// Truncates a string to `max_len` bytes, respecting UTF-8 char boundaries.
fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        return s;
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
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
///     started_at: None,
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
    /// When the phase started executing (for live elapsed display).
    pub started_at: Option<Instant>,
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
///         started_at: None,
///     },
///     InitPhaseDisplay {
///         name: "setup-project".to_string(),
///         status: PhaseDisplayStatus::Running,
///         duration: None,
///         cost_usd: None,
///         started_at: None,
///     },
/// ];
/// let blocks = formatter::init_progress(&phases);
/// assert!(!blocks.is_empty());
/// ```
pub fn init_progress(phases: &[InitPhaseDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("CODA Init")];
    blocks.push(section(&format_init_phase_lines(phases)));
    blocks
}

/// Builds a Block Kit success message for a completed init operation.
///
/// Shows `"CODA Init \u{2705}"` header with all phase metrics and a
/// context line summarising total duration and cost.
pub fn init_success(phases: &[InitPhaseDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("CODA Init \u{2705}")];
    blocks.push(section(&format_init_phase_lines(phases)));

    let total_duration: Duration = phases.iter().filter_map(|p| p.duration).sum();
    let total_cost: f64 = phases.iter().filter_map(|p| p.cost_usd).sum();
    blocks.push(context(&format!(
        "Completed in {} \u{00b7} ${total_cost:.2}",
        format_duration(total_duration.as_secs()),
    )));

    blocks
}

/// Builds a Block Kit failure message for a failed init operation.
///
/// Preserves completed phase metrics and appends a truncated error
/// summary so the user can see both progress and the failure reason.
pub fn init_failure(phases: &[InitPhaseDisplay], error: &str) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("CODA Init \u{274c}")];
    blocks.push(section(&format_init_phase_lines(phases)));

    let error_preview = truncate_str(error, 300);
    blocks.push(section(&format!(":warning: {error_preview}")));

    blocks
}

/// Builds a Block Kit cancellation message for an init operation.
///
/// Shows completed phase metrics with a cancellation indicator instead
/// of a failure, since cancellation is intentional (e.g., user abort,
/// server shutdown).
pub fn init_cancelled(phases: &[InitPhaseDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header("CODA Init \u{1f6d1}")];
    blocks.push(section(&format_init_phase_lines(phases)));
    blocks.push(context("Cancelled — run `/coda init` to retry."));
    blocks
}

/// Formats init phase lines for reuse across progress/success/failure.
fn format_init_phase_lines(phases: &[InitPhaseDisplay]) -> String {
    if phases.is_empty() {
        return "Starting\u{2026}".to_string();
    }

    let mut lines = Vec::with_capacity(phases.len());
    for phase in phases {
        let icon = display_status_icon(phase.status);
        let mut line = format!("{icon} `{}`", phase.name);

        match phase.status {
            PhaseDisplayStatus::Running => {
                if let Some(started) = phase.started_at {
                    let elapsed = started.elapsed().as_secs();
                    line.push_str(&format!(" \u{2014} {}", format_duration(elapsed),));
                }
            }
            PhaseDisplayStatus::Completed => {
                if let Some(duration) = phase.duration {
                    line.push_str(&format!(
                        " \u{2014} {}",
                        format_duration(duration.as_secs()),
                    ));
                }
                if let Some(cost) = phase.cost_usd {
                    line.push_str(&format!(" \u{00b7} ${cost:.2}"));
                }
            }
            PhaseDisplayStatus::Failed => {
                if let Some(duration) = phase.duration {
                    line.push_str(&format!(
                        " \u{2014} failed after {}",
                        format_duration(duration.as_secs()),
                    ));
                }
            }
            PhaseDisplayStatus::Pending => {}
        }

        lines.push(line);
    }

    lines.join("\n")
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

    let text = if lines.is_empty() {
        "Starting\u{2026}".to_string()
    } else {
        lines.join("\n")
    };
    blocks.push(section(&text));
    blocks
}

/// Builds a Block Kit failure message for a failed run operation.
///
/// Preserves completed phase metrics and appends a truncated error
/// summary so the user can see both progress and the failure reason.
pub fn run_failure(
    feature_slug: &str,
    phases: &[RunPhaseDisplay],
    error: &str,
) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Run: `{feature_slug}` \u{274c}"))];

    if !phases.is_empty() {
        let mut lines = Vec::with_capacity(phases.len());
        for phase in phases {
            let icon = display_status_icon(phase.status);
            let mut line = format!("{icon} `{}`", phase.name);
            if let Some(duration) = phase.duration {
                line.push_str(&format!(
                    " \u{2014} {}",
                    format_duration(duration.as_secs())
                ));
            }
            lines.push(line);
        }
        blocks.push(section(&lines.join("\n")));
    }

    let error_preview = truncate_str(error, 300);
    blocks.push(section(&format!(":warning: {error_preview}")));

    blocks
}

/// Builds a Block Kit cancellation message for a cancelled run operation.
///
/// Shows completed phase metrics with a cancellation indicator instead
/// of a failure, since cancellation is intentional (e.g., user abort,
/// server shutdown). The run can be resumed with `coda run`.
pub fn run_cancelled(feature_slug: &str, phases: &[RunPhaseDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Run: `{feature_slug}` \u{1f6d1}"))];

    if !phases.is_empty() {
        let mut lines = Vec::with_capacity(phases.len());
        for phase in phases {
            let icon = display_status_icon(phase.status);
            let mut line = format!("{icon} `{}`", phase.name);
            if let Some(duration) = phase.duration {
                line.push_str(&format!(
                    " \u{2014} {}",
                    format_duration(duration.as_secs())
                ));
            }
            lines.push(line);
        }
        blocks.push(section(&lines.join("\n")));
    }

    blocks.push(context("Cancelled \u{2014} run `/coda run` to resume."));

    blocks
}

/// Builds a cancellation notification for a cancelled run (posted as a
/// new channel message to trigger a Slack notification).
pub fn run_cancellation_notification(slug: &str) -> Vec<serde_json::Value> {
    vec![section(&format!(
        "\u{1f6d1} Run `{slug}` was cancelled \u{2014} resume with `/coda run {slug}`"
    ))]
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

/// Builds a Block Kit thread parent message for a plan session.
///
/// Displays the feature slug and current phase indicator (e.g.,
/// "Discussing", "Approved", "Finalized", "Cancelled"). Updated
/// in-place via `chat.update` as the session progresses.
///
/// # Examples
///
/// ```
/// use coda_server::formatter;
///
/// let blocks = formatter::plan_thread_header("add-auth", "Discussing");
/// assert_eq!(blocks.len(), 2);
/// let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
/// assert!(header_text.contains("add-auth"));
/// ```
pub fn plan_thread_header(feature_slug: &str, phase: &str) -> Vec<serde_json::Value> {
    let phase_icon = match phase {
        "Discussing" => ":speech_balloon:",
        "Approved" => ":white_check_mark:",
        "Finalized" => ":rocket:",
        "Cancelled" => ":no_entry_sign:",
        _ => ":clipboard:",
    };

    vec![
        header(&format!("Plan: `{feature_slug}`")),
        section(&format!("{phase_icon} *Status:* {phase}")),
    ]
}

/// Builds a Block Kit notification message posted after a run finishes.
///
/// This is posted as a **new** channel message (not an update) so that
/// Slack delivers a notification to users. The existing progress message
/// is still updated in-place separately.
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
///         status: PhaseDisplayStatus::Completed,
///         duration: Some(Duration::from_secs(120)),
///         turns: Some(10),
///         cost_usd: Some(0.98),
///     },
/// ];
/// let blocks = formatter::run_completion_notification("add-auth", true, &phases, None, None);
/// assert!(!blocks.is_empty());
/// ```
pub fn run_completion_notification(
    slug: &str,
    success: bool,
    phases: &[RunPhaseDisplay],
    pr_url: Option<&str>,
    error_msg: Option<&str>,
) -> Vec<serde_json::Value> {
    if success {
        let total_turns: u32 = phases.iter().filter_map(|p| p.turns).sum();
        let total_cost: f64 = phases.iter().filter_map(|p| p.cost_usd).sum();
        let total_secs: u64 = phases
            .iter()
            .filter_map(|p| p.duration.map(|d| d.as_secs()))
            .sum();
        let duration = format_duration(total_secs);

        let mut text = format!(
            ":white_check_mark: Run `{slug}` completed \u{2014} \
             {total_turns} turns \u{00b7} ${total_cost:.2} \u{00b7} {duration}"
        );

        if let Some(url) = pr_url {
            text.push_str(&format!("\n:link: PR: <{url}>"));
        }

        vec![section(&text)]
    } else {
        let failed: Vec<&str> = phases
            .iter()
            .filter(|p| p.status == PhaseDisplayStatus::Failed)
            .map(|p| p.name.as_str())
            .collect();

        let detail = if let Some(err) = error_msg {
            truncate_str(err, 200).to_string()
        } else if failed.is_empty() {
            "unknown".to_string()
        } else {
            format!("failed phase(s): {}", failed.join(", "))
        };

        vec![section(&format!(
            ":x: Run `{slug}` failed \u{2014} {detail}"
        ))]
    }
}

/// Display state for a single repo sync step in the progress message.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_server::formatter::{RepoStepDisplay, PhaseDisplayStatus};
///
/// let step = RepoStepDisplay {
///     label: "Cloning".to_string(),
///     status: PhaseDisplayStatus::Completed,
///     duration: Some(Duration::from_secs(12)),
///     started_at: None,
///     detail: None,
/// };
/// assert_eq!(step.label, "Cloning");
/// ```
#[derive(Debug, Clone)]
pub struct RepoStepDisplay {
    /// Step label (e.g., `"Fetching"`, `"Cloning"`).
    pub label: String,
    /// Current display status.
    pub status: PhaseDisplayStatus,
    /// Duration if completed or failed.
    pub duration: Option<Duration>,
    /// When the step started executing (for live elapsed display).
    pub started_at: Option<Instant>,
    /// Optional detail line (e.g., `"Receiving objects: 45% (556/1234)"`).
    pub detail: Option<String>,
}

/// Builds a Block Kit progress message for a repo sync (clone or update).
///
/// Shows a header and a list of steps with status icons, durations, and
/// optional detail text. Updated in-place via `chat.update` as steps progress.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use coda_server::formatter::{self, RepoStepDisplay, PhaseDisplayStatus};
///
/// let steps = vec![
///     RepoStepDisplay {
///         label: "Cloning".to_string(),
///         status: PhaseDisplayStatus::Running,
///         duration: None,
///         started_at: None,
///         detail: Some("Receiving objects: 45%".to_string()),
///     },
/// ];
/// let blocks = formatter::repo_sync_progress("org/repo", &steps);
/// assert!(!blocks.is_empty());
/// ```
pub fn repo_sync_progress(repo_name: &str, steps: &[RepoStepDisplay]) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Syncing `{repo_name}`"))];
    blocks.push(section(&format_repo_step_lines(steps)));
    blocks
}

/// Builds a Block Kit success message for a completed repo sync.
///
/// Shows a checkmark header with all step metrics and the bind label.
pub fn repo_sync_success(
    repo_name: &str,
    steps: &[RepoStepDisplay],
    bind_label: &str,
) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Syncing `{repo_name}` \u{2705}"))];
    blocks.push(section(&format_repo_step_lines(steps)));

    let total_duration: Duration = steps.iter().filter_map(|s| s.duration).sum();
    blocks.push(context(&format!(
        "{bind_label} \u{00b7} completed in {}",
        format_duration(total_duration.as_secs()),
    )));

    blocks
}

/// Builds a Block Kit failure message for a failed repo sync.
///
/// Preserves completed step metrics and appends a truncated error
/// summary so the user can see both progress and the failure reason.
pub fn repo_sync_failure(
    repo_name: &str,
    steps: &[RepoStepDisplay],
    error: &str,
) -> Vec<serde_json::Value> {
    let mut blocks = vec![header(&format!("Syncing `{repo_name}` \u{274c}"))];
    blocks.push(section(&format_repo_step_lines(steps)));

    let error_preview = truncate_str(error, 300);
    blocks.push(section(&format!(":warning: {error_preview}")));

    blocks
}

/// Formats repo sync step lines for reuse across progress/success/failure.
fn format_repo_step_lines(steps: &[RepoStepDisplay]) -> String {
    if steps.is_empty() {
        return "Starting\u{2026}".to_string();
    }

    let mut lines = Vec::with_capacity(steps.len());
    for step in steps {
        let icon = display_status_icon(step.status);
        let mut line = format!("{icon} `{}`", step.label);

        match step.status {
            PhaseDisplayStatus::Running => {
                if let Some(started) = step.started_at {
                    let elapsed = started.elapsed().as_secs();
                    line.push_str(&format!(" \u{2014} {}", format_duration(elapsed)));
                }
                if let Some(ref detail) = step.detail {
                    line.push_str(&format!(" \u{2014} _{detail}_"));
                }
            }
            PhaseDisplayStatus::Completed => {
                if let Some(duration) = step.duration {
                    line.push_str(&format!(
                        " \u{2014} {}",
                        format_duration(duration.as_secs()),
                    ));
                }
            }
            PhaseDisplayStatus::Failed => {
                if let Some(duration) = step.duration {
                    line.push_str(&format!(
                        " \u{2014} failed after {}",
                        format_duration(duration.as_secs()),
                    ));
                }
            }
            PhaseDisplayStatus::Pending => {}
        }

        lines.push(line);
    }

    lines.join("\n")
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

pub(crate) fn format_duration(secs: u64) -> String {
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

// ── Config interactive select ──────────────────────────────────────

/// Builds a config key selection dropdown.
///
/// Shows all settable keys with their current values as descriptions.
/// The selected key is sent as the option value and routed via
/// [`CONFIG_KEY_SELECT_ACTION`].
pub fn config_key_select(keys: &[coda_core::ConfigKeyDescriptor]) -> Vec<serde_json::Value> {
    let options: Vec<serde_json::Value> = keys
        .iter()
        .map(|d| {
            let label = truncate_str(&d.key, SELECT_OPTION_TEXT_MAX);
            let desc = format!("{} [current: {}]", d.label, d.current_value);
            let desc_truncated = truncate_str(&desc, SELECT_OPTION_TEXT_MAX);
            serde_json::json!({
                "text": { "type": "plain_text", "text": label },
                "description": { "type": "plain_text", "text": desc_truncated },
                "value": &d.key,
            })
        })
        .collect();

    vec![
        header("Config Set"),
        serde_json::json!({
            "type": "actions",
            "elements": [{
                "type": "static_select",
                "placeholder": { "type": "plain_text", "text": "Select a config key..." },
                "action_id": CONFIG_KEY_SELECT_ACTION,
                "options": options,
            }],
        }),
        context("_Select a key, then choose a value_"),
    ]
}

/// Builds a config value selection dropdown for a specific key.
///
/// For `Enum` and `Bool` types, produces a `static_select` with the
/// valid options. For `Suggest` types, adds suggestions plus a hint
/// about free-text input. For `String`/numeric types, shows usage
/// instructions since Slack has no free-text input in dropdowns.
pub fn config_value_select(descriptor: &coda_core::ConfigKeyDescriptor) -> Vec<serde_json::Value> {
    match &descriptor.value_type {
        coda_core::ConfigValueType::Enum(options) => {
            config_value_dropdown(&descriptor.key, &descriptor.label, options)
        }
        coda_core::ConfigValueType::Bool => {
            let options = vec!["true".to_string(), "false".to_string()];
            config_value_dropdown(&descriptor.key, &descriptor.label, &options)
        }
        coda_core::ConfigValueType::Suggest(suggestions) => {
            let mut blocks = config_value_dropdown(&descriptor.key, &descriptor.label, suggestions);
            blocks.push(context(&format!(
                "_Or type `/coda config set {} <custom-value>`_",
                descriptor.key,
            )));
            blocks
        }
        coda_core::ConfigValueType::U32
        | coda_core::ConfigValueType::F64
        | coda_core::ConfigValueType::String => vec![
            section(&format!(
                "*{}* (`{}`)\nCurrent: `{}`",
                descriptor.label, descriptor.key, descriptor.current_value,
            )),
            context(&format!(
                "_Type `/coda config set {} <value>` to update_",
                descriptor.key,
            )),
        ],
    }
}

/// Builds a `static_select` dropdown for config value options.
fn config_value_dropdown(key: &str, label: &str, options: &[String]) -> Vec<serde_json::Value> {
    let select_options: Vec<serde_json::Value> = options
        .iter()
        .map(|opt| {
            // Encode "key=value" in the option value for the interaction handler
            let encoded = format!("{key}={opt}");
            serde_json::json!({
                "text": { "type": "plain_text", "text": truncate_str(opt, SELECT_OPTION_TEXT_MAX) },
                "value": encoded,
            })
        })
        .collect();

    vec![
        section(&format!("*{label}* (`{key}`)")),
        serde_json::json!({
            "type": "actions",
            "elements": [{
                "type": "static_select",
                "placeholder": { "type": "plain_text", "text": "Select a value..." },
                "action_id": CONFIG_VALUE_SELECT_ACTION,
                "options": select_options,
            }],
        }),
    ]
}

// ---------------------------------------------------------------------------
// Init config interactive flow formatters
// ---------------------------------------------------------------------------

/// Builds a config preview table with Start/Modify buttons for the init flow.
///
/// Shows the resolved config for all operations (init/plan/run/review/verify)
/// and two action buttons: "Start Init" and "Modify Settings".
pub fn init_config_preview(
    summary: &coda_core::ResolvedConfigSummary,
    force: bool,
) -> Vec<serde_json::Value> {
    let force_str = if force { "true" } else { "false" };

    let rows: Vec<String> = [
        ("init", &summary.init),
        ("plan", &summary.plan),
        ("run", &summary.run),
        ("review", &summary.review),
        ("verify", &summary.verify),
    ]
    .iter()
    .map(|(name, resolved)| {
        format!(
            "  {:<10} {:<10} {:<26} {}",
            name, resolved.backend, resolved.model, resolved.effort,
        )
    })
    .collect();

    let table_header = format!(
        "  {:<10} {:<10} {:<26} {}",
        "Operation", "Backend", "Model", "Effort"
    );
    let separator = format!("  {}", "─".repeat(56));
    let table = format!("```\n{table_header}\n{separator}\n{}\n```", rows.join("\n"));

    vec![
        header("Init — Config Preview"),
        section(&table),
        serde_json::json!({
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "text": { "type": "plain_text", "text": "Start Init" },
                    "action_id": INIT_START_ACTION,
                    "value": force_str,
                    "style": "primary",
                },
                {
                    "type": "button",
                    "text": { "type": "plain_text", "text": "Modify Settings" },
                    "action_id": INIT_MODIFY_ACTION,
                    "value": force_str,
                },
            ],
        }),
    ]
}

/// Builds an operation select dropdown for the init config modification flow.
///
/// Each option shows the operation name with its current backend/model/effort
/// inline. The selected value encodes `"{op}|{force}"` for the next step.
pub fn init_config_op_select(
    summaries: &[coda_core::OperationSummary],
    force: bool,
) -> Vec<serde_json::Value> {
    let force_str = if force { "true" } else { "false" };

    let options: Vec<serde_json::Value> = summaries
        .iter()
        .map(|op| {
            let desc = format!("{} / {} / {}", op.backend, op.model, op.effort);
            serde_json::json!({
                "text": { "type": "plain_text", "text": &op.label },
                "description": { "type": "plain_text", "text": truncate_str(&desc, SELECT_OPTION_TEXT_MAX) },
                "value": format!("{}|{force_str}", op.name),
            })
        })
        .collect();

    vec![
        header("Init — Select Operation"),
        serde_json::json!({
            "type": "actions",
            "elements": [{
                "type": "static_select",
                "placeholder": { "type": "plain_text", "text": "Select an operation..." },
                "action_id": INIT_OP_SELECT_ACTION,
                "options": options,
            }],
        }),
        context("_Select an operation to configure its backend, model, and effort_"),
    ]
}

/// Builds a backend select dropdown for the init config modification flow.
///
/// The selected value encodes `"{op}|{backend}|{force}"` for the next step.
pub fn init_config_backend_select(
    op: &str,
    options: &[String],
    current: &str,
    force: bool,
) -> Vec<serde_json::Value> {
    let force_str = if force { "true" } else { "false" };

    let select_options: Vec<serde_json::Value> = options
        .iter()
        .map(|backend| {
            serde_json::json!({
                "text": { "type": "plain_text", "text": backend },
                "value": format!("{op}|{backend}|{force_str}"),
            })
        })
        .collect();

    let initial_option = options.iter().position(|b| b == current).map(|idx| {
        serde_json::json!({
            "text": { "type": "plain_text", "text": &options[idx] },
            "value": format!("{op}|{}|{force_str}", options[idx]),
        })
    });

    let mut select = serde_json::json!({
        "type": "static_select",
        "placeholder": { "type": "plain_text", "text": "Select backend..." },
        "action_id": INIT_BACKEND_SELECT_ACTION,
        "options": select_options,
    });
    if let Some(initial) = initial_option {
        select["initial_option"] = initial;
    }

    vec![
        header(&format!("Init — {op} Backend")),
        serde_json::json!({
            "type": "actions",
            "elements": [select],
        }),
        context(&format!("_Current: `{current}`_")),
    ]
}

/// Builds a model select dropdown for the init config modification flow.
///
/// The selected value encodes `"{op}|{backend}|{model}|{force}"` for the
/// next step. Includes a hint about using `/coda config set` for custom models.
pub fn init_config_model_select(
    op: &str,
    backend: &str,
    suggestions: &[String],
    current: &str,
    force: bool,
) -> Vec<serde_json::Value> {
    let force_str = if force { "true" } else { "false" };

    let select_options: Vec<serde_json::Value> = suggestions
        .iter()
        .map(|model| {
            serde_json::json!({
                "text": { "type": "plain_text", "text": truncate_str(model, SELECT_OPTION_TEXT_MAX) },
                "value": format!("{op}|{backend}|{model}|{force_str}"),
            })
        })
        .collect();

    let initial_option = suggestions
        .iter()
        .position(|m| m == current)
        .map(|idx| {
            serde_json::json!({
                "text": { "type": "plain_text", "text": truncate_str(&suggestions[idx], SELECT_OPTION_TEXT_MAX) },
                "value": format!("{op}|{backend}|{}|{force_str}", suggestions[idx]),
            })
        });

    let mut select = serde_json::json!({
        "type": "static_select",
        "placeholder": { "type": "plain_text", "text": "Select model..." },
        "action_id": INIT_MODEL_SELECT_ACTION,
        "options": select_options,
    });
    if let Some(initial) = initial_option {
        select["initial_option"] = initial;
    }

    vec![
        header(&format!("Init — {op} Model")),
        serde_json::json!({
            "type": "actions",
            "elements": [select],
        }),
        context(&format!(
            "_Current: `{current}` · For custom models use `/coda config set agents.{op}.model <name>`_"
        )),
    ]
}

/// Builds an effort select dropdown for the init config modification flow.
///
/// The selected value encodes `"{op}|{backend}|{model}|{effort}|{force}"`
/// which triggers the batch config apply at the interaction handler.
pub fn init_config_effort_select(
    op: &str,
    backend: &str,
    model: &str,
    options: &[String],
    current: &str,
    force: bool,
) -> Vec<serde_json::Value> {
    let force_str = if force { "true" } else { "false" };

    let select_options: Vec<serde_json::Value> = options
        .iter()
        .map(|effort| {
            serde_json::json!({
                "text": { "type": "plain_text", "text": effort },
                "value": format!("{op}|{backend}|{model}|{effort}|{force_str}"),
            })
        })
        .collect();

    let initial_option = options.iter().position(|e| e == current).map(|idx| {
        serde_json::json!({
            "text": { "type": "plain_text", "text": &options[idx] },
            "value": format!("{op}|{backend}|{model}|{}|{force_str}", options[idx]),
        })
    });

    let mut select = serde_json::json!({
        "type": "static_select",
        "placeholder": { "type": "plain_text", "text": "Select effort..." },
        "action_id": INIT_EFFORT_SELECT_ACTION,
        "options": select_options,
    });
    if let Some(initial) = initial_option {
        select["initial_option"] = initial;
    }

    vec![
        header(&format!("Init — {op} Effort")),
        serde_json::json!({
            "type": "actions",
            "elements": [select],
        }),
        context(&format!(
            "_Current: `{current}` · Backend: `{backend}` · Model: `{model}`_"
        )),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use coda_core::state::{
        FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseKind, PhaseRecord, PhaseStatus,
        PrInfo, TokenCost, TotalStats,
    };
    use coda_core::{CleanedWorktree, PrState};

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
            pr_state: state.parse::<PrState>().unwrap_or(PrState::Merged),
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
            pr_state: PrState::Merged,
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
            pr_state: PrState::Merged,
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
        assert_eq!(target.pr_state, PrState::Closed);
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
                started_at: None,
            },
            InitPhaseDisplay {
                name: "setup-project".to_string(),
                status: PhaseDisplayStatus::Pending,
                duration: None,
                cost_usd: None,
                started_at: None,
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
                started_at: None,
            },
            InitPhaseDisplay {
                name: "setup-project".to_string(),
                status: PhaseDisplayStatus::Running,
                duration: None,
                cost_usd: None,
                started_at: Some(Instant::now()),
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
            started_at: None,
        }];
        let blocks = init_progress(&phases);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":x:"));
        assert!(text.contains("failed after 10s"));
    }

    #[test]
    fn test_should_build_init_success_with_totals() {
        let phases = vec![
            InitPhaseDisplay {
                name: "analyze-repo".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(17)),
                cost_usd: Some(0.06),
                started_at: None,
            },
            InitPhaseDisplay {
                name: "setup-project".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(40)),
                cost_usd: Some(0.12),
                started_at: None,
            },
        ];
        let blocks = init_success(&phases);

        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("\u{2705}"));
        let section_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(section_text.contains("17s"));
        assert!(section_text.contains("$0.12"));
        // context block with totals
        let ctx = blocks[2]["elements"][0]["text"].as_str().unwrap_or("");
        assert!(ctx.contains("57s"));
        assert!(ctx.contains("$0.18"));
    }

    #[test]
    fn test_should_build_init_failure_with_error() {
        let phases = vec![InitPhaseDisplay {
            name: "analyze-repo".to_string(),
            status: PhaseDisplayStatus::Failed,
            duration: Some(Duration::from_secs(3)),
            cost_usd: Some(0.0),
            started_at: None,
        }];
        let blocks = init_failure(&phases, "agent error: Please run /login");

        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("\u{274c}"));
        let error_text = blocks[2]["text"]["text"].as_str().unwrap_or("");
        assert!(error_text.contains("Please run /login"));
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

    #[test]
    fn test_should_build_plan_thread_header_discussing() {
        let blocks = plan_thread_header("add-auth", "Discussing");
        assert_eq!(blocks.len(), 2);

        assert_eq!(blocks[0]["type"], "header");
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("add-auth"));

        let status_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(status_text.contains(":speech_balloon:"));
        assert!(status_text.contains("Discussing"));
    }

    #[test]
    fn test_should_build_plan_thread_header_approved() {
        let blocks = plan_thread_header("fix-login", "Approved");
        let status_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(status_text.contains(":white_check_mark:"));
        assert!(status_text.contains("Approved"));
    }

    #[test]
    fn test_should_build_plan_thread_header_finalized() {
        let blocks = plan_thread_header("add-api", "Finalized");
        let status_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(status_text.contains(":rocket:"));
        assert!(status_text.contains("Finalized"));
    }

    #[test]
    fn test_should_build_plan_thread_header_cancelled() {
        let blocks = plan_thread_header("add-api", "Cancelled");
        let status_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(status_text.contains(":no_entry_sign:"));
        assert!(status_text.contains("Cancelled"));
    }

    #[test]
    fn test_should_build_plan_thread_header_unknown_phase() {
        let blocks = plan_thread_header("add-api", "CustomPhase");
        let status_text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(status_text.contains(":clipboard:"));
        assert!(status_text.contains("CustomPhase"));
    }

    #[test]
    fn test_should_build_repo_select_menu() {
        let repos = vec![
            RepoListEntry {
                name_with_owner: "org/repo-a".to_string(),
                description: Some("First repo".to_string()),
            },
            RepoListEntry {
                name_with_owner: "org/repo-b".to_string(),
                description: None,
            },
        ];
        let blocks = repo_select_menu(&repos);

        // header + actions + context = 3
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "header");
        assert_eq!(blocks[1]["type"], "actions");

        let select = &blocks[1]["elements"][0];
        assert_eq!(select["type"], "static_select");
        assert_eq!(select["action_id"], REPO_SELECT_ACTION);

        let options = select["options"].as_array().expect("options array");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["value"], "org/repo-a");
        assert_eq!(options[1]["value"], "org/repo-b");

        // First option has description, second does not
        assert!(options[0]["description"]["text"].as_str().is_some());
        assert!(options[1].get("description").is_none());

        let context_text = blocks[2]["elements"][0]["text"].as_str().unwrap_or("");
        assert!(context_text.contains("2 repo(s) found"));
    }

    #[test]
    fn test_should_truncate_long_description_in_select_menu() {
        let long_desc = "a".repeat(100);
        let repos = vec![RepoListEntry {
            name_with_owner: "org/repo".to_string(),
            description: Some(long_desc),
        }];
        let blocks = repo_select_menu(&repos);

        let desc = blocks[1]["elements"][0]["options"][0]["description"]["text"]
            .as_str()
            .expect("description text");
        assert!(desc.len() <= SELECT_OPTION_TEXT_MAX);
    }

    #[test]
    fn test_should_skip_empty_description_in_select_menu() {
        let repos = vec![RepoListEntry {
            name_with_owner: "org/repo".to_string(),
            description: Some(String::new()),
        }];
        let blocks = repo_select_menu(&repos);

        // Empty description should not produce a description field
        assert!(
            blocks[1]["elements"][0]["options"][0]
                .get("description")
                .is_none()
        );
    }

    #[test]
    fn test_should_truncate_long_name_in_select_menu() {
        let long_name = format!("org/{}", "a".repeat(100));
        let repos = vec![RepoListEntry {
            name_with_owner: long_name.clone(),
            description: None,
        }];
        let blocks = repo_select_menu(&repos);

        let text = blocks[1]["elements"][0]["options"][0]["text"]["text"]
            .as_str()
            .expect("text");
        assert!(text.len() <= SELECT_OPTION_TEXT_MAX);
        // Value should still contain the full name for cloning
        let value = blocks[1]["elements"][0]["options"][0]["value"]
            .as_str()
            .expect("value");
        assert_eq!(value, long_name);
    }

    #[test]
    fn test_should_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello");
        // Multi-byte: "你好" = 6 bytes, truncate at 4 should give "你" (3 bytes)
        assert_eq!(truncate_str("你好", 4), "你");
    }

    #[test]
    fn test_should_build_success_completion_notification() {
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
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(120)),
                turns: Some(10),
                cost_usd: Some(0.98),
            },
        ];
        let blocks = run_completion_notification("add-auth", true, &phases, None, None);
        assert_eq!(blocks.len(), 1);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":white_check_mark:"));
        assert!(text.contains("add-auth"));
        assert!(text.contains("13 turns"));
        assert!(text.contains("$1.23"));
        assert!(text.contains("3m"));
    }

    #[test]
    fn test_should_build_success_completion_notification_with_pr() {
        let phases = vec![RunPhaseDisplay {
            name: "setup".to_string(),
            status: PhaseDisplayStatus::Completed,
            duration: Some(Duration::from_secs(60)),
            turns: Some(3),
            cost_usd: Some(0.25),
        }];
        let blocks = run_completion_notification(
            "add-auth",
            true,
            &phases,
            Some("https://github.com/org/repo/pull/42"),
            None,
        );
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":link:"));
        assert!(text.contains("pull/42"));
    }

    #[test]
    fn test_should_build_failure_completion_notification() {
        let phases = vec![
            RunPhaseDisplay {
                name: "setup".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(60)),
                turns: Some(3),
                cost_usd: Some(0.25),
            },
            RunPhaseDisplay {
                name: "review".to_string(),
                status: PhaseDisplayStatus::Failed,
                duration: Some(Duration::from_secs(30)),
                turns: Some(2),
                cost_usd: Some(0.10),
            },
        ];
        let blocks = run_completion_notification("add-auth", false, &phases, None, None);
        assert_eq!(blocks.len(), 1);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":x:"));
        assert!(text.contains("add-auth"));
        assert!(text.contains("review"));
    }

    #[test]
    fn test_should_build_failure_notification_with_no_failed_phases() {
        let blocks = run_completion_notification("add-auth", false, &[], None, None);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("unknown"));
    }

    #[test]
    fn test_should_build_failure_notification_with_error_message() {
        let blocks = run_completion_notification(
            "add-auth",
            false,
            &[],
            None,
            Some("Agent error: connection timeout"),
        );
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":x:"));
        assert!(text.contains("connection timeout"));
    }

    // -----------------------------------------------------------------------
    // Repo sync progress
    // -----------------------------------------------------------------------

    #[test]
    fn test_should_build_repo_sync_progress_with_empty_steps() {
        let blocks = repo_sync_progress("org/repo", &[]);
        assert_eq!(blocks.len(), 2); // header + section
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("Starting\u{2026}"));
    }

    #[test]
    fn test_should_build_repo_sync_progress_with_running_step_and_detail() {
        let steps = vec![RepoStepDisplay {
            label: "Cloning".to_string(),
            status: PhaseDisplayStatus::Running,
            duration: None,
            started_at: None,
            detail: Some("Receiving objects: 45%".to_string()),
        }];
        let blocks = repo_sync_progress("org/repo", &steps);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":arrows_counterclockwise:"));
        assert!(text.contains("Cloning"));
        assert!(text.contains("Receiving objects: 45%"));
    }

    #[test]
    fn test_should_build_repo_sync_progress_with_completed_step() {
        let steps = vec![RepoStepDisplay {
            label: "Fetching".to_string(),
            status: PhaseDisplayStatus::Completed,
            duration: Some(Duration::from_secs(5)),
            started_at: None,
            detail: None,
        }];
        let blocks = repo_sync_progress("org/repo", &steps);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":white_check_mark:"));
        assert!(text.contains("5s"));
    }

    #[test]
    fn test_should_build_repo_sync_success_with_bind_label() {
        let steps = vec![
            RepoStepDisplay {
                label: "Fetching".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(3)),
                started_at: None,
                detail: None,
            },
            RepoStepDisplay {
                label: "Pulling".to_string(),
                status: PhaseDisplayStatus::Completed,
                duration: Some(Duration::from_secs(2)),
                started_at: None,
                detail: None,
            },
        ];
        let blocks = repo_sync_success("org/repo", &steps, "Updated and bound (branch `main`)");
        assert_eq!(blocks.len(), 3); // header + section + context
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("\u{2705}"));
        let ctx = blocks[2]["elements"][0]["text"].as_str().unwrap_or("");
        assert!(ctx.contains("Updated and bound"));
        assert!(ctx.contains("5s"));
    }

    #[test]
    fn test_should_build_repo_sync_failure_with_error() {
        let steps = vec![RepoStepDisplay {
            label: "Cloning".to_string(),
            status: PhaseDisplayStatus::Failed,
            duration: Some(Duration::from_secs(8)),
            started_at: None,
            detail: None,
        }];
        let blocks = repo_sync_failure("org/repo", &steps, "gh repo clone failed: auth error");
        assert_eq!(blocks.len(), 3); // header + section + error section
        let header_text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(header_text.contains("\u{274c}"));
        let error_text = blocks[2]["text"]["text"].as_str().unwrap_or("");
        assert!(error_text.contains("auth error"));
    }

    #[test]
    fn test_should_build_repo_sync_progress_with_failed_step_duration() {
        let steps = vec![RepoStepDisplay {
            label: "Fetching".to_string(),
            status: PhaseDisplayStatus::Failed,
            duration: Some(Duration::from_secs(15)),
            started_at: None,
            detail: None,
        }];
        let blocks = repo_sync_progress("org/repo", &steps);
        let text = blocks[1]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains(":x:"));
        assert!(text.contains("failed after 15s"));
    }

    // ── Config interactive select ──────────────────────────────────────

    #[test]
    fn test_should_build_config_key_select_with_action_id() {
        let keys = vec![
            coda_core::ConfigKeyDescriptor {
                key: "agents.run.backend".to_string(),
                label: "Run backend".to_string(),
                value_type: coda_core::ConfigValueType::Enum(vec![
                    "claude".to_string(),
                    "codex".to_string(),
                ]),
                current_value: "claude".to_string(),
            },
            coda_core::ConfigKeyDescriptor {
                key: "agent.model".to_string(),
                label: "Default model".to_string(),
                value_type: coda_core::ConfigValueType::Suggest(vec![
                    "claude-opus-4-6".to_string(),
                ]),
                current_value: "claude-opus-4-6".to_string(),
            },
        ];
        let blocks = config_key_select(&keys);
        // header + actions + context = 3 blocks
        assert_eq!(blocks.len(), 3);

        let select = &blocks[1]["elements"][0];
        assert_eq!(select["action_id"], CONFIG_KEY_SELECT_ACTION);
        let options = select["options"].as_array().expect("options array");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["value"], "agents.run.backend");
        assert_eq!(options[1]["value"], "agent.model");
    }

    #[test]
    fn test_should_build_config_value_select_for_enum() {
        let descriptor = coda_core::ConfigKeyDescriptor {
            key: "agents.run.backend".to_string(),
            label: "Run backend".to_string(),
            value_type: coda_core::ConfigValueType::Enum(vec![
                "claude".to_string(),
                "codex".to_string(),
                "cursor".to_string(),
            ]),
            current_value: "claude".to_string(),
        };
        let blocks = config_value_select(&descriptor);
        // section + actions = 2 blocks
        assert_eq!(blocks.len(), 2);

        let select = &blocks[1]["elements"][0];
        assert_eq!(select["action_id"], CONFIG_VALUE_SELECT_ACTION);
        let options = select["options"].as_array().expect("options array");
        assert_eq!(options.len(), 3);
        // Encoded as "key=value"
        assert_eq!(options[0]["value"], "agents.run.backend=claude");
    }

    #[test]
    fn test_should_build_config_value_select_for_bool() {
        let descriptor = coda_core::ConfigKeyDescriptor {
            key: "git.auto_commit".to_string(),
            label: "Auto commit".to_string(),
            value_type: coda_core::ConfigValueType::Bool,
            current_value: "true".to_string(),
        };
        let blocks = config_value_select(&descriptor);
        let select = &blocks[1]["elements"][0];
        let options = select["options"].as_array().expect("options array");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["value"], "git.auto_commit=true");
        assert_eq!(options[1]["value"], "git.auto_commit=false");
    }

    #[test]
    fn test_should_build_config_value_select_for_suggest_with_hint() {
        let descriptor = coda_core::ConfigKeyDescriptor {
            key: "agent.model".to_string(),
            label: "Default model".to_string(),
            value_type: coda_core::ConfigValueType::Suggest(vec![
                "claude-opus-4-6".to_string(),
                "claude-sonnet-4-6".to_string(),
            ]),
            current_value: "claude-opus-4-6".to_string(),
        };
        let blocks = config_value_select(&descriptor);
        // section + actions + context hint = 3 blocks
        assert_eq!(blocks.len(), 3);

        // Check hint text
        let hint = blocks[2]["elements"][0]["text"].as_str().unwrap_or("");
        assert!(hint.contains("/coda config set agent.model"));
    }

    #[test]
    fn test_should_build_config_value_text_for_string_type() {
        let descriptor = coda_core::ConfigKeyDescriptor {
            key: "git.branch_prefix".to_string(),
            label: "Branch prefix".to_string(),
            value_type: coda_core::ConfigValueType::String,
            current_value: "feature".to_string(),
        };
        let blocks = config_value_select(&descriptor);
        // section + context = 2 blocks (no dropdown for free-text)
        assert_eq!(blocks.len(), 2);
        let text = blocks[0]["text"]["text"].as_str().unwrap_or("");
        assert!(text.contains("feature"));
    }

    // -----------------------------------------------------------------------
    // Init config preview & modification flow tests
    // -----------------------------------------------------------------------

    fn make_resolved_config_summary() -> coda_core::ResolvedConfigSummary {
        use coda_core::config::{AgentBackend, ReasoningEffort};

        coda_core::ResolvedConfigSummary {
            init: coda_core::ResolvedAgentConfig {
                backend: AgentBackend::Claude,
                model: "claude-opus-4-6".to_string(),
                effort: ReasoningEffort::High,
            },
            plan: coda_core::ResolvedAgentConfig {
                backend: AgentBackend::Claude,
                model: "claude-opus-4-6".to_string(),
                effort: ReasoningEffort::High,
            },
            run: coda_core::ResolvedAgentConfig {
                backend: AgentBackend::Claude,
                model: "claude-opus-4-6".to_string(),
                effort: ReasoningEffort::High,
            },
            review: coda_core::ResolvedAgentConfig {
                backend: AgentBackend::Codex,
                model: "gpt-5.3-codex".to_string(),
                effort: ReasoningEffort::High,
            },
            verify: coda_core::ResolvedAgentConfig {
                backend: AgentBackend::Codex,
                model: "gpt-5.3-codex".to_string(),
                effort: ReasoningEffort::High,
            },
        }
    }

    fn make_operation_summaries() -> Vec<coda_core::OperationSummary> {
        let backend_opts = vec![
            "claude".to_string(),
            "codex".to_string(),
            "cursor".to_string(),
        ];
        let effort_opts = vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "max".to_string(),
        ];

        vec![
            coda_core::OperationSummary {
                name: "init".to_string(),
                label: "Init".to_string(),
                backend: "claude".to_string(),
                model: "claude-opus-4-6".to_string(),
                effort: "high".to_string(),
                backend_options: backend_opts.clone(),
                model_suggestions: vec!["claude-opus-4-6".to_string()],
                effort_options: effort_opts.clone(),
            },
            coda_core::OperationSummary {
                name: "run".to_string(),
                label: "Run".to_string(),
                backend: "codex".to_string(),
                model: "gpt-5.3-codex".to_string(),
                effort: "high".to_string(),
                backend_options: backend_opts,
                model_suggestions: vec!["gpt-5.3-codex".to_string()],
                effort_options: effort_opts,
            },
        ]
    }

    #[test]
    fn test_should_build_init_config_preview_with_start_and_modify_buttons() {
        let summary = make_resolved_config_summary();
        let blocks = init_config_preview(&summary, false);

        // header + section (table) + actions
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "header");
        assert_eq!(blocks[1]["type"], "section");
        assert_eq!(blocks[2]["type"], "actions");

        // Verify both buttons exist with correct action IDs
        let elements = blocks[2]["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0]["action_id"], INIT_START_ACTION);
        assert_eq!(elements[1]["action_id"], INIT_MODIFY_ACTION);

        // Verify force flag is encoded in button values
        assert_eq!(elements[0]["value"], "false");
        assert_eq!(elements[1]["value"], "false");
    }

    #[test]
    fn test_should_encode_force_true_in_init_config_preview() {
        let summary = make_resolved_config_summary();
        let blocks = init_config_preview(&summary, true);

        let elements = blocks[2]["elements"].as_array().unwrap();
        assert_eq!(elements[0]["value"], "true");
        assert_eq!(elements[1]["value"], "true");
    }

    #[test]
    fn test_should_include_all_operations_in_config_preview_table() {
        let summary = make_resolved_config_summary();
        let blocks = init_config_preview(&summary, false);

        let table_text = blocks[1]["text"]["text"].as_str().unwrap();
        for op in &["init", "plan", "run", "review", "verify"] {
            assert!(table_text.contains(op), "Table should contain '{op}'");
        }
    }

    #[test]
    fn test_should_build_init_config_op_select_with_dropdown() {
        let summaries = make_operation_summaries();
        let blocks = init_config_op_select(&summaries, false);

        // header + actions + context
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "header");
        assert_eq!(blocks[1]["type"], "actions");

        let select = &blocks[1]["elements"][0];
        assert_eq!(select["action_id"], INIT_OP_SELECT_ACTION);

        let options = select["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);

        // Verify pipe-delimited encoding: "{op}|{force}"
        assert_eq!(options[0]["value"], "init|false");
        assert_eq!(options[1]["value"], "run|false");
    }

    #[test]
    fn test_should_build_init_config_backend_select() {
        let options = vec![
            "claude".to_string(),
            "codex".to_string(),
            "cursor".to_string(),
        ];
        let blocks = init_config_backend_select("run", &options, "codex", true);

        assert_eq!(blocks.len(), 3);
        let select = &blocks[1]["elements"][0];
        assert_eq!(select["action_id"], INIT_BACKEND_SELECT_ACTION);

        let select_opts = select["options"].as_array().unwrap();
        assert_eq!(select_opts.len(), 3);

        // Verify pipe-delimited encoding: "{op}|{backend}|{force}"
        assert_eq!(select_opts[0]["value"], "run|claude|true");
        assert_eq!(select_opts[1]["value"], "run|codex|true");
        assert_eq!(select_opts[2]["value"], "run|cursor|true");

        // Verify initial_option is set to current value
        let initial = &select["initial_option"];
        assert_eq!(initial["value"], "run|codex|true");
    }

    #[test]
    fn test_should_build_init_config_model_select() {
        let suggestions = vec!["gpt-5.3-codex".to_string(), "gpt-4.1".to_string()];
        let blocks = init_config_model_select("run", "codex", &suggestions, "gpt-5.3-codex", false);

        assert_eq!(blocks.len(), 3);
        let select = &blocks[1]["elements"][0];
        assert_eq!(select["action_id"], INIT_MODEL_SELECT_ACTION);

        let select_opts = select["options"].as_array().unwrap();
        assert_eq!(select_opts.len(), 2);

        // Verify pipe-delimited encoding: "{op}|{backend}|{model}|{force}"
        assert_eq!(select_opts[0]["value"], "run|codex|gpt-5.3-codex|false");
        assert_eq!(select_opts[1]["value"], "run|codex|gpt-4.1|false");

        // Verify custom model hint in context
        let ctx = blocks[2]["elements"][0]["text"].as_str().unwrap();
        assert!(ctx.contains("/coda config set"));
    }

    #[test]
    fn test_should_build_init_config_effort_select() {
        let options = vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "max".to_string(),
        ];
        let blocks =
            init_config_effort_select("run", "codex", "gpt-5.3-codex", &options, "high", true);

        assert_eq!(blocks.len(), 3);
        let select = &blocks[1]["elements"][0];
        assert_eq!(select["action_id"], INIT_EFFORT_SELECT_ACTION);

        let select_opts = select["options"].as_array().unwrap();
        assert_eq!(select_opts.len(), 4);

        // Verify pipe-delimited encoding: "{op}|{backend}|{model}|{effort}|{force}"
        assert_eq!(select_opts[0]["value"], "run|codex|gpt-5.3-codex|low|true");
        assert_eq!(select_opts[2]["value"], "run|codex|gpt-5.3-codex|high|true");

        // Verify initial_option is set to current value
        let initial = &select["initial_option"];
        assert_eq!(initial["value"], "run|codex|gpt-5.3-codex|high|true");

        // Verify context shows backend and model info
        let ctx = blocks[2]["elements"][0]["text"].as_str().unwrap();
        assert!(ctx.contains("codex"));
        assert!(ctx.contains("gpt-5.3-codex"));
    }

    #[test]
    fn test_should_handle_no_initial_option_when_current_not_in_list() {
        let options = vec!["claude".to_string(), "codex".to_string()];
        let blocks = init_config_backend_select("run", &options, "unknown-backend", false);

        let select = &blocks[1]["elements"][0];
        // initial_option should not be present when current isn't in the list
        assert!(select.get("initial_option").is_none());
    }
}
