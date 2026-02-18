//! Terminal UI for the `coda run` command.
//!
//! Provides a ratatui-based TUI that displays real-time progress of
//! feature development phases, including a phase pipeline, live elapsed
//! timers, spinner animation, and a summary bar.

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
    widgets::{Block, BorderType, Borders, Paragraph},
};
use tokio::sync::mpsc::UnboundedReceiver;

/// Spinner animation frames for the active phase indicator.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Border color used throughout the run UI.
const BORDER_COLOR: Color = Color::Cyan;

/// Display status of a single phase in the pipeline.
#[derive(Debug, Clone)]
enum PhaseDisplayStatus {
    /// Not yet started.
    Pending,
    /// Currently running.
    Running,
    /// Completed with metrics.
    Completed {
        duration: Duration,
        turns: u32,
        cost_usd: f64,
    },
    /// Phase failed with an error.
    Failed { error: String },
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
#[allow(dead_code)]
pub struct RunSummary {
    /// Total elapsed wall-clock time.
    pub elapsed: Duration,
    /// Total agent conversation turns.
    pub total_turns: u32,
    /// Total cost in USD.
    pub total_cost: f64,
    /// PR URL if created.
    pub pr_url: Option<String>,
    /// Whether the run succeeded.
    pub success: bool,
}

/// Interactive TUI for displaying `coda run` progress.
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
    /// # Errors
    ///
    /// Returns an error if terminal setup fails.
    pub fn new(feature_slug: &str) -> Result<Self> {
        enable_raw_mode()?;

        let init = || -> Result<Self> {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen)?;
            let backend = CrosstermBackend::new(stdout);
            let terminal = Terminal::new(backend)?;
            Ok(Self {
                terminal,
                feature_slug: feature_slug.to_string(),
                phases: Vec::new(),
                active_phase: None,
                start_time: Instant::now(),
                total_turns: 0,
                total_cost: 0.0,
                pr_status: PrDisplayStatus::Pending,
                finished: false,
                success: false,
                spinner_tick: 0,
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
            // Non-blocking: drain all available events
            loop {
                match rx.try_recv() {
                    Ok(event) => self.handle_event(event),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        // Sender dropped — engine finished
                        self.finished = true;
                        break;
                    }
                }
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
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    return Err(anyhow::anyhow!("Cancelled by user"));
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
            }
            RunEvent::PhaseCompleted {
                index,
                duration,
                turns,
                cost_usd,
                ..
            } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Completed {
                        duration,
                        turns,
                        cost_usd,
                    };
                }
                self.total_turns += turns;
                self.total_cost += cost_usd;
                self.active_phase = None;
            }
            RunEvent::PhaseFailed { index, error, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Failed {
                        error: error.clone(),
                    };
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
            success: self.success,
        }
    }

    /// Draws the current UI state to the terminal.
    fn draw(&mut self) -> Result<()> {
        let feature_slug = self.feature_slug.clone();
        let phases = self.phases.clone();
        let active_phase = self.active_phase;
        let start_time = self.start_time;
        let total_turns = self.total_turns;
        let total_cost = self.total_cost;
        let pr_status = self.pr_status.clone();
        let finished = self.finished;
        let success = self.success;
        let spinner_tick = self.spinner_tick;

        self.terminal.draw(|frame| {
            let area = frame.area();

            // Calculate phase list height: 1 per phase + 1 for PR + 2 borders + 1 title
            let phase_count = phases.len();
            #[allow(clippy::cast_possible_truncation)]
            let phase_list_height =
                (phase_count as u16 + 1 /* PR */ + 3).min(area.height.saturating_sub(6));

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),                 // Header
                    Constraint::Length(3),                 // Pipeline bar
                    Constraint::Length(phase_list_height), // Phase details
                    Constraint::Length(3),                 // Summary bar
                    Constraint::Length(1),                 // Help bar
                    Constraint::Min(0),                    // Spacer
                ])
                .split(area);

            render_header(frame, chunks[0], &feature_slug, finished, success);
            render_pipeline(frame, chunks[1], &phases, &pr_status, spinner_tick);
            render_phase_list(
                frame,
                chunks[2],
                &phases,
                active_phase,
                &pr_status,
                spinner_tick,
            );
            render_summary(
                frame,
                chunks[3],
                start_time,
                total_turns,
                total_cost,
                &pr_status,
            );
            render_help_bar(frame, chunks[4], finished);
        })?;

        Ok(())
    }
}

impl std::fmt::Debug for RunUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunUi")
            .field("feature_slug", &self.feature_slug)
            .field("phases", &self.phases.len())
            .field("finished", &self.finished)
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
            PhaseDisplayStatus::Failed { .. } => ("✗", Style::default().fg(Color::Red)),
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

/// Renders the detailed phase list with status, timing, and cost.
fn render_phase_list(
    frame: &mut Frame,
    area: Rect,
    phases: &[PhaseDisplay],
    _active_phase: Option<usize>,
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
        let line = match &phase.status {
            PhaseDisplayStatus::Pending => Line::from(vec![Span::styled(
                format!("  [○] {:<24}", phase.name),
                Style::default().fg(Color::DarkGray),
            )]),
            PhaseDisplayStatus::Running => {
                let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
                let elapsed = phase
                    .started_at
                    .map(|t| format_duration(t.elapsed()))
                    .unwrap_or_default();

                let detail = if phase.detail.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", phase.detail)
                };

                Line::from(vec![
                    Span::styled(
                        format!("  [{spinner}] {:<24}", phase.name),
                        Style::default().fg(Color::Yellow).bold(),
                    ),
                    Span::styled(
                        format!("{elapsed:>8}  Running..."),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(detail, Style::default().fg(Color::DarkGray)),
                ])
            }
            PhaseDisplayStatus::Completed {
                duration,
                turns,
                cost_usd,
            } => Line::from(vec![
                Span::styled(
                    format!("  [●] {:<24}", phase.name),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!(
                        "{:>8}  {:>3} turns  ${cost_usd:.4}",
                        format_duration(*duration),
                        turns,
                    ),
                    Style::default().fg(Color::White),
                ),
            ]),
            PhaseDisplayStatus::Failed { error } => Line::from(vec![
                Span::styled(
                    format!("  [✗] {:<24}", phase.name),
                    Style::default().fg(Color::Red),
                ),
                Span::styled(
                    format!("Failed: {}", truncate_str(error, 50)),
                    Style::default().fg(Color::Red),
                ),
            ]),
        };
        lines.push(line);
    }

    // PR line
    let pr_line = match pr_status {
        PrDisplayStatus::Pending => Line::from(vec![Span::styled(
            "  [○] create-pr",
            Style::default().fg(Color::DarkGray),
        )]),
        PrDisplayStatus::Creating => {
            let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            Line::from(vec![Span::styled(
                format!("  [{spinner}] create-pr              Running..."),
                Style::default().fg(Color::Yellow).bold(),
            )])
        }
        PrDisplayStatus::Created { url } => Line::from(vec![
            Span::styled(
                "  [●] create-pr              ",
                Style::default().fg(Color::Green),
            ),
            Span::styled(format!("PR: {url}"), Style::default().fg(Color::Cyan)),
        ]),
        PrDisplayStatus::Failed => Line::from(vec![Span::styled(
            "  [✗] create-pr              No PR created",
            Style::default().fg(Color::Red),
        )]),
    };
    lines.push(pr_line);

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
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
fn render_help_bar(frame: &mut Frame, area: Rect, finished: bool) {
    let help = if finished {
        Line::from(vec![Span::styled(
            " Run finished. Exiting...",
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

/// Formats a `Duration` into a human-readable string (e.g., `"1m 23s"`, `"45s"`).
fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        format!("{hours}h {mins}m {secs}s")
    } else if total_secs >= 60 {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}s")
    } else {
        format!("{total_secs}s")
    }
}

/// Truncates a string to fit within `max_len` characters, appending `...` if needed.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}
