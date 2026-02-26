//! Terminal UI for the `coda init` command.
//!
//! Provides a ratatui-based TUI that displays real-time progress of the
//! init pipeline (analyze-repo → setup-project), using the same split-panel
//! layout as the run TUI: a compact phase sidebar on the left and a
//! scrollable streaming output panel on the right, plus a pipeline bar,
//! summary bar, and help bar.
//!
//! When the terminal is too narrow, the sidebar is hidden and the content
//! area fills the full width.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::Result;
use coda_core::InitEvent;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::markdown::MarkdownRenderer;
use crate::tui_widgets::{
    self, MiddlePanel, PhaseDisplay, PhaseDisplayStatus, PrDisplayStatus, SummaryFields,
};

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
/// live progress display with a split-panel layout matching the run TUI.
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
    /// Markdown renderer for converting streaming AI text into styled lines.
    md_renderer: MarkdownRenderer,
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
    finished: bool,
    success: bool,
    spinner_tick: usize,
    /// Resolved config display string (e.g., "claude / claude-sonnet-4-6 / high").
    config_info: Option<String>,
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
                md_renderer: MarkdownRenderer::new(),
                content_lines: Vec::new(),
                scroll_offset: 0,
                auto_scroll: true,
                content_visible_height: 0,
                content_visible_width: 0,
                finished: false,
                success: false,
                spinner_tick: 0,
                config_info: None,
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
            // Non-blocking: drain all available events, coalescing consecutive
            // text events into a single buffer append.
            let mut pending_text: Option<String> = None;
            loop {
                match rx.try_recv() {
                    Ok(InitEvent::StreamText { text }) => {
                        pending_text.get_or_insert_with(String::new).push_str(&text);
                    }
                    Ok(event) => {
                        // Flush any accumulated text before processing non-text event
                        if let Some(coalesced) = pending_text.take() {
                            self.handle_event(InitEvent::StreamText { text: coalesced });
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
                self.handle_event(InitEvent::StreamText { text: coalesced });
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

    /// Processes a single [`InitEvent`] and updates internal state.
    fn handle_event(&mut self, event: InitEvent) {
        match event {
            InitEvent::InitStarting { phases, config } => {
                self.config_info = Some(config.to_string());
                self.phases = phases
                    .into_iter()
                    .map(|name| PhaseDisplay {
                        name,
                        status: PhaseDisplayStatus::Pending,
                        started_at: None,
                        current_turn: 0,
                        detail: String::new(),
                        round_label: String::new(),
                    })
                    .collect();
            }
            InitEvent::PhaseStarting { index, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Running;
                    phase.started_at = Some(Instant::now());
                }
                self.active_phase = Some(index);
                // Clear content buffer and reset markdown renderer for the new phase
                self.content_lines.clear();
                self.md_renderer = MarkdownRenderer::new();
                self.scroll_offset = 0;
                self.auto_scroll = true;
            }
            InitEvent::StreamText { text } => {
                tui_widgets::append_markdown_text(
                    &mut self.md_renderer,
                    &mut self.content_lines,
                    &text,
                );
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            InitEvent::PhaseCompleted {
                index,
                duration,
                cost_usd,
                ..
            } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Completed {
                        duration,
                        cost_usd: Some(cost_usd),
                    };
                }
                self.total_cost += cost_usd;
                self.active_phase = None;
            }
            InitEvent::PhaseFailed { index, error, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Failed { error };
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

    /// Computes the maximum scroll offset based on visual row count and visible height.
    fn max_scroll_offset(&self) -> u16 {
        tui_widgets::max_scroll_offset(
            &self.content_lines,
            self.content_visible_width,
            self.content_visible_height,
        )
    }

    /// Draws the current UI state to the terminal.
    ///
    /// Uses the same split-panel layout as the run TUI: the middle area
    /// is divided horizontally into a fixed-width sidebar (phase list)
    /// and a flexible content area (scrollable streaming output).
    fn draw(&mut self) -> Result<()> {
        let project_root = self.project_root.clone();
        let phases = self.phases.clone();
        let start_time = self.start_time;
        let total_cost = self.total_cost;
        let finished = self.finished;
        let success = self.success;
        let spinner_tick = self.spinner_tick;
        let scroll_offset = self.scroll_offset;
        let pr_status = PrDisplayStatus::NotApplicable;
        let is_running = self.active_phase.is_some();
        let active_phase_name = self
            .active_phase
            .and_then(|idx| self.phases.get(idx))
            .map(|p| p.name.as_str())
            .unwrap_or("")
            .to_string();

        // Move content_lines out temporarily via O(1) pointer swap
        let content_lines = std::mem::take(&mut self.content_lines);
        let config_info = self.config_info.clone();

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

            tui_widgets::render_header(
                frame,
                chunks[0],
                "Init",
                &project_root,
                finished,
                success,
                config_info.as_deref(),
            );
            tui_widgets::render_pipeline(frame, chunks[1], &phases, &pr_status, spinner_tick);

            (visible_height, visible_width) = tui_widgets::render_middle(
                frame,
                chunks[2],
                &MiddlePanel {
                    phases: &phases,
                    pr_status: &pr_status,
                    spinner_tick,
                    content_lines: &content_lines,
                    scroll_offset,
                    content_title: &active_phase_name,
                },
            );

            tui_widgets::render_summary(
                frame,
                chunks[3],
                &SummaryFields {
                    elapsed: start_time.elapsed(),
                    turns: None,
                    cost: total_cost,
                    pr_status: &pr_status,
                },
            );
            tui_widgets::render_help_bar(frame, chunks[4], finished, is_running);
        })?;

        self.content_visible_height = visible_height;
        self.content_visible_width = visible_width;
        // Restore content_lines after draw (O(1) pointer swap)
        self.content_lines = content_lines;

        Ok(())
    }
}

impl std::fmt::Debug for InitUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitUi")
            .field("project_root", &self.project_root)
            .field("phases", &self.phases.len())
            .field("content_lines", &self.content_lines.len())
            .field("finished", &self.finished)
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

impl Drop for InitUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}
