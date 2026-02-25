//! Planning session for interactive feature planning.
//!
//! Provides `PlanSession` which wraps an [`AgentSession`](crate::session::AgentSession)
//! with the Planner profile for multi-turn feature planning conversations,
//! and `PlanOutput` describing the artifacts produced by a successful
//! planning session.
//!
//! The agent interaction logic (streaming, timeout, reconnect) is delegated
//! to [`AgentSession`](crate::session::AgentSession), eliminating the
//! previously duplicated streaming pattern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_agent_sdk_rs::ClaudeClient;
use coda_pm::PromptManager;
use tracing::{debug, info, warn};

use crate::CoreError;
use crate::async_ops::{AsyncGitOps, commit_coda_artifacts_async};
use crate::config::CodaConfig;
use crate::git::GitOps;
use crate::profile::AgentProfile;
use crate::session::{AgentSession, SessionConfig, SessionEvent};
use crate::state::{
    FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseKind, PhaseRecord, PhaseStatus,
    TokenCost, TotalStats,
};

/// Incremental update from a streaming plan conversation.
///
/// Sent through a channel by [`PlanSession::send_streaming`] so that the
/// caller can display live progress while the agent is still generating.
#[derive(Debug, Clone)]
pub enum PlanStreamUpdate {
    /// Accumulated AI response text so far.
    Text(String),
    /// The agent is invoking a tool (e.g., reading a file, running a command).
    ToolActivity {
        /// Tool name (e.g., "Read", "Grep", "Bash").
        tool_name: String,
        /// Brief description of the tool invocation (e.g., file path, command).
        summary: String,
    },
}

/// Output produced by finalizing a planning session.
#[derive(Debug)]
pub struct PlanOutput {
    /// Path to the generated design spec (`<worktree>/.coda/<slug>/specs/design.md`).
    pub design_spec: PathBuf,

    /// Path to the generated verification plan (`<worktree>/.coda/<slug>/specs/verification.md`).
    pub verification: PathBuf,

    /// Path to the feature state file (`<worktree>/.coda/<slug>/state.yml`).
    pub state: PathBuf,

    /// Path to the git worktree (`.trees/<slug>/`).
    pub worktree: PathBuf,
}

/// Fixed quality-assurance phase names appended after dynamic dev phases.
const QUALITY_PHASES: &[&str] = &["review", "verify", "update-docs"];

/// An interactive planning session wrapping an [`AgentSession`] with the
/// Planner profile for multi-turn feature planning conversations.
///
/// # Usage
///
/// 1. Create via [`PlanSession::new`]
/// 2. Call [`send`](Self::send) repeatedly for conversation turns
/// 3. Call [`approve`](Self::approve) to formalize the approved design
/// 4. Call [`finalize`](Self::finalize) to write specs and create a worktree
pub struct PlanSession {
    session: AgentSession,
    feature_slug: String,
    project_root: PathBuf,
    pm: PromptManager,
    config: CodaConfig,
    /// The formalized design spec produced by [`approve`](Self::approve).
    approved_design: Option<String>,
    /// The verification plan produced by [`approve`](Self::approve).
    approved_verification: Option<String>,
    /// Git operations implementation (async wrapper for blocking safety).
    git: AsyncGitOps,
    /// Accumulated API cost in USD across all planning turns.
    planning_cost_usd: f64,
    /// Accumulated conversation turns across the entire planning session.
    planning_turns: u32,
}

impl PlanSession {
    /// Creates a new planning session for the given feature.
    ///
    /// Initializes an [`AgentSession`] with the Planner profile and renders
    /// the planning system prompt with repository context.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if the system prompt cannot be rendered.
    pub fn new(
        feature_slug: String,
        project_root: PathBuf,
        pm: &PromptManager,
        config: &CodaConfig,
        git: Arc<dyn GitOps>,
    ) -> Result<Self, CoreError> {
        let coda_md_path = project_root.join(".coda.md");
        let coda_md = match std::fs::read_to_string(&coda_md_path) {
            Ok(content) => content,
            Err(_) => {
                return Err(CoreError::PlanError(format!(
                    "Missing .coda.md at {}. Run `coda init` first to generate it.",
                    coda_md_path.display(),
                )));
            }
        };

        let system_prompt = pm.render("plan/system", minijinja::context!(coda_md => coda_md))?;

        let mut options = AgentProfile::Planner.to_options(
            &system_prompt,
            project_root.clone(),
            config.agent.max_turns,
            config.agent.max_budget_usd,
            &config.agent.model,
        );

        // Enable partial messages so the SDK emits token-level text deltas,
        // allowing the TUI to display live streaming during planning.
        options.include_partial_messages = true;

        let client = ClaudeClient::new(options);
        let session_config = SessionConfig {
            idle_timeout_secs: config.agent.idle_timeout_secs,
            tool_execution_timeout_secs: config.agent.tool_execution_timeout_secs,
            idle_retries: config.agent.idle_retries,
            max_budget_usd: config.agent.max_budget_usd,
        };
        let session = AgentSession::new(client, session_config);

        Ok(Self {
            session,
            feature_slug,
            project_root,
            pm: pm.clone(),
            config: config.clone(),
            approved_design: None,
            approved_verification: None,
            git: AsyncGitOps::new(git),
            planning_cost_usd: 0.0,
            planning_turns: 0,
        })
    }

    /// Connects the underlying agent session to the Claude process.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` if the connection fails.
    pub async fn connect(&mut self) -> Result<(), CoreError> {
        self.session.connect().await?;
        debug!("PlanSession connected to Claude");
        Ok(())
    }

    /// Disconnects the underlying agent session.
    ///
    /// Safe to call multiple times or when not connected.
    pub async fn disconnect(&mut self) {
        self.session.disconnect().await;
        debug!("PlanSession disconnected from Claude");
    }

    /// Sends a user message and collects the agent's response.
    ///
    /// Automatically connects on the first call.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` if the query or response
    /// streaming fails.
    pub async fn send(&mut self, message: &str) -> Result<String, CoreError> {
        self.send_inner(message, None).await
    }

    /// Sends a user message and streams the agent's response as it generates.
    ///
    /// Similar to [`send`](Self::send), but sends [`PlanStreamUpdate`] events
    /// via `update_tx` as the agent generates text and invokes tools. This
    /// enables the caller to display partial responses and tool activity
    /// while the agent is still working.
    ///
    /// Automatically connects on the first call.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` if the query or response
    /// streaming fails.
    pub async fn send_streaming(
        &mut self,
        message: &str,
        update_tx: tokio::sync::mpsc::UnboundedSender<PlanStreamUpdate>,
    ) -> Result<String, CoreError> {
        self.send_inner(message, Some(update_tx)).await
    }

    /// Shared implementation for [`send`](Self::send) and
    /// [`send_streaming`](Self::send_streaming).
    ///
    /// Delegates to [`AgentSession::send`] for the actual streaming,
    /// timeout, and reconnection logic. When `update_tx` is `Some`,
    /// bridges [`SessionEvent`] to [`PlanStreamUpdate`] events.
    ///
    /// On [`SessionEvent::Reconnecting`], the accumulated text buffer is
    /// cleared and an empty [`PlanStreamUpdate::Text`] is emitted so that
    /// the UI discards stale partial text from the timed-out attempt.
    async fn send_inner(
        &mut self,
        message: &str,
        update_tx: Option<tokio::sync::mpsc::UnboundedSender<PlanStreamUpdate>>,
    ) -> Result<String, CoreError> {
        // Set up event bridging if streaming is requested
        if let Some(ref plan_tx) = update_tx {
            let (session_tx, mut session_rx) =
                tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
            let plan_tx_clone = plan_tx.clone();
            // Accumulated text for PlanStreamUpdate::Text (sends the full
            // accumulated text on each delta, matching the original behavior).
            // On reconnect, the buffer is cleared so stale partial text from
            // the timed-out attempt is discarded.
            tokio::spawn(async move {
                let mut accumulated = String::new();
                while let Some(event) = session_rx.recv().await {
                    match event {
                        SessionEvent::TextDelta { text } => {
                            accumulated.push_str(&text);
                            let _ = plan_tx_clone.send(PlanStreamUpdate::Text(accumulated.clone()));
                        }
                        SessionEvent::ToolActivity { tool_name, summary } => {
                            let _ = plan_tx_clone
                                .send(PlanStreamUpdate::ToolActivity { tool_name, summary });
                        }
                        SessionEvent::Reconnecting { .. } => {
                            // Agent subprocess is being replaced after idle timeout.
                            // Clear accumulated text and notify the UI to reset its
                            // display buffer — the new subprocess will re-stream
                            // from scratch.
                            accumulated.clear();
                            let _ = plan_tx_clone.send(PlanStreamUpdate::Text(String::new()));
                        }
                        _ => {}
                    }
                }
            });
            self.session.set_event_sender(session_tx);
        }

        let result = self.session.send(message, None).await;

        // Clear the per-call event sender so the spawned forwarder task
        // observes channel closure and terminates. This must run on both
        // success and error paths to prevent the forwarder (which holds
        // a clone of plan_tx) from keeping the receiver alive indefinitely.
        if update_tx.is_some() {
            self.session.clear_event_sender();
        }

        let resp = result?;

        // Extract cost and turns from the response
        if let Some(ref result) = resp.result {
            if let Some(cost) = result.total_cost_usd {
                self.planning_cost_usd += cost;
            }
            self.planning_turns += result.num_turns;
        }

        Ok(resp.text)
    }

    /// Formalizes the approved design and generates a verification plan.
    ///
    /// First asks the agent to produce a structured design specification
    /// document, then generates a verification plan based on the design.
    /// Both are stored so [`finalize`](Self::finalize) can write them
    /// directly without re-generating.
    ///
    /// Returns `(design, verification)` so the UI can display both.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if template rendering or agent communication fails.
    pub async fn approve(&mut self) -> Result<(String, String), CoreError> {
        // Step 1: Generate formal design spec
        let approve_prompt = self.pm.render(
            "plan/approve",
            minijinja::context!(
                feature_slug => &self.feature_slug,
            ),
        )?;

        let design = self.send(&approve_prompt).await?;
        info!("Design approved and formalized");

        // Step 2: Generate verification plan
        let verification_prompt = self.pm.render(
            "plan/verification",
            minijinja::context!(
                design_spec => &design,
                checks => &self.config.checks,
                feature_slug => &self.feature_slug,
            ),
        )?;

        let verification = match self.send(&verification_prompt).await {
            Ok(v) => v,
            Err(e) => {
                // Both must succeed atomically; don't leave partial state
                return Err(e);
            }
        };
        info!("Verification plan generated");

        // Store both only after both succeed
        self.approved_design = Some(design.clone());
        self.approved_verification = Some(verification.clone());

        Ok((design, verification))
    }

    /// Returns `true` if both design and verification have been approved
    /// via [`approve`](Self::approve).
    pub fn is_approved(&self) -> bool {
        self.approved_design.is_some() && self.approved_verification.is_some()
    }

    /// Finalizes the planning session by creating a worktree and writing specs.
    ///
    /// This method:
    /// 1. Creates a git worktree from the base branch
    /// 2. Writes the approved design spec into the worktree
    /// 3. Writes the approved verification plan into the worktree
    /// 4. Writes the initial `state.yml` into the worktree
    ///
    /// All feature artifacts are written under `<worktree>/.coda/<slug>/`
    /// so they travel with the feature branch and merge cleanly into main.
    ///
    /// Git operations are run via `spawn_blocking` to avoid blocking
    /// the async runtime.
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if git operations, directory creation, or
    /// file writes fail.
    pub async fn finalize(&mut self) -> Result<PlanOutput, CoreError> {
        let slug = self.feature_slug.clone();
        let worktree_abs = self.project_root.join(".trees").join(&slug);
        let worktree_rel = PathBuf::from(".trees").join(&slug);

        // Guard: design and verification must be approved before finalizing.
        // Clone instead of take so a retry is possible if a later step fails.
        let design_content = self.approved_design.clone().ok_or_else(|| {
            CoreError::PlanError(
                "Cannot finalize: design has not been approved. Use /approve first.".to_string(),
            )
        })?;
        let verification_content = self.approved_verification.clone().ok_or_else(|| {
            CoreError::PlanError("Cannot finalize: verification plan is missing.".to_string())
        })?;

        // 1. Create git worktree
        let base_branch = if self.config.git.base_branch == "auto" {
            self.git.detect_default_branch().await
        } else {
            self.config.git.base_branch.clone()
        };

        // Pre-flight: verify CODA config is committed to the base branch.
        // If missing (e.g., user used --no-commit or modified files after
        // init), auto-commit the init artifacts as a fallback.
        if !self
            .git
            .file_exists_in_ref(&base_branch, ".coda/config.yml")
            .await
        {
            warn!(
                base = %base_branch,
                "CODA init files not committed, auto-committing before worktree creation"
            );
            let paths: &[&str] = &[".coda/", ".coda.md", "CLAUDE.md", ".gitignore"];
            commit_coda_artifacts_async(
                &self.git,
                &self.project_root,
                paths,
                "chore: initialize CODA project",
            )
            .await?;

            // Re-check: if still missing, the files don't exist at all
            if !self
                .git
                .file_exists_in_ref(&base_branch, ".coda/config.yml")
                .await
            {
                return Err(CoreError::PlanError(format!(
                    "CODA init files not found on '{base_branch}' even after auto-commit.\n  \
                     Run `coda init` first to initialize the project."
                )));
            }
        }

        let branch_name = format!("{}/{}", self.config.git.branch_prefix, slug);
        if let Some(parent) = worktree_abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(CoreError::IoError)?;
        }
        self.git
            .worktree_add(&worktree_abs, &branch_name, &base_branch)
            .await?;
        info!(
            branch = %branch_name,
            worktree = %worktree_abs.display(),
            "Created git worktree"
        );

        // 2. Write artifacts into worktree/.coda/<slug>/
        let coda_feature_dir = worktree_abs.join(".coda").join(&slug);
        let specs_dir = coda_feature_dir.join("specs");
        tokio::fs::create_dir_all(&specs_dir)
            .await
            .map_err(CoreError::IoError)?;

        let design_spec_path = specs_dir.join("design.md");
        tokio::fs::write(&design_spec_path, &design_content)
            .await
            .map_err(CoreError::IoError)?;
        debug!(path = %design_spec_path.display(), "Wrote design spec");

        let verification_path = specs_dir.join("verification.md");
        tokio::fs::write(&verification_path, &verification_content)
            .await
            .map_err(CoreError::IoError)?;
        debug!(path = %verification_path.display(), "Wrote verification plan");

        // 3. Write initial state.yml (store relative worktree path for portability)
        let dev_phases = extract_dev_phases(&design_content);
        let state = build_initial_state(
            &self.feature_slug,
            &worktree_rel,
            &branch_name,
            &base_branch,
            &dev_phases,
            self.planning_turns,
            self.planning_cost_usd,
        );
        let state_path = coda_feature_dir.join("state.yml");
        let state_yaml = serde_yaml::to_string(&state)?;
        tokio::fs::write(&state_path, state_yaml)
            .await
            .map_err(CoreError::IoError)?;
        debug!(path = %state_path.display(), "Wrote state.yml");

        // 4. Initial commit so planning artifacts are version-controlled
        commit_coda_artifacts_async(
            &self.git,
            &worktree_abs,
            &[".coda/"],
            &format!("feat({slug}): initialize planning artifacts"),
        )
        .await?;

        // 5. Clear approved state only after everything succeeded
        self.approved_design = None;
        self.approved_verification = None;

        // 6. Disconnect client
        self.disconnect().await;

        info!("Planning session finalized successfully");

        Ok(PlanOutput {
            design_spec: design_spec_path,
            verification: verification_path,
            state: state_path,
            worktree: worktree_abs,
        })
    }

    /// Returns the directory name for this feature (the slug itself).
    pub fn feature_dir_name(&self) -> &str {
        &self.feature_slug
    }

    /// Returns the feature slug.
    pub fn feature_slug(&self) -> &str {
        &self.feature_slug
    }
}

impl std::fmt::Debug for PlanSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanSession")
            .field("feature_slug", &self.feature_slug)
            .field("project_root", &self.project_root)
            .field("connected", &self.session.is_connected())
            .finish_non_exhaustive()
    }
}

/// Extracts development phase names from a design specification.
///
/// Matches headings like `## Phase 1: <name>`, `### Phase 1: <name>`, or
/// `#### Phase 1: <name>` (2–4 `#` levels). Parenthetical annotations
/// such as `(Day 1)` or `(Day 1-2)` are stripped before slugifying.
/// Falls back to a single `"default"` phase if no phase headings are found.
///
/// # Examples
///
/// ```
/// # use coda_core::planner::extract_dev_phases;
/// let design = "### Phase 1: Type Definitions\n### Phase 2: Transport Layer\n";
/// let phases = extract_dev_phases(design);
/// assert_eq!(phases, vec!["type-definitions", "transport-layer"]);
/// ```
pub fn extract_dev_phases(design_content: &str) -> Vec<String> {
    let mut phases = Vec::new();

    for line in design_content.lines() {
        let trimmed = line.trim();
        // Match 2–4 leading '#' followed by " Phase "
        let rest = if let Some(r) = trimmed.strip_prefix("#### Phase ") {
            Some(r)
        } else if let Some(r) = trimmed.strip_prefix("### Phase ") {
            Some(r)
        } else {
            trimmed.strip_prefix("## Phase ")
        };
        let Some(rest) = rest else { continue };

        // Skip the number and colon: "1: <name>" → "<name>"
        if let Some((_num, name)) = rest.split_once(':') {
            let name = strip_parenthetical(name.trim());
            if !name.is_empty() {
                let slug = slugify_phase_name(&name);
                if !slug.is_empty() {
                    phases.push(slug);
                }
            }
        }
    }

    if phases.is_empty() {
        phases.push("default".to_string());
    }

    phases
}

/// Strips trailing parenthetical annotations from a phase name.
///
/// For example, `"Type Definitions (Day 1)"` becomes `"Type Definitions"`.
fn strip_parenthetical(name: &str) -> String {
    if let Some(idx) = name.rfind('(') {
        let before = name[..idx].trim_end();
        if !before.is_empty() {
            return before.to_string();
        }
    }
    name.to_string()
}

/// Converts a phase name into a slug suitable for use as a phase identifier.
///
/// Lowercases ASCII characters, preserves non-ASCII (e.g., CJK) as-is,
/// replaces spaces/underscores/punctuation with hyphens, and collapses
/// consecutive hyphens.
fn slugify_phase_name(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                if c.is_ascii() {
                    c.to_ascii_lowercase()
                } else {
                    c
                }
            } else {
                '-'
            }
        })
        .collect();

    // Collapse consecutive hyphens and trim leading/trailing hyphens
    let mut result = String::with_capacity(slug.len());
    let mut prev_was_hyphen = true; // Start true to trim leading
    for c in slug.chars() {
        if c == '-' {
            if !prev_was_hyphen {
                result.push('-');
            }
            prev_was_hyphen = true;
        } else {
            result.push(c);
            prev_was_hyphen = false;
        }
    }

    // Trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }

    result
}

/// Builds an initial `FeatureState` for a newly planned feature.
///
/// Creates dev phases from `dev_phase_names`, appends the fixed
/// review + verify + update-docs quality phases, and initialises everything
/// to `Pending` with zeroed cost/duration statistics. Planning session costs
/// are recorded in `total` so they appear in status reports.
pub(crate) fn build_initial_state(
    feature_slug: &str,
    worktree_path: &Path,
    branch: &str,
    base_branch: &str,
    dev_phase_names: &[String],
    planning_turns: u32,
    planning_cost_usd: f64,
) -> FeatureState {
    let now = chrono::Utc::now();

    let make_record = |name: &str, kind: PhaseKind| PhaseRecord {
        name: name.to_string(),
        kind,
        status: PhaseStatus::Pending,
        started_at: None,
        completed_at: None,
        turns: 0,
        cost_usd: 0.0,
        cost: TokenCost::default(),
        duration_secs: 0,
        details: serde_json::json!({}),
    };

    let mut phases: Vec<PhaseRecord> = dev_phase_names
        .iter()
        .map(|name| make_record(name, PhaseKind::Dev))
        .collect();

    for &qp in QUALITY_PHASES {
        phases.push(make_record(qp, PhaseKind::Quality));
    }

    FeatureState {
        feature: FeatureInfo {
            slug: feature_slug.to_string(),
            created_at: now,
            updated_at: now,
        },
        status: FeatureStatus::Planned,
        current_phase: 0,
        git: GitInfo {
            worktree_path: worktree_path.to_path_buf(),
            branch: branch.to_string(),
            base_branch: base_branch.to_string(),
        },
        phases,
        pr: None,
        total: TotalStats {
            turns: planning_turns,
            cost_usd: planning_cost_usd,
            ..TotalStats::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_build_initial_state_with_dynamic_dev_phases() {
        let worktree = PathBuf::from(".trees/add-auth");
        let dev_phases = vec![
            "type-definitions".to_string(),
            "transport-layer".to_string(),
            "client-methods".to_string(),
        ];
        let state = build_initial_state(
            "add-auth",
            &worktree,
            "feature/add-auth",
            "main",
            &dev_phases,
            5,
            0.42,
        );

        assert_eq!(state.feature.slug, "add-auth");
        assert!(state.feature.created_at <= chrono::Utc::now());
        assert!(state.feature.updated_at <= chrono::Utc::now());

        assert_eq!(state.status, FeatureStatus::Planned);
        assert_eq!(state.current_phase, 0);

        assert_eq!(state.git.worktree_path, PathBuf::from(".trees/add-auth"));
        assert_eq!(state.git.branch, "feature/add-auth");
        assert_eq!(state.git.base_branch, "main");

        // Phases: 3 dev + 3 quality = 6
        assert_eq!(state.phases.len(), 6);
        let names: Vec<&str> = state.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "type-definitions",
                "transport-layer",
                "client-methods",
                "review",
                "verify",
                "update-docs",
            ]
        );

        assert_eq!(state.phases[0].kind, PhaseKind::Dev);
        assert_eq!(state.phases[3].kind, PhaseKind::Quality);
        assert_eq!(state.phases[4].kind, PhaseKind::Quality);
        assert_eq!(state.phases[5].kind, PhaseKind::Quality);

        for phase in &state.phases {
            assert_eq!(phase.status, PhaseStatus::Pending);
            assert!(phase.started_at.is_none());
            assert_eq!(phase.turns, 0);
        }

        assert!(state.pr.is_none());
        // Planning cost is preserved in total stats
        assert_eq!(state.total.turns, 5);
        assert!((state.total.cost_usd - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_build_initial_state_serializable_to_yaml() {
        let worktree = PathBuf::from(".trees/new-feature");
        let dev_phases = vec!["phase-one".to_string(), "phase-two".to_string()];
        let state = build_initial_state(
            "new-feature",
            &worktree,
            "feature/new-feature",
            "main",
            &dev_phases,
            0,
            0.0,
        );

        let yaml = serde_yaml::to_string(&state).unwrap();
        assert!(yaml.contains("planned"));
        assert!(yaml.contains("new-feature"));
        assert!(yaml.contains("phase-one"));
        assert!(yaml.contains("review"));
        assert!(yaml.contains("verify"));
        assert!(yaml.contains("update-docs"));

        let deserialized: FeatureState = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.phases.len(), 5);
        assert_eq!(deserialized.status, FeatureStatus::Planned);
    }

    #[test]
    fn test_should_extract_dev_phases_from_design_spec() {
        let design = r#"
# Feature: support-image-input

## Development Phases

### Phase 1: Type Definitions (Day 1)
- **Goal**: Define types
- **Tasks**:
  - Create models

### Phase 2: Transport Layer (Day 1-2)
- **Goal**: Implement transport
- **Tasks**:
  - Wire up transport

### Phase 3: Testing & Documentation (Day 3)
- **Goal**: Complete testing
"#;

        let phases = extract_dev_phases(design);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0], "type-definitions");
        assert_eq!(phases[1], "transport-layer");
        assert_eq!(phases[2], "testing-documentation");
    }

    #[test]
    fn test_should_extract_phases_with_varied_heading_levels() {
        let design = "## Phase 1: Setup\n#### Phase 2: Refactor\n";
        let phases = extract_dev_phases(design);
        assert_eq!(phases, vec!["setup", "refactor"]);
    }

    #[test]
    fn test_should_extract_chinese_dev_phases() {
        let design = r#"
### Phase 1: 基础设施
### Phase 2: Init 命令
### Phase 3: Plan 命令
"#;

        let phases = extract_dev_phases(design);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0], "基础设施");
        assert_eq!(phases[1], "init-命令");
        assert_eq!(phases[2], "plan-命令");
    }

    #[test]
    fn test_should_fallback_to_default_phase() {
        let design = "# Feature without phases\nJust some text.";
        let phases = extract_dev_phases(design);
        assert_eq!(phases, vec!["default"]);
    }

    #[test]
    fn test_should_slugify_phase_names_correctly() {
        assert_eq!(slugify_phase_name("Type Definitions"), "type-definitions");
        assert_eq!(
            slugify_phase_name("Testing & Documentation"),
            "testing-documentation"
        );
        assert_eq!(slugify_phase_name("  spaced  "), "spaced");
    }

    #[test]
    fn test_should_strip_parenthetical_annotations() {
        assert_eq!(
            strip_parenthetical("Type Definitions (Day 1)"),
            "Type Definitions"
        );
        assert_eq!(
            strip_parenthetical("Transport Layer (Day 1-2)"),
            "Transport Layer"
        );
        assert_eq!(strip_parenthetical("No parens"), "No parens");
        assert_eq!(strip_parenthetical("(all parens)"), "(all parens)");
    }

    /// Verifies that the event forwarding logic in `send_inner` clears
    /// the accumulated text buffer when a `Reconnecting` event arrives.
    ///
    /// This test simulates the forwarder task directly (without a real
    /// `AgentSession`) by sending events through the same channel pattern
    /// used in `send_inner`.
    #[tokio::test]
    async fn test_should_clear_accumulated_text_on_reconnect() {
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (plan_tx, mut plan_rx) = tokio::sync::mpsc::unbounded_channel::<PlanStreamUpdate>();

        // Spawn a forwarder task identical to the one in send_inner
        tokio::spawn(async move {
            let mut accumulated = String::new();
            while let Some(event) = session_rx.recv().await {
                match event {
                    SessionEvent::TextDelta { text } => {
                        accumulated.push_str(&text);
                        let _ = plan_tx.send(PlanStreamUpdate::Text(accumulated.clone()));
                    }
                    SessionEvent::ToolActivity { tool_name, summary } => {
                        let _ = plan_tx.send(PlanStreamUpdate::ToolActivity { tool_name, summary });
                    }
                    SessionEvent::Reconnecting { .. } => {
                        accumulated.clear();
                        let _ = plan_tx.send(PlanStreamUpdate::Text(String::new()));
                    }
                    _ => {}
                }
            }
        });

        // Phase 1: Send text deltas (simulating pre-timeout output)
        session_tx
            .send(SessionEvent::TextDelta {
                text: "Hello ".to_string(),
            })
            .unwrap();
        session_tx
            .send(SessionEvent::TextDelta {
                text: "world".to_string(),
            })
            .unwrap();

        // Drain accumulated updates
        let update1 = plan_rx.recv().await.unwrap();
        let update2 = plan_rx.recv().await.unwrap();
        assert!(matches!(update1, PlanStreamUpdate::Text(ref t) if t == "Hello "));
        assert!(matches!(update2, PlanStreamUpdate::Text(ref t) if t == "Hello world"));

        // Phase 2: Reconnect event (simulating idle timeout recovery)
        session_tx
            .send(SessionEvent::Reconnecting {
                attempt: 1,
                max_retries: 3,
            })
            .unwrap();

        let reconnect_update = plan_rx.recv().await.unwrap();
        assert!(
            matches!(reconnect_update, PlanStreamUpdate::Text(ref t) if t.is_empty()),
            "Reconnect should emit empty text to clear UI buffer"
        );

        // Phase 3: New text from fresh subprocess
        session_tx
            .send(SessionEvent::TextDelta {
                text: "Fresh ".to_string(),
            })
            .unwrap();
        session_tx
            .send(SessionEvent::TextDelta {
                text: "response".to_string(),
            })
            .unwrap();

        let update3 = plan_rx.recv().await.unwrap();
        let update4 = plan_rx.recv().await.unwrap();
        assert!(
            matches!(update3, PlanStreamUpdate::Text(ref t) if t == "Fresh "),
            "After reconnect, accumulated text should start fresh"
        );
        assert!(
            matches!(update4, PlanStreamUpdate::Text(ref t) if t == "Fresh response"),
            "After reconnect, accumulated text should not contain pre-reconnect data"
        );
    }
}
