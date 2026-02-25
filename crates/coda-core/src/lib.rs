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
//! - [`AgentSession`](session::AgentSession) provides shared streaming/timeout/reconnect logic
//! - [`phases`] module contains focused phase executors (dev, review, verify, docs, PR)
//! - [`StateManager`](state::StateManager) centralizes state persistence with invariant enforcement
//! - [`FeatureScanner`] discovers features from `.trees/` worktree directories
//! - [`GitOps`] / [`GhOps`] abstract external CLI operations for testability
//! - [`AsyncGitOps`](async_ops::AsyncGitOps) / [`AsyncGhOps`](async_ops::AsyncGhOps) wrap
//!   blocking CLI operations in `spawn_blocking` for async safety
//! - [`CodaConfig`] holds project configuration from `.coda/config.yml`
//! - [`FeatureState`](state::FeatureState) tracks execution progress in `state.yml`

pub mod async_ops;
pub mod codex;
pub mod config;
mod engine;
mod error;
pub mod gh;
pub mod git;
pub mod parser;
pub mod phases;
pub mod planner;
pub mod profile;
pub mod project;
pub mod reviewer;
pub mod runner;
pub mod scanner;
pub mod session;
pub mod state;
pub mod task;

pub use async_ops::{AsyncGhOps, AsyncGitOps};
pub use config::{
    AgentBackend, AgentsConfig, CodaConfig, ConfigKeyDescriptor, ConfigValueType,
    OperationAgentConfig, ReasoningEffort, ResolvedAgentConfig,
};
pub use engine::{
    CleanedWorktree, Engine, InitEvent, ResolvedConfigSummary, commit_coda_artifacts,
    commit_with_hooks, emit, remove_feature_logs, validate_feature_slug,
};
pub use error::CoreError;
pub use gh::{DefaultGhOps, GhOps, PrState, PrStatus};
pub use git::{DefaultGitOps, GitOps};
pub use phases::{CommitInfo, ReviewSummary, VerificationSummary};
pub use planner::{PlanOutput, PlanSession, PlanStreamUpdate};
pub use profile::{AgentProfile, build_safety_hooks};
pub use project::find_project_root;
pub use reviewer::{ReviewResult, ReviewSeverity};
pub use runner::{RunEvent, RunProgress, Runner};
pub use scanner::FeatureScanner;
pub use session::{AgentResponse, AgentSession, SessionConfig, SessionEvent};
pub use state::{PhaseOutcome, StateManager};
pub use task::{Task, TaskResult, TaskStatus};
