//! CODA Core Execution Engine
//!
//! The core engine for orchestrating Claude agent interactions. Provides
//! configuration management, task execution, agent profile selection, and
//! state tracking for CODA's development workflow.
//!
//! # Architecture
//!
//! - [`Engine`] orchestrates init, plan, and run operations
//! - [`PlanSession`] manages interactive planning conversations
//! - [`Runner`] executes phased development (dynamic dev phases → review → verify)
//! - [`CodaConfig`] holds project configuration from `.coda/config.yml`
//! - [`FeatureState`](state::FeatureState) tracks execution progress in `state.yml`

pub mod config;
mod engine;
mod error;
pub mod parser;
pub mod planner;
pub mod profile;
pub mod project;
pub mod reviewer;
pub mod runner;
pub mod state;
pub mod task;

pub use config::CodaConfig;
pub use engine::Engine;
pub use error::CoreError;
pub use planner::{PlanOutput, PlanSession};
pub use profile::{AgentProfile, build_safety_hooks};
pub use project::find_project_root;
pub use reviewer::ReviewResult;
pub use runner::{CommitInfo, ReviewSummary, RunEvent, RunProgress, Runner, VerificationSummary};
pub use task::{Task, TaskResult, TaskStatus};
