//! Terminal UI for the `coda run` command.
//!
//! Provides a ratatui-based TUI that displays real-time progress of
//! feature development phases. The layout uses a split panel: a compact
//! phase sidebar on the left and a scrollable streaming content area on
//! the right, plus a pipeline bar, summary bar, and help bar.
//!
//! When the terminal is too narrow (< [`MIN_SIDEBAR_TERMINAL_WIDTH`]),
//! the sidebar is hidden and the content area fills the entire middle
//! section.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use coda_core::RunEvent;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::fmt_utils::{format_duration, truncate_str};

/// Spinner animation frames for the active phase indicator.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Border color used throughout the run UI.
const BORDER_COLOR: Color = Color::Cyan;

/// Width of the left sidebar showing phase status.
const SIDEBAR_WIDTH: u16 = 22;

/// Minimum terminal width to show the sidebar alongside the content area.
///
/// Below this threshold, the sidebar is hidden and the content area fills
/// the full width to prevent layout breakage on very small terminals.
const MIN_SIDEBAR_TERMINAL_WIDTH: u16 = SIDEBAR_WIDTH + 20;

/// Display status of a single phase in the pipeline.
#[derive(Debug, Clone)]
enum PhaseDisplayStatus {
    /// Not yet started.
    Pending,
    /// Currently running.
    Running,
    /// Completed successfully.
    Completed {
        /// Wall-clock duration of the phase.
        duration: Duration,
    },
    /// Phase failed.
    Failed,
}

/// Display state of a single phase.
#[derive(Debug, Clone)]
struct PhaseDisplay {
    name: String,
    status: PhaseDisplayStatus,
    started_at: Option<Instant>,
    detail: String,
}

/// Summary data displayed after the run finishes.
#[derive(Debug, Default)]
pub struct RunSummary {
    /// Total elapsed wall-clock time.
    pub elapsed: Duration,
    /// Total agent conversation turns.
    pub total_turns: u32,
    /// Total cost in USD.
    pub total_cost: f64,
    /// PR URL if created.
    pub pr_url: Option<String>,
}

/// Interactive TUI for displaying `coda run` progress.
///
/// Uses a split-panel layout with a compact phase sidebar on the left
/// and a scrollable streaming content area on the right.
pub struct RunUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    feature_slug: String,
    phases: Vec<PhaseDisplay>,
    active_phase: Option<usize>,
    start_time: Instant,
    total_turns: u32,
    total_cost: f64,
    pr_status: PrDisplayStatus,
    finished: bool,
    success: bool,
    spinner_tick: usize,
    /// Accumulated styled lines for the streaming content area.
    content_lines: Vec<Line<'static>>,
    /// Vertical scroll offset (number of visual rows scrolled from top).
    scroll_offset: u16,
    /// Whether auto-scroll is active (follows new content).
    auto_scroll: bool,
    /// Height of the content area in rows (updated each draw for scroll math).
    content_visible_height: u16,
    /// Width of the content area in columns (updated each draw for wrap calculations).
    content_visible_width: u16,
}

/// Display status of the PR creation step.
#[derive(Debug, Clone)]
enum PrDisplayStatus {
    /// PR creation not started yet.
    Pending,
    /// PR is being created.
    Creating,
    /// PR created successfully.
    Created { url: String },
    /// PR creation failed.
    Failed,
}

impl RunUi {
    /// Creates a new run UI, entering the alternate screen and raw mode.
    ///
    /// `initial_phases` pre-populates the phase pipeline so the first
    /// frame already shows all phase names (avoids an empty-pipeline
    /// flash while the engine connects to Claude).
    ///
    /// # Errors
    ///
    /// Returns an error if terminal setup fails.
    pub fn new(feature_slug: &str, initial_phases: Vec<String>) -> Result<Self> {
        enable_raw_mode()?;

        let phases = initial_phases
            .into_iter()
            .map(|name| PhaseDisplay {
                name,
                status: PhaseDisplayStatus::Pending,
                started_at: None,
                detail: String::new(),
            })
            .collect();

        let init = || -> Result<Self> {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen)?;
            let backend = CrosstermBackend::new(stdout);
            let terminal = Terminal::new(backend)?;
            Ok(Self {
                terminal,
                feature_slug: feature_slug.to_string(),
                phases,
                active_phase: None,
                start_time: Instant::now(),
                total_turns: 0,
                total_cost: 0.0,
                pr_status: PrDisplayStatus::Pending,
                finished: false,
                success: false,
                spinner_tick: 0,
                content_lines: Vec::new(),
                scroll_offset: 0,
                auto_scroll: true,
                content_visible_height: 0,
                content_visible_width: 0,
            })
        };

        init().inspect_err(|_| {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
        })
    }

    /// Runs the TUI event loop, consuming `RunEvent`s from the receiver.
    ///
    /// Returns a [`RunSummary`] when the run finishes or the channel closes.
    ///
    /// Uses non-blocking keyboard polling (`event::poll(ZERO)`) and async
    /// `tokio::time::sleep` for the tick interval so that the caller's
    /// `tokio::select!` can interleave engine polling during each sleep.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal I/O fails or the user cancels with Ctrl+C.
    pub async fn run(&mut self, mut rx: UnboundedReceiver<RunEvent>) -> Result<RunSummary> {
        loop {
            // Non-blocking: drain all available events, coalescing consecutive
            // text deltas into a single buffer append to reduce per-tick overhead.
            let mut pending_text: Option<String> = None;
            loop {
                match rx.try_recv() {
                    Ok(RunEvent::AgentTextDelta { text }) => {
                        pending_text.get_or_insert_with(String::new).push_str(&text);
                    }
                    Ok(event) => {
                        // Flush any accumulated text before processing non-text event
                        if let Some(coalesced) = pending_text.take() {
                            self.handle_event(RunEvent::AgentTextDelta { text: coalesced });
                        }
                        self.handle_event(event);
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        self.finished = true;
                        break;
                    }
                }
            }
            // Flush any remaining coalesced text after the drain loop
            if let Some(coalesced) = pending_text.take() {
                self.handle_event(RunEvent::AgentTextDelta { text: coalesced });
            }

            self.spinner_tick += 1;
            self.draw()?;

            if self.finished {
                // Show final state for a moment so the user can see the result
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.draw()?;
                break;
            }

            // Non-blocking keyboard check: poll with ZERO timeout so we never
            // block the tokio runtime thread (blocking would starve the engine
            // future in the caller's `tokio::select!`).
            while event::poll(Duration::ZERO)? {
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                {
                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Err(anyhow::anyhow!("Cancelled by user"));
                        }
                        KeyCode::Up => {
                            self.scroll_offset = self.scroll_offset.saturating_sub(1);
                            self.auto_scroll = false;
                        }
                        KeyCode::Down => {
                            let max = self.max_scroll_offset();
                            self.scroll_offset = (self.scroll_offset + 1).min(max);
                            if self.scroll_offset >= max {
                                self.auto_scroll = true;
                            }
                        }
                        KeyCode::PageUp => {
                            self.scroll_offset = self
                                .scroll_offset
                                .saturating_sub(self.content_visible_height);
                            self.auto_scroll = false;
                        }
                        KeyCode::PageDown => {
                            let max = self.max_scroll_offset();
                            self.scroll_offset =
                                (self.scroll_offset + self.content_visible_height).min(max);
                            if self.scroll_offset >= max {
                                self.auto_scroll = true;
                            }
                        }
                        KeyCode::End => {
                            self.scroll_offset = self.max_scroll_offset();
                            self.auto_scroll = true;
                        }
                        _ => {}
                    }
                }
            }

            // Async sleep yields control back to tokio, allowing the engine
            // future to be polled and make progress during the interval.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(self.build_summary())
    }

    /// Processes a single [`RunEvent`] and updates internal state.
    fn handle_event(&mut self, event: RunEvent) {
        match event {
            RunEvent::RunStarting { phases } => {
                self.phases = phases
                    .into_iter()
                    .map(|name| PhaseDisplay {
                        name,
                        status: PhaseDisplayStatus::Pending,
                        started_at: None,
                        detail: String::new(),
                    })
                    .collect();
            }
            RunEvent::PhaseStarting { index, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Running;
                    phase.started_at = Some(Instant::now());
                }
                self.active_phase = Some(index);
                // Clear content buffer for the new phase
                self.content_lines.clear();
                self.scroll_offset = 0;
                self.auto_scroll = true;
            }
            RunEvent::PhaseCompleted {
                index,
                duration,
                turns,
                cost_usd,
                ..
            } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Completed { duration };
                }
                self.total_turns += turns;
                self.total_cost += cost_usd;
                self.active_phase = None;
            }
            RunEvent::PhaseFailed { index, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Failed;
                }
                self.active_phase = None;
            }
            RunEvent::ReviewRound {
                round,
                max_rounds,
                issues_found,
            } => {
                if let Some(idx) = self.active_phase
                    && let Some(phase) = self.phases.get_mut(idx)
                {
                    if issues_found == 0 {
                        phase.detail = format!("round {round}/{max_rounds}: passed");
                    } else {
                        phase.detail = format!(
                            "round {round}/{max_rounds}: {issues_found} issue(s), fixing..."
                        );
                    }
                }
            }
            RunEvent::VerifyAttempt {
                attempt,
                max_attempts,
                passed,
            } => {
                if let Some(idx) = self.active_phase
                    && let Some(phase) = self.phases.get_mut(idx)
                {
                    if passed {
                        phase.detail =
                            format!("attempt {attempt}/{max_attempts}: all checks passed");
                    } else {
                        phase.detail =
                            format!("attempt {attempt}/{max_attempts}: failures found, fixing...");
                    }
                }
            }
            RunEvent::CreatingPr => {
                self.pr_status = PrDisplayStatus::Creating;
            }
            RunEvent::PrCreated { url } => {
                self.pr_status = if let Some(url) = url {
                    PrDisplayStatus::Created { url }
                } else {
                    PrDisplayStatus::Failed
                };
            }
            RunEvent::RunFinished { success } => {
                self.finished = true;
                self.success = success;
            }
            RunEvent::AgentTextDelta { text } => {
                let style = Style::default().fg(Color::White);
                let mut parts = text.split('\n');

                // First fragment appends to the current (last) text line
                if let Some(first) = parts.next() {
                    if let Some(last_line) = self.content_lines.last_mut()
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
                        self.content_lines
                            .push(Line::from(Span::styled(first.to_string(), style)));
                    }
                }

                // Each subsequent fragment (after a \n) starts a new line
                for part in parts {
                    self.content_lines
                        .push(Line::from(Span::styled(part.to_string(), style)));
                }

                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::ToolActivity { tool_name, summary } => {
                self.content_lines.push(Line::from(Span::styled(
                    format!("┄ {tool_name} {summary}"),
                    Style::default().fg(Color::DarkGray),
                )));

                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            // Future variants added to the non_exhaustive enum
            _ => {}
        }
    }

    /// Builds the final summary from accumulated state.
    fn build_summary(&self) -> RunSummary {
        RunSummary {
            elapsed: self.start_time.elapsed(),
            total_turns: self.total_turns,
            total_cost: self.total_cost,
            pr_url: match &self.pr_status {
                PrDisplayStatus::Created { url } => Some(url.clone()),
                _ => None,
            },
        }
    }

    /// Returns the name of the currently active phase, if any.
    fn active_phase_name(&self) -> Option<&str> {
        self.active_phase
            .and_then(|idx| self.phases.get(idx))
            .map(|p| p.name.as_str())
    }

    /// Computes the maximum scroll offset based on visual row count and visible height.
    ///
    /// Accounts for line wrapping by computing the total number of visual rows
    /// each `Line` occupies at the current content area width. This matches
    /// `Paragraph::scroll()` behavior, which operates on visual rows after
    /// wrapping, not logical line count.
    fn max_scroll_offset(&self) -> u16 {
        let width = (self.content_visible_width as usize).max(1);
        let total_rows: usize = self
            .content_lines
            .iter()
            .map(|line| wrapped_line_count(line, width))
            .sum();
        total_rows.saturating_sub(self.content_visible_height as usize) as u16
    }

    /// Draws the current UI state to the terminal.
    ///
    /// Uses a split-panel layout: the middle area is divided horizontally
    /// into a fixed-width sidebar (phase list) and a flexible content area
    /// (scrollable streaming output). When the terminal is too narrow,
    /// the sidebar is hidden and the content area fills the full width.
    fn draw(&mut self) -> Result<()> {
        let feature_slug = self.feature_slug.clone();
        let phases = self.phases.clone();
        let start_time = self.start_time;
        let total_turns = self.total_turns;
        let total_cost = self.total_cost;
        let pr_status = self.pr_status.clone();
        let finished = self.finished;
        let success = self.success;
        let spinner_tick = self.spinner_tick;
        // Move content_lines out temporarily via O(1) pointer swap instead
        // of cloning the entire buffer every frame. Restored after draw.
        let content_lines = std::mem::take(&mut self.content_lines);
        let scroll_offset = self.scroll_offset;
        let active_phase_name = self.active_phase_name().unwrap_or("").to_string();
        let is_running = self.active_phase.is_some();

        // Capture visible dimensions from the draw closure
        let mut visible_height: u16 = 0;
        let mut visible_width: u16 = 0;

        self.terminal.draw(|frame| {
            let area = frame.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // Header
                    Constraint::Length(3), // Pipeline bar
                    Constraint::Min(8),    // Middle: sidebar + content (flexible)
                    Constraint::Length(3), // Summary bar
                    Constraint::Length(1), // Help bar
                ])
                .split(area);

            render_header(frame, chunks[0], &feature_slug, finished, success);
            render_pipeline(frame, chunks[1], &phases, &pr_status, spinner_tick);

            let middle = chunks[2];

            if middle.width < MIN_SIDEBAR_TERMINAL_WIDTH {
                // Small terminal: content only, skip sidebar to avoid breakage
                (visible_height, visible_width) = render_content(
                    frame,
                    middle,
                    &content_lines,
                    scroll_offset,
                    &active_phase_name,
                );
            } else {
                // Normal layout: sidebar | content
                let h_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)])
                    .split(middle);

                render_sidebar(frame, h_chunks[0], &phases, &pr_status, spinner_tick);
                (visible_height, visible_width) = render_content(
                    frame,
                    h_chunks[1],
                    &content_lines,
                    scroll_offset,
                    &active_phase_name,
                );
            }

            render_summary(
                frame,
                chunks[3],
                start_time,
                total_turns,
                total_cost,
                &pr_status,
            );
            render_help_bar(frame, chunks[4], finished, is_running);
        })?;

        self.content_visible_height = visible_height;
        self.content_visible_width = visible_width;
        // Restore content_lines after draw (O(1) pointer swap)
        self.content_lines = content_lines;

        Ok(())
    }
}

impl std::fmt::Debug for RunUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunUi")
            .field("feature_slug", &self.feature_slug)
            .field("phases", &self.phases.len())
            .field("finished", &self.finished)
            .field("content_lines", &self.content_lines.len())
            .field("scroll_offset", &self.scroll_offset)
            .field("auto_scroll", &self.auto_scroll)
            .field(
                "content_visible",
                &format_args!(
                    "{}x{}",
                    self.content_visible_width, self.content_visible_height
                ),
            )
            .finish_non_exhaustive()
    }
}

impl Drop for RunUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

// ── Rendering Functions ──────────────────────────────────────────────

/// Renders the header line showing feature name and overall status.
fn render_header(frame: &mut Frame, area: Rect, feature_slug: &str, finished: bool, success: bool) {
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
            format!(" CODA Run: {feature_slug} "),
            Style::default().fg(Color::White).bold(),
        ),
        status,
    ]);
    let paragraph = Paragraph::new(header).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

/// Renders the horizontal phase pipeline with status indicators.
fn render_pipeline(
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

    let mut spans: Vec<Span> = Vec::new();

    for (i, phase) in phases.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" → ", Style::default().fg(Color::DarkGray)));
        }

        let (icon, style) = match &phase.status {
            PhaseDisplayStatus::Pending => ("○", Style::default().fg(Color::DarkGray)),
            PhaseDisplayStatus::Running => {
                let frame_char = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
                spans.push(Span::styled(
                    format!("{frame_char} "),
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::styled(
                    phase.name.clone(),
                    Style::default().fg(Color::Yellow).bold(),
                ));
                continue;
            }
            PhaseDisplayStatus::Completed { .. } => ("●", Style::default().fg(Color::Green)),
            PhaseDisplayStatus::Failed => ("✗", Style::default().fg(Color::Red)),
        };

        spans.push(Span::styled(format!("{icon} "), style));
        spans.push(Span::styled(phase.name.clone(), style));
    }

    // PR step
    spans.push(Span::styled(" → ", Style::default().fg(Color::DarkGray)));
    match pr_status {
        PrDisplayStatus::Pending => {
            spans.push(Span::styled("○ PR", Style::default().fg(Color::DarkGray)));
        }
        PrDisplayStatus::Creating => {
            let frame_char = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            spans.push(Span::styled(
                format!("{frame_char} PR"),
                Style::default().fg(Color::Yellow).bold(),
            ));
        }
        PrDisplayStatus::Created { .. } => {
            spans.push(Span::styled("● PR", Style::default().fg(Color::Green)));
        }
        PrDisplayStatus::Failed => {
            spans.push(Span::styled("✗ PR", Style::default().fg(Color::Red)));
        }
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, inner);
}

/// Renders the compact phase sidebar in the left panel.
///
/// Each phase occupies one or two lines: a status line with icon, truncated
/// name, and elapsed time; plus an optional detail sub-line for running
/// phases that have review/verify round information. The PR step is
/// appended as the final entry.
fn render_sidebar(
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

    // PR entry
    lines.push(render_sidebar_pr(pr_status, spinner_tick));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Renders a single phase entry for the sidebar.
///
/// Returns one or two lines: the main status line (`[icon] name elapsed`)
/// plus an optional detail sub-line for running phases with review/verify
/// round information (e.g., "round 2/3: 5 issue(s), fixing...").
///
/// The name is truncated to fit within the available width after accounting
/// for the icon prefix and the elapsed time suffix.
fn render_sidebar_phase(
    phase: &PhaseDisplay,
    spinner_tick: usize,
    width: u16,
) -> Vec<Line<'static>> {
    // Icon prefix "[X] " takes 4 characters
    let name_budget = (width as usize).saturating_sub(4);

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
            let elapsed = phase
                .started_at
                .map(|t| format_duration(t.elapsed()))
                .unwrap_or_default();
            // Reserve space for " elapsed" suffix
            let time_len = elapsed.len() + 1;
            let avail = name_budget.saturating_sub(time_len);
            let name = truncate_str(&phase.name, avail);
            let mut result = vec![Line::from(vec![
                Span::styled(
                    format!("[{spinner}] {name}"),
                    Style::default().fg(Color::Yellow).bold(),
                ),
                Span::styled(format!(" {elapsed}"), Style::default().fg(Color::Yellow)),
            ])];
            // Show review/verify detail text as a sub-line when available
            if !phase.detail.is_empty() {
                let detail = truncate_str(&phase.detail, name_budget);
                result.push(Line::from(Span::styled(
                    format!("    {detail}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
            result
        }
        PhaseDisplayStatus::Completed { duration, .. } => {
            let dur = format_duration(*duration);
            let time_len = dur.len() + 1;
            let avail = name_budget.saturating_sub(time_len);
            let name = truncate_str(&phase.name, avail);
            vec![Line::from(vec![
                Span::styled(format!("[●] {name}"), Style::default().fg(Color::Green)),
                Span::styled(format!(" {dur}"), Style::default().fg(Color::White)),
            ])]
        }
        PhaseDisplayStatus::Failed => {
            let name = truncate_str(&phase.name, name_budget);
            vec![Line::from(Span::styled(
                format!("[✗] {name}"),
                Style::default().fg(Color::Red),
            ))]
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
    }
}

/// Renders the scrollable streaming content area in the right panel.
///
/// Displays `content_lines` as a wrapped `Paragraph` with vertical
/// scrolling. The current phase name is shown as the block title.
///
/// Returns `(visible_height, visible_width)` of the content area (inner
/// dimensions after borders) so the caller can use them for scroll offset
/// and line-wrap calculations.
fn render_content(
    frame: &mut Frame,
    area: Rect,
    content_lines: &[Line<'_>],
    scroll_offset: u16,
    active_phase_name: &str,
) -> (u16, u16) {
    let title = if active_phase_name.is_empty() {
        " Output ".to_string()
    } else {
        format!(" {active_phase_name} ")
    };

    let block = Block::default()
        .title(title)
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

    (inner.height, inner.width)
}

/// Renders the summary bar with live elapsed time, turns, and cost.
fn render_summary(
    frame: &mut Frame,
    area: Rect,
    start_time: Instant,
    total_turns: u32,
    total_cost: f64,
    pr_status: &PrDisplayStatus,
) {
    let block = Block::default()
        .title(" Summary ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let elapsed = format_duration(start_time.elapsed());

    let mut spans = vec![
        Span::styled(
            format!("  Elapsed: {elapsed}"),
            Style::default().fg(Color::White).bold(),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("Turns: {total_turns}"),
            Style::default().fg(Color::White),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("Cost: ${total_cost:.4} USD"),
            Style::default().fg(Color::White),
        ),
    ];

    if let PrDisplayStatus::Created { url } = pr_status {
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
fn render_help_bar(frame: &mut Frame, area: Rect, finished: bool, is_running: bool) {
    let help = if finished {
        Line::from(vec![Span::styled(
            " Run finished. Exiting...",
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

// ── Utility Functions ────────────────────────────────────────────────

/// Calculates the number of visual rows a styled [`Line`] occupies when
/// wrapped to the given panel width.
///
/// Sums the character count across all spans in the line, then divides
/// by the panel width (rounding up). Returns at least 1 row per line.
///
/// Uses character count as an approximation — accurate for monospace
/// terminals with ASCII text (which covers the vast majority of AI agent
/// output).
fn wrapped_line_count(line: &Line<'_>, panel_width: usize) -> usize {
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
        // 100 chars at width 80 = 2 visual rows
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
        // Two spans totaling 100 chars at width 80 = 2 visual rows
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
}
