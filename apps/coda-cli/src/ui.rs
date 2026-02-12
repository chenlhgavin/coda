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
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

/// Role label for user messages in the chat history.
const USER_ROLE: &str = "You";
/// Role label for agent messages in the chat history.
const AGENT_ROLE: &str = "Agent";

/// A single message in the chat history.
struct ChatMessage {
    role: String,
    content: String,
}

/// Interactive planning UI for multi-turn conversation with Claude.
pub struct PlanUi {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    messages: Vec<ChatMessage>,
    input: String,
    scroll_offset: u16,
    status: String,
}

impl PlanUi {
    /// Creates a new planning UI, entering the alternate screen and raw mode.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal setup fails.
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            messages: Vec::new(),
            input: String::new(),
            scroll_offset: 0,
            status: "Type your message and press Enter. /done to finalize, /quit to exit."
                .to_string(),
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
        self.messages.push(ChatMessage {
            role: AGENT_ROLE.to_string(),
            content: format!(
                "Welcome! Let's plan feature '{}'. Please describe what you'd like to build.",
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

                match key.code {
                    KeyCode::Enter => {
                        let input = self.input.trim().to_string();
                        if input.is_empty() {
                            continue;
                        }
                        self.input.clear();

                        // Handle special commands
                        if input == "/quit" {
                            return Ok(None);
                        }

                        if input == "/done" {
                            self.status = "Finalizing plan...".to_string();
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

                        // Send to agent and get response
                        self.status = "Agent is thinking...".to_string();
                        self.draw()?;

                        match session.send(&input).await {
                            Ok(response) => {
                                self.messages.push(ChatMessage {
                                    role: AGENT_ROLE.to_string(),
                                    content: response,
                                });
                                self.scroll_to_bottom();
                                self.status = "Type your message and press Enter. /done to finalize, /quit to exit.".to_string();
                            }
                            Err(e) => {
                                self.status = format!("Error: {e}");
                            }
                        }
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    KeyCode::Char(c) => {
                        self.input.push(c);
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Up => {
                        self.scroll_offset = self.scroll_offset.saturating_add(1);
                    }
                    KeyCode::Down => {
                        self.scroll_offset = self.scroll_offset.saturating_sub(1);
                    }
                    KeyCode::Esc => {
                        return Ok(None);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Scrolls the chat view to show the most recent messages.
    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Draws the current UI state to the terminal.
    fn draw(&mut self) -> Result<()> {
        let messages = &self.messages;
        let input = &self.input;
        let status = &self.status;
        let scroll_offset = self.scroll_offset;

        self.terminal.draw(|frame| {
            let area = frame.area();

            // Layout: chat area (flexible) + input (3 lines) + status (1 line)
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(5),    // Chat history
                    Constraint::Length(3), // Input box
                    Constraint::Length(1), // Status bar
                ])
                .split(area);

            // Render chat history
            render_chat(frame, chunks[0], messages, scroll_offset);

            // Render input box
            let input_block = Block::default().title(" Input ").borders(Borders::ALL);
            let input_text = Paragraph::new(input.as_str()).block(input_block);
            frame.render_widget(input_text, chunks[1]);

            // Render status bar
            let status_paragraph =
                Paragraph::new(status.as_str()).style(Style::default().fg(Color::DarkGray));
            frame.render_widget(status_paragraph, chunks[2]);
        })?;

        Ok(())
    }
}

/// Renders the chat history in the given area.
fn render_chat(frame: &mut Frame, area: Rect, messages: &[ChatMessage], scroll_offset: u16) {
    let chat_block = Block::default()
        .title(" CODA Plan - Chat ")
        .borders(Borders::ALL);
    let inner = chat_block.inner(area);
    frame.render_widget(chat_block, area);

    // Build chat text with styled roles
    let mut lines: Vec<Line> = Vec::new();
    for msg in messages {
        let role_style = if msg.role == USER_ROLE {
            Style::default().fg(Color::Cyan).bold()
        } else {
            Style::default().fg(Color::Green).bold()
        };

        lines.push(Line::from(vec![Span::styled(
            format!("[{}]", msg.role),
            role_style,
        )]));

        for line in msg.content.lines() {
            lines.push(Line::from(line.to_string()));
        }
        lines.push(Line::from(""));
    }

    let total_lines = lines.len() as u16;
    let visible_height = inner.height;
    let max_scroll = total_lines.saturating_sub(visible_height);
    let effective_scroll = max_scroll.saturating_sub(scroll_offset);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0));
    frame.render_widget(paragraph, inner);

    // Render scrollbar
    let mut scrollbar_state =
        ScrollbarState::new(total_lines as usize).position(effective_scroll as usize);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

impl Drop for PlanUi {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}
