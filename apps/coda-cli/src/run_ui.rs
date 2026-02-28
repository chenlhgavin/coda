//! Terminal UI for the `coda run` command.
//!
//! Provides a ratatui-based TUI that displays real-time progress of
//! feature development phases. Uses the shared split-panel layout from
//! [`tui_widgets`]: a compact phase sidebar on the left and a scrollable
//! streaming content area on the right, plus a pipeline bar, summary bar,
//! and help bar.
//!
//! When the terminal is too narrow (< [`tui_widgets::MIN_SIDEBAR_TERMINAL_WIDTH`]),
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
use ratatui::prelude::*;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::markdown::MarkdownRenderer;
use crate::tui_widgets::{
    self, MiddlePanel, PhaseDisplay, PhaseDisplayStatus, PrDisplayStatus, SummaryFields,
};

/// Pre-loaded phase data from `state.yml` for initializing the UI
/// without waiting for engine events.
#[derive(Debug, Clone)]
pub struct InitialPhase {
    /// Phase name.
    pub name: String,
    /// Whether this phase already completed in a previous session.
    pub completed: bool,
    /// Wall-clock duration (only meaningful when `completed` is true).
    pub duration: Duration,
    /// Agent turns used (only meaningful when `completed` is true).
    pub turns: u32,
    /// Cost in USD (only meaningful when `completed` is true).
    pub cost_usd: f64,
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
    /// Whether the run finished successfully (all phases + PR creation).
    pub success: bool,
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
    /// Accumulated elapsed time from phases completed in previous sessions.
    prior_elapsed: Duration,
    total_turns: u32,
    total_cost: f64,
    pr_status: PrDisplayStatus,
    finished: bool,
    success: bool,
    spinner_tick: usize,
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
    /// Resolved config display string (e.g., "claude / claude-sonnet-4-6 / high").
    config_info: Option<String>,
}

impl RunUi {
    /// Creates a new run UI, entering the alternate screen and raw mode.
    ///
    /// `initial_phases` pre-populates the phase pipeline with status from
    /// `state.yml` so the first frame shows completed phases as green with
    /// correct metrics, and the summary includes their accumulated
    /// turns/cost/elapsed time.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal setup fails.
    pub fn new(feature_slug: &str, initial_phases: Vec<InitialPhase>) -> Result<Self> {
        enable_raw_mode()?;

        let mut prior_elapsed = Duration::ZERO;
        let mut total_turns = 0u32;
        let mut total_cost = 0.0f64;

        let phases = initial_phases
            .into_iter()
            .map(|ip| {
                if ip.completed {
                    prior_elapsed += ip.duration;
                    total_turns += ip.turns;
                    total_cost += ip.cost_usd;
                    PhaseDisplay {
                        name: ip.name,
                        status: PhaseDisplayStatus::Completed {
                            duration: ip.duration,
                            cost_usd: None,
                        },
                        started_at: None,
                        current_turn: 0,
                        detail: String::new(),
                        round_label: String::new(),
                    }
                } else {
                    PhaseDisplay {
                        name: ip.name,
                        status: PhaseDisplayStatus::Pending,
                        started_at: None,
                        current_turn: 0,
                        detail: String::new(),
                        round_label: String::new(),
                    }
                }
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
                prior_elapsed,
                total_turns,
                total_cost,
                pr_status: PrDisplayStatus::Pending,
                finished: false,
                success: false,
                spinner_tick: 0,
                md_renderer: MarkdownRenderer::new(),
                content_lines: Vec::new(),
                scroll_offset: 0,
                auto_scroll: true,
                content_visible_height: 0,
                content_visible_width: 0,
                config_info: None,
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
            RunEvent::RunStarting { phases, config } => {
                self.config_info = Some(config.to_string());
                // If phases were already pre-populated (resume scenario),
                // keep the existing state so completed phases stay green.
                if self.phases.is_empty() {
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
            }
            RunEvent::PhaseStarting { index, .. } => {
                // Skip if this phase was already pre-populated as completed
                // (replay events from resume should not revert it to Running).
                if self
                    .phases
                    .get(index)
                    .is_some_and(|p| matches!(p.status, PhaseDisplayStatus::Completed { .. }))
                {
                    return;
                }
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
            RunEvent::PhaseCompleted {
                index,
                duration,
                turns,
                cost_usd,
                ..
            } => {
                // Skip accumulation if this phase was already pre-populated
                // as completed during init (avoids double-counting on resume).
                let already_completed = self
                    .phases
                    .get(index)
                    .is_some_and(|p| matches!(p.status, PhaseDisplayStatus::Completed { .. }));

                if let Some(phase) = self.phases.get_mut(index) {
                    // Preserve turn count for sidebar display (set by
                    // TurnCompleted events during the phase, or from the
                    // completion event for replayed/resumed phases).
                    phase.current_turn = turns;
                    phase.status = PhaseDisplayStatus::Completed {
                        duration,
                        cost_usd: None,
                    };
                }
                if !already_completed {
                    self.total_turns += turns;
                    self.total_cost += cost_usd;
                }
                self.active_phase = None;
            }
            RunEvent::TurnCompleted { current_turn } => {
                if let Some(idx) = self.active_phase
                    && let Some(phase) = self.phases.get_mut(idx)
                {
                    phase.current_turn = current_turn;
                }
            }
            RunEvent::PhaseFailed { index, .. } => {
                if let Some(phase) = self.phases.get_mut(index) {
                    phase.status = PhaseDisplayStatus::Failed {
                        error: String::new(),
                    };
                }
                self.active_phase = None;
            }
            RunEvent::ReviewerCompleted {
                reviewer,
                issues_found,
            } => {
                let msg = if issues_found == 0 {
                    format!("{reviewer} review: passed")
                } else {
                    format!("{reviewer} review: {issues_found} issue(s)")
                };
                tui_widgets::append_tool_activity(&mut self.content_lines, "[review]", &msg);
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::ReviewRound {
                round,
                max_rounds,
                issues_found,
            } => {
                if let Some(idx) = self.active_phase
                    && let Some(phase) = self.phases.get_mut(idx)
                {
                    phase.round_label = format!("R{round}/{max_rounds}");
                    phase.detail = if issues_found == 0 {
                        "passed".to_owned()
                    } else {
                        format!("{issues_found} issues")
                    };
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
                tui_widgets::append_markdown_text(
                    &mut self.md_renderer,
                    &mut self.content_lines,
                    &text,
                );
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::ToolActivity { tool_name, summary } => {
                tui_widgets::append_tool_activity(&mut self.content_lines, &tool_name, &summary);
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::Connecting => {
                tui_widgets::append_markdown_text(
                    &mut self.md_renderer,
                    &mut self.content_lines,
                    "Connecting to Claude...\n",
                );
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::StderrOutput { line } => {
                tui_widgets::append_tool_activity(&mut self.content_lines, "[stderr]", line.trim());
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::IdleWarning {
                attempt,
                max_retries,
                idle_secs,
            } => {
                tui_widgets::append_tool_activity(
                    &mut self.content_lines,
                    "[warn]",
                    &format!("Agent idle for {idle_secs}s — retry {attempt}/{max_retries}",),
                );
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::AliveIdle {
                alive_idle_total_secs,
                max_alive_idle_secs,
            } => {
                tui_widgets::append_tool_activity(
                    &mut self.content_lines,
                    "[info]",
                    &format!(
                        "Process alive but idle ({alive_idle_total_secs}s / {max_alive_idle_secs}s cap) — waiting",
                    ),
                );
                if self.auto_scroll {
                    self.scroll_offset = self.max_scroll_offset();
                }
            }
            RunEvent::Reconnecting {
                attempt,
                max_retries,
            } => {
                tui_widgets::append_tool_activity(
                    &mut self.content_lines,
                    "[warn]",
                    &format!("Agent unresponsive — reconnecting (attempt {attempt}/{max_retries})",),
                );
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
            elapsed: self.prior_elapsed + self.start_time.elapsed(),
            total_turns: self.total_turns,
            total_cost: self.total_cost,
            pr_url: match &self.pr_status {
                PrDisplayStatus::Created { url } => Some(url.clone()),
                _ => None,
            },
            success: self.success,
        }
    }

    /// Returns the name of the currently active phase, if any.
    fn active_phase_name(&self) -> Option<&str> {
        self.active_phase
            .and_then(|idx| self.phases.get(idx))
            .map(|p| p.name.as_str())
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
    /// Uses a split-panel layout: the middle area is divided horizontally
    /// into a fixed-width sidebar (phase list) and a flexible content area
    /// (scrollable streaming output). When the terminal is too narrow,
    /// the sidebar is hidden and the content area fills the full width.
    fn draw(&mut self) -> Result<()> {
        let feature_slug = self.feature_slug.clone();
        let phases = self.phases.clone();
        let start_time = self.start_time;
        let prior_elapsed = self.prior_elapsed;
        // Include live turn count from the active phase so the summary
        // updates in real-time as the agent works.
        let live_turns = self.total_turns
            + self
                .active_phase
                .and_then(|idx| self.phases.get(idx))
                .map_or(0, |p| p.current_turn);
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
        let config_info = self.config_info.clone();

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

            tui_widgets::render_header(
                frame,
                chunks[0],
                "Run",
                &feature_slug,
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
                    elapsed: prior_elapsed + start_time.elapsed(),
                    turns: Some(live_turns),
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
