//! Terminal UI implementation using ratatui.
//!
//! Provides interactive chat UI for the `coda plan` command and
//! progress display for the `coda run` command.

use std::io::{self, Stdout};

use anyhow::Result;
use coda_core::planner::{PlanOutput, PlanSession};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Role label for user messages in the chat history.
const USER_ROLE: &str = "You";
/// Role label for agent messages in the chat history.
const AGENT_ROLE: &str = "Assistant";

/// Border color used throughout the plan UI.
const BORDER_COLOR: Color = Color::Cyan;

/// Input prompt displayed before the cursor.
const INPUT_PROMPT: &str = "> ";

/// Spinner animation frames for the thinking indicator.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Current phase of the planning session, displayed in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanPhase {
    /// Waiting for user input — discussing the design.
    Discussing,
    /// Agent is processing a response.
    Thinking,
    /// Agent is formalizing the approved design.
    Approving,
    /// Design has been approved, ready to finalize.
    Approved,
    /// Generating final specs and worktree.
    Finalizing,
}

impl std::fmt::Display for PlanPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Discussing => write!(f, "Discussing"),
            Self::Thinking => write!(f, "Thinking"),
            Self::Approving => write!(f, "Approving"),
            Self::Approved => write!(f, "Approved"),
            Self::Finalizing => write!(f, "Finalizing"),
        }
    }
}

/// A single message in the chat history.
struct ChatMessage {
    role: String,
    content: String,
}

/// Interactive planning UI for multi-turn conversation with Claude.
pub struct PlanUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    messages: Vec<ChatMessage>,
    editor: crate::line_editor::LineEditor,
    scroll_offset: u16,
    feature_slug: String,
    phase: PlanPhase,
}

impl PlanUi {
    /// Creates a new planning UI, entering the alternate screen and raw mode.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal setup fails.
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;

        // After enabling raw mode, any failure must restore terminal state.
        // Use a closure + inspect_err so cleanup runs on every error path.
        let init = || -> Result<Self> {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen)?;
            let backend = CrosstermBackend::new(stdout);
            let terminal = Terminal::new(backend)?;
            Ok(Self {
                terminal,
                messages: Vec::new(),
                editor: crate::line_editor::LineEditor::new(),
                scroll_offset: 0,
                feature_slug: String::new(),
                phase: PlanPhase::Discussing,
            })
        };

        init().inspect_err(|_| {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
        })
    }

    /// Runs the interactive planning session.
    ///
    /// Alternates between collecting user input and sending messages to
    /// the planning agent. Returns `PlanOutput` after finalization, or
    /// `None` if the user quits without finalizing.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal I/O, agent communication, or
    /// finalization fails.
    pub async fn run_plan(&mut self, session: &mut PlanSession) -> Result<Option<PlanOutput>> {
        self.feature_slug = session.feature_slug().to_string();
        self.phase = PlanPhase::Discussing;

        self.messages.push(ChatMessage {
            role: AGENT_ROLE.to_string(),
            content: format!(
                "Planning session started for **{}**.\n\n\
                 Please describe what you want to build. \
                 I'll first make sure I understand your intent before proposing a design.",
                session.feature_slug()
            ),
        });

        loop {
            self.draw()?;

            // Poll for keyboard events
            if event::poll(std::time::Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Global keybindings (not delegated to line editor)
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        session.disconnect().await;
                        return Ok(None);
                    }
                    KeyCode::Esc => {
                        session.disconnect().await;
                        return Ok(None);
                    }
                    KeyCode::PageUp => {
                        self.scroll_offset = self.scroll_offset.saturating_add(10);
                        continue;
                    }
                    KeyCode::PageDown => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(10);
                        continue;
                    }
                    KeyCode::Up => {
                        self.scroll_offset = self.scroll_offset.saturating_add(1);
                        continue;
                    }
                    KeyCode::Down => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(1);
                        continue;
                    }
                    _ => {}
                }

                // Submit on Enter
                if key.code == KeyCode::Enter {
                    let input = self.editor.take_trimmed();
                    if input.is_empty() {
                        continue;
                    }

                    // Handle special commands
                    if input == "/quit" {
                        session.disconnect().await;
                        return Ok(None);
                    }

                    if input == "/approve" {
                        if session.is_approved() {
                            self.messages.push(ChatMessage {
                                role: AGENT_ROLE.to_string(),
                                content: "Design is already approved. Type /done to finalize."
                                    .to_string(),
                            });
                            self.scroll_to_bottom();
                            continue;
                        }

                        self.messages.push(ChatMessage {
                            role: USER_ROLE.to_string(),
                            content: "/approve".to_string(),
                        });
                        self.scroll_to_bottom();

                        let result = self
                            .run_with_spinner(
                                session.approve(),
                                PlanPhase::Approving,
                                "Approving design & generating verification plan...",
                            )
                            .await;

                        match result {
                            Ok(Some((design, verification))) => {
                                self.messages.push(ChatMessage {
                                    role: AGENT_ROLE.to_string(),
                                    content: design,
                                });
                                self.messages.push(ChatMessage {
                                    role: AGENT_ROLE.to_string(),
                                    content: format!(
                                        "---\n\n**Verification Plan:**\n\n{verification}"
                                    ),
                                });
                                self.scroll_to_bottom();
                                self.phase = PlanPhase::Approved;
                            }
                            Ok(None) => {
                                session.disconnect().await;
                                return Ok(None);
                            }
                            Err(e) => {
                                self.messages.push(ChatMessage {
                                    role: AGENT_ROLE.to_string(),
                                    content: format!("Error during approval: {e}"),
                                });
                                self.scroll_to_bottom();
                                self.phase = PlanPhase::Discussing;
                            }
                        }
                        continue;
                    }

                    if input == "/done" {
                        if !session.is_approved() {
                            self.messages.push(ChatMessage {
                                role: AGENT_ROLE.to_string(),
                                content: "Design has not been approved yet. Type /approve first."
                                    .to_string(),
                            });
                            self.scroll_to_bottom();
                            continue;
                        }

                        self.phase = PlanPhase::Finalizing;
                        self.draw()?;

                        let output = session.finalize().await?;
                        return Ok(Some(output));
                    }

                    // Add user message to history
                    self.messages.push(ChatMessage {
                        role: USER_ROLE.to_string(),
                        content: input.clone(),
                    });
                    self.scroll_to_bottom();

                    // Send to agent with animated spinner
                    let result = self
                        .run_with_spinner(session.send(&input), PlanPhase::Thinking, "Thinking...")
                        .await;

                    match result {
                        Ok(Some(response)) => {
                            self.messages.push(ChatMessage {
                                role: AGENT_ROLE.to_string(),
                                content: response,
                            });
                            self.scroll_to_bottom();
                            self.phase = PlanPhase::Discussing;
                        }
                        Ok(None) => {
                            session.disconnect().await;
                            return Ok(None);
                        }
                        Err(e) => {
                            self.messages.push(ChatMessage {
                                role: AGENT_ROLE.to_string(),
                                content: format!("Error: {e}"),
                            });
                            self.scroll_to_bottom();
                            self.phase = PlanPhase::Discussing;
                        }
                    }
                    continue;
                }

                // Delegate all other keys to the readline-style line editor
                self.editor.handle_key(&key);
            }
        }
    }

    /// Runs a future while displaying an animated spinner in the chat.
    ///
    /// Returns `Ok(Some(value))` on success, `Ok(None)` if the user
    /// cancels with Ctrl+C, or `Err` on failure.
    async fn run_with_spinner<F, T>(
        &mut self,
        future: F,
        phase: PlanPhase,
        label: &str,
    ) -> Result<Option<T>>
    where
        F: std::future::Future<Output = Result<T, coda_core::CoreError>>,
    {
        self.messages.push(ChatMessage {
            role: AGENT_ROLE.to_string(),
            content: format!("{} {label}", SPINNER_FRAMES[0]),
        });
        let thinking_idx = self.messages.len() - 1;
        self.scroll_to_bottom();
        self.phase = phase;

        tokio::pin!(future);

        let mut spinner_tick: usize = 0;

        let result = loop {
            self.messages[thinking_idx].content = format!(
                "{} {label}",
                SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()]
            );
            spinner_tick += 1;
            self.draw()?;

            tokio::select! {
                result = &mut future => break result,
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if event::poll(std::time::Duration::from_millis(0))?
                        && let Event::Key(key) = event::read()?
                        && key.kind == KeyEventKind::Press
                        && key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.messages.pop();
                        return Ok(None);
                    }
                }
            }
        };

        self.messages.pop();

        match result {
            Ok(value) => Ok(Some(value)),
            Err(e) => Err(e.into()),
        }
    }

    /// Scrolls the chat view to show the most recent messages.
    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Draws the current UI state to the terminal.
    fn draw(&mut self) -> Result<()> {
        let messages = &self.messages;
        let input = self.editor.content();
        let input_display_width = self.editor.display_width();
        let cursor_display_offset = self.editor.cursor_display_offset();
        let scroll_offset = self.scroll_offset;
        let feature_slug = &self.feature_slug;
        let phase = self.phase;

        self.terminal.draw(|frame| {
            let area = frame.area();

            // Calculate dynamic input height based on content wrapping.
            // All widths are in terminal columns (display width), not bytes.
            let inner_width = area.width.saturating_sub(2).max(1);
            #[allow(clippy::cast_possible_truncation)]
            let prompt_display_width = UnicodeWidthStr::width(INPUT_PROMPT) as u16;
            #[allow(clippy::cast_possible_truncation)]
            let total_display_len = prompt_display_width + input_display_width as u16;
            #[allow(clippy::cast_possible_truncation)]
            let cursor_col = prompt_display_width + cursor_display_offset as u16;
            // Lines needed for the text content
            let text_lines = total_display_len.div_ceil(inner_width).max(1);
            // Lines needed so the cursor remains visible (cursor may sit one
            // position past the last character when at the end of a full line)
            let cursor_line = (cursor_col / inner_width) + 1;
            let wrapped_lines = text_lines.max(cursor_line);
            // +2 for top and bottom borders, cap to avoid stealing all chat space
            let input_height = (wrapped_lines + 2).min(area.height / 3);

            // Layout: header (1) + chat (flex) + input (dynamic) + help bar (1)
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),            // Header
                    Constraint::Min(5),               // Chat history
                    Constraint::Length(input_height), // Input box (dynamic)
                    Constraint::Length(1),            // Help bar
                ])
                .split(area);

            // Render header
            render_header(frame, chunks[0], feature_slug, phase);

            // Render chat history
            render_chat(frame, chunks[1], messages, scroll_offset);

            // Render input box with manual wrapping for correct cursor alignment.
            let input_block = Block::default()
                .title(" Input ")
                .title_style(Style::default().fg(Color::White).bold())
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(BORDER_COLOR));
            let inner_area = input_block.inner(chunks[2]);
            let visual_lines = build_input_visual_lines(input, inner_area.width);

            // Scroll the input so the cursor line is always visible.
            let iw = inner_area.width.max(1);
            let cursor_visual_line = cursor_col / iw;
            let input_scroll =
                cursor_visual_line.saturating_sub(inner_area.height.saturating_sub(1));

            let input_paragraph = Paragraph::new(visual_lines)
                .scroll((input_scroll, 0))
                .block(input_block);
            frame.render_widget(input_paragraph, chunks[2]);

            // Place cursor using display-width offsets for correct CJK support.
            let cursor_x = inner_area.x + (cursor_col % iw);
            let cursor_y = inner_area.y + cursor_visual_line.saturating_sub(input_scroll);
            frame.set_cursor_position(Position::new(cursor_x, cursor_y));

            // Render context-sensitive help bar
            render_help_bar(frame, chunks[3], phase);
        })?;

        Ok(())
    }
}

/// Renders the header line showing feature name and current phase.
fn render_header(frame: &mut Frame, area: Rect, feature_slug: &str, phase: PlanPhase) {
    let header = Line::from(vec![
        Span::styled(
            format!(" CODA Plan: {feature_slug} "),
            Style::default().fg(Color::White).bold(),
        ),
        Span::styled(format!("[{phase}]"), Style::default().fg(Color::Yellow)),
    ]);
    let paragraph = Paragraph::new(header).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

/// Calculates the number of visual (wrapped) lines a single [`Line`] occupies
/// when rendered into the given `width`, using Unicode display widths so that
/// CJK characters (2 columns each) are accounted for correctly.
fn visual_line_count(line: &Line, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let line_width: u16 = line
        .spans
        .iter()
        .map(|s| {
            #[allow(clippy::cast_possible_truncation)]
            let w = UnicodeWidthStr::width(s.content.as_ref()) as u16;
            w
        })
        .sum();
    if line_width == 0 {
        return 1;
    }
    line_width.div_ceil(width)
}

/// Renders the chat history in the given area.
fn render_chat(frame: &mut Frame, area: Rect, messages: &[ChatMessage], scroll_offset: u16) {
    let chat_block = Block::default()
        .title(" Chat ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER_COLOR));
    let inner = chat_block.inner(area);
    frame.render_widget(chat_block, area);

    // Build chat text with styled roles
    let mut lines: Vec<Line> = Vec::new();
    for msg in messages {
        let (role_style, role_label) = if msg.role == USER_ROLE {
            (Style::default().fg(Color::Cyan).bold(), "You: ")
        } else {
            (Style::default().fg(Color::Yellow).bold(), "Assistant: ")
        };

        // First line: role label + first line of content
        let mut content_lines: Vec<&str> = msg.content.lines().collect();
        let first_content_line = if content_lines.is_empty() {
            ""
        } else {
            content_lines.remove(0)
        };

        lines.push(Line::from(vec![
            Span::styled(role_label, role_style),
            Span::raw(first_content_line),
        ]));

        // Remaining lines with indentation matching the role label width
        let indent = " ".repeat(role_label.len());
        for line in content_lines {
            if line.is_empty() {
                lines.push(Line::from(""));
            } else {
                lines.push(Line::from(format!("{indent}{line}")));
            }
        }

        // Blank line between messages
        lines.push(Line::from(""));
    }

    // Calculate total visual lines (accounting for wrapping)
    let total_visual_lines: u16 = lines
        .iter()
        .map(|l| visual_line_count(l, inner.width))
        .sum();
    let visible_height = inner.height;
    let max_scroll = total_visual_lines.saturating_sub(visible_height);
    let effective_scroll = max_scroll.saturating_sub(scroll_offset);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0));
    frame.render_widget(paragraph, inner);

    // Render scrollbar
    let mut scrollbar_state =
        ScrollbarState::new(total_visual_lines as usize).position(effective_scroll as usize);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

/// Renders the bottom help bar with context-sensitive keyboard shortcuts.
fn render_help_bar(frame: &mut Frame, area: Rect, phase: PlanPhase) {
    let key_style = Style::default().fg(Color::White);
    let sep = Span::raw("  ");

    let help = match phase {
        PlanPhase::Discussing => Line::from(vec![
            Span::styled("[Enter] Send", key_style),
            sep.clone(),
            Span::styled("[/approve] Lock design", key_style),
            sep.clone(),
            Span::styled("[/done] Finalize", key_style),
            sep.clone(),
            Span::styled("[Ctrl+C] Quit", key_style),
            sep,
            Span::styled("[PgUp/PgDn] Scroll", key_style),
        ]),
        PlanPhase::Approved => Line::from(vec![
            Span::styled("[/done] Finalize & create worktree", key_style),
            sep.clone(),
            Span::styled("[Enter] Continue discussing", key_style),
            sep.clone(),
            Span::styled("[Ctrl+C] Quit", key_style),
            sep,
            Span::styled("[PgUp/PgDn] Scroll", key_style),
        ]),
        PlanPhase::Thinking | PlanPhase::Approving => {
            Line::from(vec![Span::styled("[Ctrl+C] Cancel", key_style)])
        }
        PlanPhase::Finalizing => Line::from(vec![Span::styled(
            "Finalizing — writing specs and creating worktree...",
            key_style,
        )]),
    };
    let paragraph = Paragraph::new(help).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

/// Builds visual lines for the input box by manually wrapping at exact
/// column boundaries.
///
/// This ensures the rendered text aligns perfectly with the cursor position
/// calculation (`cursor_col % width` / `cursor_col / width`). Using
/// ratatui's `Wrap` can produce misaligned results because its
/// `WordWrapper` breaks at word boundaries rather than fixed column widths.
fn build_input_visual_lines(input: &str, width: u16) -> Vec<Line<'static>> {
    let width = width as usize;
    let prompt = INPUT_PROMPT;
    let prompt_style = Style::default().fg(Color::DarkGray);

    if width == 0 {
        return vec![Line::from(vec![
            Span::styled(prompt.to_string(), prompt_style),
            Span::raw(input.to_string()),
        ])];
    }

    let prompt_width = UnicodeWidthStr::width(prompt);
    let first_line_available = width.saturating_sub(prompt_width);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut chars = input.chars().peekable();

    // First visual line: styled prompt + as many input characters as fit.
    let mut first_chunk = String::new();
    let mut col = 0;
    while let Some(&ch) = chars.peek() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col + ch_w > first_line_available {
            break;
        }
        first_chunk.push(chars.next().expect("peek guarantees Some"));
        col += ch_w;
    }
    lines.push(Line::from(vec![
        Span::styled(prompt.to_string(), prompt_style),
        Span::raw(first_chunk),
    ]));

    // Subsequent visual lines: full-width chunks.
    while chars.peek().is_some() {
        let mut chunk = String::new();
        col = 0;
        while let Some(&ch) = chars.peek() {
            let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if col + ch_w > width {
                break;
            }
            chunk.push(chars.next().expect("peek guarantees Some"));
            col += ch_w;
        }
        if chunk.is_empty() {
            // Character wider than the available width; skip to avoid
            // an infinite loop (should not happen with normal text).
            chars.next();
            continue;
        }
        lines.push(Line::from(chunk));
    }

    lines
}

impl std::fmt::Debug for PlanUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanUi")
            .field("messages", &self.messages.len())
            .field("feature_slug", &self.feature_slug)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl Drop for PlanUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}
