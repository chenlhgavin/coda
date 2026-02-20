//! Terminal UI for the `coda init` command.
//!
//! Provides a ratatui-based TUI that displays real-time progress of the
//! init pipeline (analyze-repo → setup-project), including a phase pipeline
//! indicator, live elapsed timers, cost tracking, and a scrollable output
//! panel that streams AI-generated text as it arrives.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use coda_core::InitEvent;
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

/// Border color used throughout the init UI.
const BORDER_COLOR: Color = Color::Cyan;

/// Maximum number of lines retained in the output buffer.
const OUTPUT_BUFFER_MAX_LINES: usize = 200;

/// Display status of a single phase in the pipeline.
#[derive(Debug, Clone)]
enum PhaseDisplayStatus {
    /// Not yet started.
    Pending,
    /// Currently running.
    Running,
    /// Completed with metrics.
    Completed { duration: Duration, cost_usd: f64 },
    /// Phase failed with an error.
    Failed { error: String },
}

/// Display state of a single phase.
#[derive(Debug, Clone)]
struct PhaseDisplay {
    name: String,
    status: PhaseDisplayStatus,
    started_at: Option<Instant>,
}

/// Summary data returned after the init TUI finishes.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
///
/// # // This example shows the struct shape; it cannot actually construct
/// # // an InitSummary without the crate, so we just demonstrate the type.
/// # #[derive(Debug)]
/// # struct InitSummary { elapsed: Duration, total_cost: f64, success: bool }
/// let summary = InitSummary {
///     elapsed: Duration::from_secs(44),
///     total_cost: 0.0123,
///     success: true,
/// };
/// assert!(summary.success);
/// ```
#[derive(Debug)]
#[allow(dead_code)]
pub struct InitSummary {
    /// Total elapsed wall-clock time.
    pub elapsed: Duration,
    /// Total cost in USD across all phases.
    pub total_cost: f64,
    /// Whether the init completed successfully.
    pub success: bool,
}

/// Interactive TUI for displaying `coda init` progress.
///
/// Consumes [`InitEvent`]s from an unbounded channel and renders a
/// live progress display with a streaming output panel.
///
/// # Examples
///
/// ```no_run
/// use coda_core::InitEvent;
/// use tokio::sync::mpsc;
///
/// # async fn example() -> anyhow::Result<()> {
/// let (tx, rx) = mpsc::unbounded_channel::<InitEvent>();
/// // In practice, InitUi::new() enters alternate screen + raw mode,
/// // so it must be called in a real terminal context.
/// # Ok(())
/// # }
/// ```
pub struct InitUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    project_root: String,
    phases: Vec<PhaseDisplay>,
    active_phase: Option<usize>,
    start_time: Instant,
    total_cost: f64,
    output_lines: Vec<String>,
    finished: bool,
    success: bool,
    spinner_tick: usize,
}

impl InitUi {
    /// Creates a new init UI, entering the alternate screen and raw mode.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal setup fails (e.g., not a real TTY).
    pub fn new(project_root: &str) -> Result<Self> {
        enable_raw_mode()?;

        let init = || -> Result<Self> {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen)?;
            let backend = CrosstermBackend::new(stdout);
            let terminal = Terminal::new(backend)?;
            Ok(Self {
                terminal,
                project_root: project_root.to_string(),
                phases: Vec::new(),
                active_phase: None,
                start_time: Instant::now(),
                total_cost: 0.0,
                output_lines: Vec::new(),
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

    /// Runs the TUI event loop, consuming [`InitEvent`]s from the receiver.
    ///
    /// Returns an [`InitSummary`] when the init finishes or the channel closes.
    ///
    /// Uses non-blocking keyboard polling (`event::poll(ZERO)`) and async
    /// `tokio::time::sleep` for the tick interval so that the caller's
    /// `tokio::select!` can interleave engine polling during each sleep.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal I/O fails or the user cancels with Ctrl+C.
    pub async fn run(&mut self, mut rx: UnboundedReceiver<InitEvent>) -> Result<InitSummary> {
        loop {
            // Non-blocking: drain all available events
            loop {
                match rx.try_recv() {
                    Ok(event) => self.handle_event(event),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        self.finished = true;
                        break;
                    }
                }
            }

            self.spinner_tick += 1;
            self.draw()?;

            if self.finished {
                // Show final state briefly so the user can see the result
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.draw()?;
                break;
            }

            // Non-blocking keyboard check: poll with ZERO timeout so we never
            // block the tokio runtime thread.
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

    /// Processes a single [`InitEvent`] and updates internal state.
    fn handle_event(&mut self, event: InitEvent) {
        match event {
            InitEvent::InitStarting { phases } => {
                self.phases = phases
                    .into_iter()
                    .map(|name| PhaseDisplay {
                        name,
                        status: PhaseDisplayStatus::Pending,
                        started_at: None,
                    })
                    .collect();
            }
            InitEvent::PhaseStarting { index, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Running;
                    phase.started_at = Some(Instant::now());
                }
                self.active_phase = Some(index);
            }
            InitEvent::StreamText { text } => {
                append_stream_text(&mut self.output_lines, &text);
            }
            InitEvent::PhaseCompleted {
                index,
                duration,
                cost_usd,
                ..
            } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Completed { duration, cost_usd };
                }
                self.total_cost += cost_usd;
                self.active_phase = None;
            }
            InitEvent::PhaseFailed { index, error, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Failed {
                        error: error.clone(),
                    };
                }
                self.active_phase = None;
            }
            InitEvent::InitFinished { success } => {
                self.finished = true;
                self.success = success;
            }
            _ => {}
        }
    }

    /// Builds the final summary from accumulated state.
    fn build_summary(&self) -> InitSummary {
        InitSummary {
            elapsed: self.start_time.elapsed(),
            total_cost: self.total_cost,
            success: self.success,
        }
    }

    /// Draws the current UI state to the terminal.
    fn draw(&mut self) -> Result<()> {
        let project_root = self.project_root.clone();
        let phases = self.phases.clone();
        let active_phase = self.active_phase;
        let start_time = self.start_time;
        let total_cost = self.total_cost;
        let finished = self.finished;
        let success = self.success;
        let spinner_tick = self.spinner_tick;
        let output_lines = self.output_lines.clone();

        self.terminal.draw(|frame| {
            let area = frame.area();

            // Phase list height: 1 per phase + 2 borders + 1 title line
            let phase_count = phases.len();
            #[allow(clippy::cast_possible_truncation)]
            let phase_list_height = (phase_count as u16 + 3).min(area.height.saturating_sub(10));

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),                 // Header
                    Constraint::Length(3),                 // Pipeline bar
                    Constraint::Length(phase_list_height), // Phase details
                    Constraint::Min(5),                    // Output panel (flexible)
                    Constraint::Length(3),                 // Summary bar
                    Constraint::Length(1),                 // Help bar
                ])
                .split(area);

            render_header(frame, chunks[0], &project_root, finished, success);
            render_pipeline(frame, chunks[1], &phases, spinner_tick);
            render_phase_list(frame, chunks[2], &phases, active_phase, spinner_tick);
            render_output_panel(frame, chunks[3], &output_lines);
            render_summary(frame, chunks[4], start_time, total_cost);
            render_help_bar(frame, chunks[5], finished);
        })?;

        Ok(())
    }
}

impl std::fmt::Debug for InitUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitUi")
            .field("project_root", &self.project_root)
            .field("phases", &self.phases.len())
            .field("output_lines", &self.output_lines.len())
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Drop for InitUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}

// ── Rendering Functions ──────────────────────────────────────────────

/// Renders the header line showing project root and overall status.
fn render_header(frame: &mut Frame, area: Rect, project_root: &str, finished: bool, success: bool) {
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
            format!(" CODA Init: {project_root} "),
            Style::default().fg(Color::White).bold(),
        ),
        status,
    ]);
    let paragraph = Paragraph::new(header).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

/// Renders the horizontal phase pipeline with status indicators.
fn render_pipeline(frame: &mut Frame, area: Rect, phases: &[PhaseDisplay], spinner_tick: usize) {
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

                Line::from(vec![
                    Span::styled(
                        format!("  [{spinner}] {:<24}", phase.name),
                        Style::default().fg(Color::Yellow).bold(),
                    ),
                    Span::styled(
                        format!("{elapsed:>8}  Running..."),
                        Style::default().fg(Color::Yellow),
                    ),
                ])
            }
            PhaseDisplayStatus::Completed { duration, cost_usd } => Line::from(vec![
                Span::styled(
                    format!("  [●] {:<24}", phase.name),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:>8}  ${cost_usd:.4}", format_duration(*duration)),
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

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

/// Renders the AI streaming output panel.
///
/// Uses `Paragraph::scroll()` with a wrap-aware row offset to ensure
/// the latest streamed text is always visible, even when long lines
/// wrap to multiple visual rows.
fn render_output_panel(frame: &mut Frame, area: Rect, output_lines: &[String]) {
    let block = Block::default()
        .title(" Output ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height as usize;
    let panel_width = inner.width as usize;

    // Calculate total visual rows accounting for line wrapping.
    // Each line occupies at least 1 row; longer lines wrap to ceil(chars / width).
    let total_rows: usize = output_lines
        .iter()
        .map(|line| wrapped_line_count(line, panel_width))
        .sum();

    #[allow(clippy::cast_possible_truncation)]
    let scroll_offset = total_rows.saturating_sub(visible_height) as u16;

    let lines: Vec<Line> = output_lines
        .iter()
        .map(|line| Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray))))
        .collect();

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0));
    frame.render_widget(paragraph, inner);
}

/// Renders the summary bar with live elapsed time and cost.
fn render_summary(frame: &mut Frame, area: Rect, start_time: Instant, total_cost: f64) {
    let block = Block::default()
        .title(" Summary ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let elapsed = format_duration(start_time.elapsed());

    let spans = vec![
        Span::styled(
            format!("  Elapsed: {elapsed}"),
            Style::default().fg(Color::White).bold(),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(
            format!("Cost: ${total_cost:.4} USD"),
            Style::default().fg(Color::White),
        ),
    ];

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, inner);
}

/// Renders the help bar with keyboard shortcuts.
fn render_help_bar(frame: &mut Frame, area: Rect, finished: bool) {
    let help = if finished {
        Line::from(vec![Span::styled(
            " Init finished. Exiting...",
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

/// Appends streaming text to the output buffer, splitting on newlines.
///
/// Maintains a rolling buffer capped at [`OUTPUT_BUFFER_MAX_LINES`] lines.
/// When the buffer exceeds the limit, the oldest lines are dropped.
///
/// # Examples
///
/// ```
/// # // This function is pub(crate) so we demonstrate the logic inline.
/// let mut buf = Vec::new();
/// // Simulating: append_stream_text(&mut buf, "line1\nline2\n");
/// // Would result in buf containing ["line1", "line2", ""]
/// ```
fn append_stream_text(buffer: &mut Vec<String>, text: &str) {
    // Split incoming text by newlines. The first fragment is appended
    // to the last existing line (streaming may split mid-line).
    let mut fragments = text.split('\n');

    if let Some(first) = fragments.next() {
        if let Some(last_line) = buffer.last_mut() {
            last_line.push_str(first);
        } else {
            buffer.push(first.to_string());
        }
    }

    for fragment in fragments {
        buffer.push(fragment.to_string());
    }

    // Enforce the rolling buffer limit
    if buffer.len() > OUTPUT_BUFFER_MAX_LINES {
        let overflow = buffer.len() - OUTPUT_BUFFER_MAX_LINES;
        buffer.drain(0..overflow);
    }
}

/// Calculates the number of visual rows a line occupies when wrapped
/// to the given panel width.
///
/// Returns at least 1 row per line. Uses character count as an
/// approximation (accurate for monospace terminals with ASCII text).
fn wrapped_line_count(line: &str, panel_width: usize) -> usize {
    if panel_width == 0 {
        return 1;
    }
    let char_count = line.chars().count();
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
    fn test_should_append_single_line_to_empty_buffer() {
        let mut buf = Vec::new();
        append_stream_text(&mut buf, "hello world");
        assert_eq!(buf, vec!["hello world"]);
    }

    #[test]
    fn test_should_append_to_existing_last_line_when_no_newline() {
        let mut buf = vec!["partial".to_string()];
        append_stream_text(&mut buf, " text");
        assert_eq!(buf, vec!["partial text"]);
    }

    #[test]
    fn test_should_split_on_newlines() {
        let mut buf = Vec::new();
        append_stream_text(&mut buf, "line1\nline2\nline3");
        assert_eq!(buf, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn test_should_handle_trailing_newline() {
        let mut buf = Vec::new();
        append_stream_text(&mut buf, "line1\n");
        assert_eq!(buf, vec!["line1", ""]);
    }

    #[test]
    fn test_should_continue_last_line_across_chunks() {
        let mut buf = Vec::new();
        append_stream_text(&mut buf, "hel");
        append_stream_text(&mut buf, "lo\nworld");
        assert_eq!(buf, vec!["hello", "world"]);
    }

    #[test]
    fn test_should_cap_buffer_at_max_lines() {
        let mut buf = Vec::new();
        // Fill buffer to the limit
        for i in 0..OUTPUT_BUFFER_MAX_LINES {
            buf.push(format!("line {i}"));
        }
        assert_eq!(buf.len(), OUTPUT_BUFFER_MAX_LINES);

        // Add one more line via newline
        append_stream_text(&mut buf, "\nnew line");
        assert_eq!(buf.len(), OUTPUT_BUFFER_MAX_LINES);
        assert_eq!(buf.last().map(String::as_str), Some("new line"));
        // Oldest line should have been dropped
        assert_eq!(buf[0], "line 1");
    }

    #[test]
    fn test_should_handle_empty_text() {
        let mut buf = vec!["existing".to_string()];
        append_stream_text(&mut buf, "");
        assert_eq!(buf, vec!["existing"]);
    }

    #[test]
    fn test_should_handle_multiple_consecutive_newlines() {
        let mut buf = Vec::new();
        append_stream_text(&mut buf, "a\n\n\nb");
        assert_eq!(buf, vec!["a", "", "", "b"]);
    }

    #[test]
    fn test_should_count_wrapped_lines_for_short_line() {
        assert_eq!(wrapped_line_count("hello", 80), 1);
    }

    #[test]
    fn test_should_count_wrapped_lines_for_long_line() {
        // 100 chars at width 80 = 2 visual rows
        let long_line = "a".repeat(100);
        assert_eq!(wrapped_line_count(&long_line, 80), 2);
    }

    #[test]
    fn test_should_count_wrapped_lines_for_empty_line() {
        assert_eq!(wrapped_line_count("", 80), 1);
    }

    #[test]
    fn test_should_count_wrapped_lines_with_zero_width() {
        assert_eq!(wrapped_line_count("hello", 0), 1);
    }

    #[test]
    fn test_should_count_wrapped_lines_exact_multiple() {
        // Exactly 80 chars at width 80 = 1 visual row
        let exact = "a".repeat(80);
        assert_eq!(wrapped_line_count(&exact, 80), 1);
        // 81 chars = 2 rows
        let one_over = "a".repeat(81);
        assert_eq!(wrapped_line_count(&one_over, 80), 2);
    }
}
