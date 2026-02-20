//! CODA Core Execution Engine
//!
//! The core engine for orchestrating Claude agent interactions. Provides
//! configuration management, task execution, agent profile selection, and
//! state tracking for CODA's development workflow.
//!
//! # Architecture
//!
//! - [`Engine`] orchestrates init, plan, run, and clean operations
//! - [`PlanSession`] manages interactive planning conversations
//! - [`Runner`] executes phased development (dynamic dev phases → review → verify)
//! - [`FeatureScanner`] discovers features from `.trees/` worktree directories
//! - [`GitOps`] / [`GhOps`] abstract external CLI operations for testability
//! - [`CodaConfig`] holds project configuration from `.coda/config.yml`
//! - [`FeatureState`](state::FeatureState) tracks execution progress in `state.yml`

pub mod config;
mod engine;
mod error;
pub mod gh;
pub mod git;
pub mod parser;
pub mod planner;
pub mod profile;
pub mod project;
pub mod reviewer;
pub mod runner;
pub mod scanner;
pub mod state;
pub mod task;

pub use config::CodaConfig;
pub use engine::{
    CleanedWorktree, Engine, InitEvent, commit_with_hooks, emit, remove_feature_logs,
    validate_feature_slug,
};
pub use error::CoreError;
pub use gh::{DefaultGhOps, GhOps, PrStatus};
pub use git::{DefaultGitOps, GitOps};
pub use planner::{PlanOutput, PlanSession};
pub use profile::{AgentProfile, build_safety_hooks};
pub use project::find_project_root;
pub use reviewer::ReviewResult;
pub use runner::{CommitInfo, ReviewSummary, RunEvent, RunProgress, Runner, VerificationSummary};
pub use scanner::FeatureScanner;
pub use task::{Task, TaskResult, TaskStatus};
