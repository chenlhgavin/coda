//! Application state and logic.

use anyhow::Result;
use coda_core::Engine;

use crate::ui::Ui;

pub struct App {
    engine: Engine,
}

impl App {
    pub fn new() -> Self {
        Self {
            engine: Engine::new(),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut ui = Ui::new()?;
        ui.run(&mut self.engine).await
    }

    pub async fn execute(&self, prompt: &str) -> Result<()> {
        let result = self.engine.run(prompt).await?;
        println!("{}", result);
        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
