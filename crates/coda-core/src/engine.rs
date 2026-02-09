//! Core execution engine implementation.

use coda_pm::PromptManager;

use crate::CoreError;

pub struct Engine {
    prompt_manager: PromptManager,
}

impl Engine {
    pub fn new() -> Self {
        Self {
            prompt_manager: PromptManager::new(),
        }
    }

    pub fn prompt_manager(&self) -> &PromptManager {
        &self.prompt_manager
    }

    pub fn prompt_manager_mut(&mut self) -> &mut PromptManager {
        &mut self.prompt_manager
    }

    pub async fn run(&self, _input: &str) -> Result<String, CoreError> {
        // TODO: Implement agent execution logic
        Ok(String::new())
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
