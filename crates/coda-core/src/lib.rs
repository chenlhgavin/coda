//! CODA Core Execution Engine
//!
//! The core engine for orchestrating Claude agent interactions.

mod engine;
mod error;

pub use engine::Engine;
pub use error::CoreError;
