//! Terminal UI implementation using ratatui.

use std::io::{self, Stdout};

use anyhow::Result;
use coda_core::Engine;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};

pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Ui {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    pub async fn run(&mut self, _engine: &mut Engine) -> Result<()> {
        loop {
            self.terminal.draw(|frame| {
                let area = frame.area();
                let block = Block::default()
                    .title(" CODA - Claude Orchestrated Development Agent ")
                    .borders(Borders::ALL);
                let paragraph = Paragraph::new("Press 'q' to quit")
                    .block(block)
                    .alignment(Alignment::Center);
                frame.render_widget(paragraph, area);
            })?;

            if event::poll(std::time::Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('q')
            {
                break;
            }
        }
        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
    }
}
