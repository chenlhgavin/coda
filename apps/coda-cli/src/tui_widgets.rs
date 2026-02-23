//! Shared TUI rendering widgets for the CODA CLI.
//!
//! Provides reusable ratatui rendering functions used by both the `init`
//! and `run` TUI screens. Each widget is parameterized to accommodate
//! the differing data requirements of each command while maintaining a
//! consistent visual style and layout structure.

use std::time::Duration;

use ratatui::{
    prelude::*,
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
};

use crate::fmt_utils::{format_duration, truncate_str};

// ── Constants ────────────────────────────────────────────────────────

/// Spinner animation frames (Braille dots) for active phase indicators.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Border color used throughout all TUI screens.
pub const BORDER_COLOR: Color = Color::Cyan;

/// Width of the left sidebar showing phase status.
pub const SIDEBAR_WIDTH: u16 = 26;

/// Minimum terminal width to show the sidebar alongside the content area.
///
/// Below this threshold, the sidebar is hidden and the content area fills
/// the full width to prevent layout breakage on very small terminals.
pub const MIN_SIDEBAR_TERMINAL_WIDTH: u16 = SIDEBAR_WIDTH + 20;

// ── Phase Display Types ──────────────────────────────────────────────

/// Display status of a single phase in the pipeline.
#[derive(Debug, Clone)]
pub enum PhaseDisplayStatus {
    /// Not yet started.
    Pending,
    /// Currently running.
    Running,
    /// Completed successfully.
    Completed {
        /// Wall-clock duration of the phase.
        duration: Duration,
        /// Cost in USD (only tracked by init, `None` for run).
        cost_usd: Option<f64>,
    },
    /// Phase failed.
    Failed {
        /// Error message (only tracked by init, empty for run).
        error: String,
    },
}

/// Display state of a single phase.
#[derive(Debug, Clone)]
pub struct PhaseDisplay {
    /// Phase name.
    pub name: String,
    /// Current display status.
    pub status: PhaseDisplayStatus,
    /// When this phase started (for live elapsed timer).
    pub started_at: Option<std::time::Instant>,
    /// Number of agent turns completed so far (live counter, run only).
    pub current_turn: u32,
    /// Detail text for review/verify rounds (run only).
    pub detail: String,
    /// Compact round/attempt label for Line 2 metadata (e.g., "R4/5", "A2/3").
    pub round_label: String,
}

/// Display status of the PR creation step (run only).
#[derive(Debug, Clone)]
pub enum PrDisplayStatus {
    /// PR creation not started yet.
    Pending,
    /// PR is being created.
    Creating,
    /// PR created successfully.
    Created {
        /// PR URL.
        url: String,
    },
    /// PR creation failed.
    Failed,
    /// PR step is not applicable (e.g., init command).
    NotApplicable,
}

// ── Rendering Functions ──────────────────────────────────────────────

/// Renders the header line showing command title and overall status.
///
/// The header occupies a single row with a dark-gray background.
/// `title_prefix` is typically "Init" or "Run", and `label` is the
/// project root or feature slug.
pub fn render_header(
    frame: &mut Frame,
    area: Rect,
    title_prefix: &str,
    label: &str,
    finished: bool,
    success: bool,
) {
    let status = if finished {
        if success {
            Span::styled(" [Completed] ", Style::default().fg(Color::Green).bold())
        } else {
            Span::styled(" [Failed] ", Style::default().fg(Color::Red).bold())
        }
    } else {
        Span::styled(" [Running] ", Style::default().fg(Color::Yellow).bold())
    };

    let header = Line::from(vec![
        Span::styled(
            format!(" CODA {title_prefix}: {label} "),
            Style::default().fg(Color::White).bold(),
        ),
        status,
    ]);
    let paragraph = Paragraph::new(header).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

/// Renders the horizontal phase pipeline with status indicators.
///
/// Uses adaptive three-tier degradation to fit within available width:
/// - **Tier 1**: Full phase names when space allows.
/// - **Tier 2**: Equally truncated names (min 3 chars) when full names overflow.
/// - **Tier 3**: Icon-only mode (`● → ⠋ → ○`) for extremely narrow terminals.
///
/// When `pr_status` is not `NotApplicable`, a PR step is appended at the end.
pub fn render_pipeline(
    frame: &mut Frame,
    area: Rect,
    phases: &[PhaseDisplay],
    pr_status: &PrDisplayStatus,
    spinner_tick: usize,
) {
    let block = Block::default()
        .title(" Pipeline ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let available = inner.width as usize;
    let has_pr = !matches!(pr_status, PrDisplayStatus::NotApplicable);

    // Collect pipeline entries: (name, icon, style, bold_name)
    let mut entries: Vec<PipelineEntry> = phases
        .iter()
        .map(|p| pipeline_entry_for_phase(p, spinner_tick))
        .collect();
    if has_pr {
        entries.push(pipeline_entry_for_pr(pr_status, spinner_tick));
    }

    let tier = choose_pipeline_tier(&entries, available);
    let spans = build_pipeline_spans(&entries, tier);

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, inner);
}

/// Minimum name length for Tier 2 truncation before falling back to Tier 3.
const PIPELINE_MIN_NAME_LEN: usize = 3;

/// A single entry in the pipeline bar.
struct PipelineEntry {
    name: String,
    icon: String,
    style: Style,
    bold_name: bool,
}

/// Builds a [`PipelineEntry`] from a phase display.
fn pipeline_entry_for_phase(phase: &PhaseDisplay, spinner_tick: usize) -> PipelineEntry {
    match &phase.status {
        PhaseDisplayStatus::Pending => PipelineEntry {
            name: phase.name.clone(),
            icon: "○".to_owned(),
            style: Style::default().fg(Color::DarkGray),
            bold_name: false,
        },
        PhaseDisplayStatus::Running => {
            let frame_char = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            PipelineEntry {
                name: phase.name.clone(),
                icon: frame_char.to_owned(),
                style: Style::default().fg(Color::Yellow),
                bold_name: true,
            }
        }
        PhaseDisplayStatus::Completed { .. } => PipelineEntry {
            name: phase.name.clone(),
            icon: "●".to_owned(),
            style: Style::default().fg(Color::Green),
            bold_name: false,
        },
        PhaseDisplayStatus::Failed { .. } => PipelineEntry {
            name: phase.name.clone(),
            icon: "✗".to_owned(),
            style: Style::default().fg(Color::Red),
            bold_name: false,
        },
    }
}

/// Builds a [`PipelineEntry`] from a PR display status.
fn pipeline_entry_for_pr(pr_status: &PrDisplayStatus, spinner_tick: usize) -> PipelineEntry {
    match pr_status {
        PrDisplayStatus::Pending => PipelineEntry {
            name: "PR".to_owned(),
            icon: "○".to_owned(),
            style: Style::default().fg(Color::DarkGray),
            bold_name: false,
        },
        PrDisplayStatus::Creating => {
            let frame_char = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            PipelineEntry {
                name: "PR".to_owned(),
                icon: frame_char.to_owned(),
                style: Style::default().fg(Color::Yellow),
                bold_name: true,
            }
        }
        PrDisplayStatus::Created { .. } => PipelineEntry {
            name: "PR".to_owned(),
            icon: "●".to_owned(),
            style: Style::default().fg(Color::Green),
            bold_name: false,
        },
        PrDisplayStatus::Failed => PipelineEntry {
            name: "PR".to_owned(),
            icon: "✗".to_owned(),
            style: Style::default().fg(Color::Red),
            bold_name: false,
        },
        PrDisplayStatus::NotApplicable => PipelineEntry {
            name: String::new(),
            icon: String::new(),
            style: Style::default(),
            bold_name: false,
        },
    }
}

/// The rendering tier for the pipeline bar.
enum PipelineTier {
    /// Full phase names.
    Full,
    /// Truncated names with a maximum character budget per name.
    Truncated { max_name_len: usize },
    /// Icon-only mode (no names).
    IconOnly,
}

/// Chooses the best rendering tier for the given available width.
///
/// Each entry occupies: `icon(1) + space(1) + name_len` characters.
/// Separators between entries: ` → ` (3 characters each).
///
/// - Tier 1 (Full): all names fit at full length.
/// - Tier 2 (Truncated): names shortened equally so everything fits (min 3 chars).
/// - Tier 3 (IconOnly): only icons + separators, no names.
fn choose_pipeline_tier(entries: &[PipelineEntry], available: usize) -> PipelineTier {
    if entries.is_empty() {
        return PipelineTier::Full;
    }

    let n = entries.len();
    // Separator overhead: (n-1) × 3 for " → "
    let separator_total = (n - 1) * 3;
    // Icon overhead per entry: "icon " = 2 chars (icon + space)
    let icon_overhead = n * 2;

    // Tier 1: try full names
    let full_name_total: usize = entries.iter().map(|e| e.name.chars().count()).sum();
    let full_width = separator_total + icon_overhead + full_name_total;
    if full_width <= available {
        return PipelineTier::Full;
    }

    // Tier 2: compute max_name_len so everything fits
    // total = separator_total + n * (2 + max_name_len) <= available
    let fixed_overhead = separator_total + icon_overhead;
    if available > fixed_overhead {
        let budget_for_names = available - fixed_overhead;
        let max_name_len = budget_for_names / n;
        if max_name_len >= PIPELINE_MIN_NAME_LEN {
            return PipelineTier::Truncated { max_name_len };
        }
    }

    // Tier 3: icon-only
    PipelineTier::IconOnly
}

/// Builds the styled spans for the pipeline bar according to the chosen tier.
fn build_pipeline_spans(entries: &[PipelineEntry], tier: PipelineTier) -> Vec<Span<'static>> {
    let mut spans: Vec<Span> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" → ", Style::default().fg(Color::DarkGray)));
        }

        match &tier {
            PipelineTier::IconOnly => {
                spans.push(Span::styled(entry.icon.clone(), entry.style));
            }
            PipelineTier::Full => {
                spans.push(Span::styled(format!("{} ", entry.icon), entry.style));
                let name_style = if entry.bold_name {
                    entry.style.bold()
                } else {
                    entry.style
                };
                spans.push(Span::styled(entry.name.clone(), name_style));
            }
            PipelineTier::Truncated { max_name_len } => {
                spans.push(Span::styled(format!("{} ", entry.icon), entry.style));
                let name_style = if entry.bold_name {
                    entry.style.bold()
                } else {
                    entry.style
                };
                let name = truncate_str(&entry.name, *max_name_len);
                spans.push(Span::styled(name, name_style));
            }
        }
    }

    spans
}

/// Renders the compact phase sidebar in the left panel.
///
/// Each phase occupies one or two lines: a status line with icon, truncated
/// name, and elapsed time; plus an optional detail sub-line for running
/// phases that have review/verify round information. When `pr_status` is
/// applicable, the PR step is appended as the final entry.
pub fn render_sidebar(
    frame: &mut Frame,
    area: Rect,
    phases: &[PhaseDisplay],
    pr_status: &PrDisplayStatus,
    spinner_tick: usize,
) {
    let block = Block::default()
        .title(" Phases ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    for phase in phases {
        lines.extend(render_sidebar_phase(phase, spinner_tick, inner.width));
    }

    // PR entry (only when applicable)
    if !matches!(pr_status, PrDisplayStatus::NotApplicable) {
        lines.push(render_sidebar_pr(pr_status, spinner_tick));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Renders a single phase entry for the sidebar.
///
/// Uses a two-line layout for non-pending phases:
/// - Line 1: `[icon] phase-name`
/// - Line 2: `    T:count  duration  $cost` (indented metadata)
///
/// Pending phases use a single line (`[○] name`).
/// Running phases may show an additional detail sub-line for review/verify
/// round information (e.g., "round 2/3: 5 issue(s), fixing...").
fn render_sidebar_phase(
    phase: &PhaseDisplay,
    spinner_tick: usize,
    width: u16,
) -> Vec<Line<'static>> {
    // Icon prefix "[X] " takes 4 characters
    let name_budget = (width as usize).saturating_sub(4);
    // Sub-line indent "  " takes 2 characters
    let meta_budget = (width as usize).saturating_sub(2);

    match &phase.status {
        PhaseDisplayStatus::Pending => {
            let name = truncate_str(&phase.name, name_budget);
            vec![Line::from(Span::styled(
                format!("[○] {name}"),
                Style::default().fg(Color::DarkGray),
            ))]
        }
        PhaseDisplayStatus::Running => {
            let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            let name = truncate_str(&phase.name, name_budget);

            // Line 1: icon + name
            let mut result = vec![Line::from(Span::styled(
                format!("[{spinner}] {name}"),
                Style::default().fg(Color::Yellow).bold(),
            ))];

            // Line 2: indented metadata (turns + elapsed)
            let elapsed = phase
                .started_at
                .map(|t| format_duration(t.elapsed()))
                .unwrap_or_default();
            let meta = if phase.current_turn > 0 {
                format!("T:{:>3}  {elapsed}", phase.current_turn)
            } else {
                elapsed
            };
            let meta = truncate_str(&meta, meta_budget);
            result.push(Line::from(Span::styled(
                format!("  {meta}"),
                Style::default().fg(Color::Yellow),
            )));

            // Optional line 3: round label prefix + detail text
            if !phase.detail.is_empty() {
                let detail = if phase.round_label.is_empty() {
                    phase.detail.clone()
                } else {
                    format!("{:<5}  {}", phase.round_label, phase.detail)
                };
                let detail = truncate_str(&detail, meta_budget);
                result.push(Line::from(Span::styled(
                    format!("  {detail}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
            result
        }
        PhaseDisplayStatus::Completed { duration, cost_usd } => {
            let name = truncate_str(&phase.name, name_budget);

            // Line 1: icon + name
            let name_line = Line::from(Span::styled(
                format!("[●] {name}"),
                Style::default().fg(Color::Green),
            ));

            // Line 2: indented metadata (turns + duration + cost)
            // Pad turn+duration to 14 chars so cost aligns across phases.
            let dur = format_duration(*duration);
            let left = if phase.current_turn > 0 {
                format!("T:{:>3}  {dur}", phase.current_turn)
            } else {
                dur
            };
            let meta = if let Some(cost) = cost_usd {
                format!("{left:<14}  ${cost:.2}")
            } else {
                left
            };
            let meta = truncate_str(&meta, meta_budget);
            let meta_line = Line::from(Span::styled(
                format!("  {meta}"),
                Style::default().fg(Color::White),
            ));

            vec![name_line, meta_line]
        }
        PhaseDisplayStatus::Failed { error } => {
            let name = truncate_str(&phase.name, name_budget);
            if error.is_empty() {
                vec![Line::from(Span::styled(
                    format!("[✗] {name}"),
                    Style::default().fg(Color::Red),
                ))]
            } else {
                let err_display = truncate_str(error, meta_budget);
                vec![
                    Line::from(Span::styled(
                        format!("[✗] {name}"),
                        Style::default().fg(Color::Red),
                    )),
                    Line::from(Span::styled(
                        format!("  {err_display}"),
                        Style::default().fg(Color::Red),
                    )),
                ]
            }
        }
    }
}

/// Renders the PR entry for the sidebar.
fn render_sidebar_pr(pr_status: &PrDisplayStatus, spinner_tick: usize) -> Line<'static> {
    match pr_status {
        PrDisplayStatus::Pending => {
            Line::from(Span::styled("[○] PR", Style::default().fg(Color::DarkGray)))
        }
        PrDisplayStatus::Creating => {
            let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            Line::from(Span::styled(
                format!("[{spinner}] PR"),
                Style::default().fg(Color::Yellow).bold(),
            ))
        }
        PrDisplayStatus::Created { .. } => {
            Line::from(Span::styled("[●] PR", Style::default().fg(Color::Green)))
        }
        PrDisplayStatus::Failed => {
            Line::from(Span::styled("[✗] PR", Style::default().fg(Color::Red)))
        }
        PrDisplayStatus::NotApplicable => Line::default(),
    }
}

/// Renders the scrollable streaming content area with a scrollbar.
///
/// Displays `content_lines` as a wrapped `Paragraph` with vertical
/// scrolling. The `title` is shown as the block title (e.g., the active
/// phase name or "Output").
///
/// Returns `(visible_height, visible_width)` of the content area (inner
/// dimensions after borders) so the caller can use them for scroll offset
/// and line-wrap calculations.
pub fn render_content(
    frame: &mut Frame,
    area: Rect,
    content_lines: &[Line<'_>],
    scroll_offset: u16,
    title: &str,
) -> (u16, u16) {
    let display_title = if title.is_empty() {
        " Output ".to_string()
    } else {
        format!(" {title} ")
    };

    let block = Block::default()
        .title(display_title)
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let paragraph = Paragraph::new(content_lines.to_vec())
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0));
    frame.render_widget(paragraph, inner);

    // Render scrollbar
    let total_visual_lines: usize = content_lines
        .iter()
        .map(|l| wrapped_line_count(l, inner.width as usize))
        .sum();
    let mut scrollbar_state =
        ScrollbarState::new(total_visual_lines).position(scroll_offset as usize);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);

    (inner.height, inner.width)
}

/// Summary fields for the summary bar.
///
/// All fields are optional except `elapsed` to support both init
/// (elapsed + cost) and run (elapsed + turns + cost + PR) use cases.
pub struct SummaryFields<'a> {
    /// Elapsed wall-clock time.
    pub elapsed: Duration,
    /// Total agent turns (run only).
    pub turns: Option<u32>,
    /// Total cost in USD.
    pub cost: f64,
    /// PR display status (run only).
    pub pr_status: &'a PrDisplayStatus,
}

/// Renders the summary bar with live metrics.
pub fn render_summary(frame: &mut Frame, area: Rect, fields: &SummaryFields<'_>) {
    let block = Block::default()
        .title(" Summary ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let elapsed = format_duration(fields.elapsed);

    let mut spans = vec![
        Span::styled(
            format!("  Elapsed: {elapsed}"),
            Style::default().fg(Color::White).bold(),
        ),
        Span::styled("   ", Style::default()),
    ];

    if let Some(turns) = fields.turns {
        spans.push(Span::styled(
            format!("Turns: {turns}"),
            Style::default().fg(Color::White),
        ));
        spans.push(Span::styled("   ", Style::default()));
    }

    spans.push(Span::styled(
        format!("Cost: ${:.4} USD", fields.cost),
        Style::default().fg(Color::White),
    ));

    if let PrDisplayStatus::Created { url } = fields.pr_status {
        spans.push(Span::styled("   ", Style::default()));
        spans.push(Span::styled(
            format!("PR: {url}"),
            Style::default().fg(Color::Cyan),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, inner);
}

/// Renders the help bar with keyboard shortcuts.
///
/// Shows scroll navigation hints when a phase is actively running.
pub fn render_help_bar(frame: &mut Frame, area: Rect, finished: bool, is_running: bool) {
    let help = if finished {
        Line::from(vec![Span::styled(
            " Finished. Exiting...",
            Style::default().fg(Color::DarkGray),
        )])
    } else if is_running {
        Line::from(vec![Span::styled(
            " [Ctrl+C] Cancel  [↑↓] Scroll  [End] Auto-scroll",
            Style::default().fg(Color::DarkGray),
        )])
    } else {
        Line::from(vec![Span::styled(
            " [Ctrl+C] Cancel",
            Style::default().fg(Color::DarkGray),
        )])
    };
    let paragraph = Paragraph::new(help);
    frame.render_widget(paragraph, area);
}

/// Parameters for rendering the middle section (sidebar + content).
pub struct MiddlePanel<'a> {
    /// Phase display data for the sidebar.
    pub phases: &'a [PhaseDisplay],
    /// PR display status for the sidebar.
    pub pr_status: &'a PrDisplayStatus,
    /// Current spinner animation tick.
    pub spinner_tick: usize,
    /// Styled lines for the content area.
    pub content_lines: &'a [Line<'a>],
    /// Current scroll offset.
    pub scroll_offset: u16,
    /// Title for the content panel (e.g., active phase name).
    pub content_title: &'a str,
}

/// Renders the middle section with optional sidebar and content area.
///
/// When the terminal width is below [`MIN_SIDEBAR_TERMINAL_WIDTH`], the
/// sidebar is hidden and the content area fills the full width.
///
/// Returns `(visible_height, visible_width)` of the content area.
pub fn render_middle(frame: &mut Frame, area: Rect, panel: &MiddlePanel<'_>) -> (u16, u16) {
    if area.width < MIN_SIDEBAR_TERMINAL_WIDTH {
        // Small terminal: content only, skip sidebar to avoid breakage
        render_content(
            frame,
            area,
            panel.content_lines,
            panel.scroll_offset,
            panel.content_title,
        )
    } else {
        // Normal layout: sidebar | content
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
            .split(area);

        render_sidebar(
            frame,
            h_chunks[0],
            panel.phases,
            panel.pr_status,
            panel.spinner_tick,
        );
        render_content(
            frame,
            h_chunks[1],
            panel.content_lines,
            panel.scroll_offset,
            panel.content_title,
        )
    }
}

// ── Utility Functions ────────────────────────────────────────────────

/// Calculates the number of visual rows a styled [`Line`] occupies when
/// wrapped to the given panel width.
///
/// Sums the character count across all spans in the line, then divides
/// by the panel width (rounding up). Returns at least 1 row per line.
pub fn wrapped_line_count(line: &Line<'_>, panel_width: usize) -> usize {
    if panel_width == 0 {
        return 1;
    }
    let char_count: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if char_count == 0 {
        1
    } else {
        char_count.div_ceil(panel_width)
    }
}

/// Appends streaming text to the styled content buffer, splitting on newlines.
///
/// Text is styled with `Color::White`. Each newline creates a new `Line`.
/// The first fragment is appended to the last existing line if it has a
/// matching style (to support streaming that splits mid-line).
pub fn append_styled_text(buffer: &mut Vec<Line<'static>>, text: &str) {
    let style = Style::default().fg(Color::White);
    let mut parts = text.split('\n');

    // First fragment appends to the current (last) text line
    if let Some(first) = parts.next() {
        if let Some(last_line) = buffer.last_mut()
            && last_line
                .spans
                .last()
                .is_some_and(|s| s.style.fg == Some(Color::White))
        {
            if let Some(last_span) = last_line.spans.last_mut() {
                let mut existing = last_span.content.to_string();
                existing.push_str(first);
                *last_span = Span::styled(existing, style);
            }
        } else if !first.is_empty() {
            buffer.push(Line::from(Span::styled(first.to_string(), style)));
        }
    }

    // Each subsequent fragment (after a \n) starts a new line
    for part in parts {
        buffer.push(Line::from(Span::styled(part.to_string(), style)));
    }
}

/// Appends a tool activity line to the content buffer.
///
/// Formatted as `"┄ tool_name summary"` in dark gray to visually
/// distinguish tool activity from agent text output.
pub fn append_tool_activity(buffer: &mut Vec<Line<'static>>, tool_name: &str, summary: &str) {
    buffer.push(Line::from(Span::styled(
        format!("┄ {tool_name} {summary}"),
        Style::default().fg(Color::DarkGray),
    )));
}

/// Computes the maximum scroll offset for the content area.
///
/// Accounts for line wrapping by computing the total number of visual rows
/// each `Line` occupies at the current content area width.
pub fn max_scroll_offset(
    content_lines: &[Line<'_>],
    visible_width: u16,
    visible_height: u16,
) -> u16 {
    let width = (visible_width as usize).max(1);
    let total_rows: usize = content_lines
        .iter()
        .map(|line| wrapped_line_count(line, width))
        .sum();
    total_rows.saturating_sub(visible_height as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_count_single_row_for_short_line() {
        let line = Line::from(Span::raw("hello"));
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_should_count_multiple_rows_for_long_line() {
        let long_text = "a".repeat(100);
        let line = Line::from(Span::raw(long_text));
        assert_eq!(wrapped_line_count(&line, 80), 2);
    }

    #[test]
    fn test_should_count_one_row_for_empty_line() {
        let line = Line::from(Span::raw(""));
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_should_handle_zero_width() {
        let line = Line::from(Span::raw("hello"));
        assert_eq!(wrapped_line_count(&line, 0), 1);
    }

    #[test]
    fn test_should_count_rows_across_multiple_spans() {
        let line = Line::from(vec![Span::raw("a".repeat(60)), Span::raw("b".repeat(40))]);
        assert_eq!(wrapped_line_count(&line, 80), 2);
    }

    #[test]
    fn test_should_count_one_row_for_exact_width() {
        let line = Line::from(Span::raw("a".repeat(80)));
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_should_count_two_rows_for_one_over_width() {
        let line = Line::from(Span::raw("a".repeat(81)));
        assert_eq!(wrapped_line_count(&line, 80), 2);
    }

    #[test]
    fn test_should_append_single_line_to_empty_buffer() {
        let mut buf: Vec<Line<'static>> = Vec::new();
        append_styled_text(&mut buf, "hello world");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].spans[0].content.as_ref(), "hello world");
    }

    #[test]
    fn test_should_append_to_existing_last_line() {
        let mut buf: Vec<Line<'static>> = Vec::new();
        append_styled_text(&mut buf, "partial");
        append_styled_text(&mut buf, " text");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].spans[0].content.as_ref(), "partial text");
    }

    #[test]
    fn test_should_split_on_newlines() {
        let mut buf: Vec<Line<'static>> = Vec::new();
        append_styled_text(&mut buf, "line1\nline2\nline3");
        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0].spans[0].content.as_ref(), "line1");
        assert_eq!(buf[1].spans[0].content.as_ref(), "line2");
        assert_eq!(buf[2].spans[0].content.as_ref(), "line3");
    }

    #[test]
    fn test_should_handle_trailing_newline() {
        let mut buf: Vec<Line<'static>> = Vec::new();
        append_styled_text(&mut buf, "line1\n");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0].spans[0].content.as_ref(), "line1");
        // Second line is empty
        assert_eq!(buf[1].spans[0].content.as_ref(), "");
    }

    #[test]
    fn test_should_append_tool_activity() {
        let mut buf: Vec<Line<'static>> = Vec::new();
        append_tool_activity(&mut buf, "read_file", "src/main.rs");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].spans[0].content.as_ref(), "┄ read_file src/main.rs");
        assert_eq!(buf[0].spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_should_compute_max_scroll_offset() {
        let lines: Vec<Line> = (0..20)
            .map(|i| Line::from(Span::raw(format!("line {i}"))))
            .collect();
        // 20 lines, each 1 row, visible height 10 => max offset 10
        assert_eq!(max_scroll_offset(&lines, 80, 10), 10);
    }

    #[test]
    fn test_should_return_zero_max_scroll_when_content_fits() {
        let lines: Vec<Line> = (0..5)
            .map(|i| Line::from(Span::raw(format!("line {i}"))))
            .collect();
        // 5 lines, visible height 10 => max offset 0
        assert_eq!(max_scroll_offset(&lines, 80, 10), 0);
    }
}
