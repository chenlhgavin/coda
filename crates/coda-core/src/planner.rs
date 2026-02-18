//! Planning session for interactive feature planning.
//!
//! Provides `PlanSession` which wraps a `ClaudeClient` with the Planner
//! profile for multi-turn feature planning conversations, and `PlanOutput`
//! describing the artifacts produced by a successful planning session.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_agent_sdk_rs::{ClaudeClient, ContentBlock, Message};
use coda_pm::PromptManager;
use futures::StreamExt;
use tracing::{debug, info};

use crate::CoreError;
use crate::config::CodaConfig;
use crate::git::GitOps;
use crate::profile::AgentProfile;
use crate::state::{
    FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseKind, PhaseRecord, PhaseStatus,
    TokenCost, TotalStats,
};

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
const QUALITY_PHASES: &[&str] = &["review", "verify"];

/// An interactive planning session wrapping a `ClaudeClient` with the
/// Planner profile for multi-turn feature planning conversations.
///
/// # Usage
///
/// 1. Create via [`PlanSession::new`]
/// 2. Call [`send`](Self::send) repeatedly for conversation turns
/// 3. Call [`approve`](Self::approve) to formalize the approved design
/// 4. Call [`finalize`](Self::finalize) to write specs and create a worktree
pub struct PlanSession {
    client: ClaudeClient,
    feature_slug: String,
    project_root: PathBuf,
    pm: PromptManager,
    config: CodaConfig,
    connected: bool,
    /// The formalized design spec produced by [`approve`](Self::approve).
    approved_design: Option<String>,
    /// The verification plan produced by [`approve`](Self::approve).
    approved_verification: Option<String>,
    /// Git operations implementation.
    git: Arc<dyn GitOps>,
}

impl PlanSession {
    /// Creates a new planning session for the given feature.
    ///
    /// Initializes a `ClaudeClient` with the Planner profile and renders
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
        // Load .coda.md for repository context
        let coda_md_path = project_root.join(".coda.md");
        let coda_md = std::fs::read_to_string(&coda_md_path).unwrap_or_default();

        let system_prompt = pm.render("plan/system", minijinja::context!(coda_md => coda_md))?;

        let options = AgentProfile::Planner.to_options(
            &system_prompt,
            project_root.clone(),
            50, // max turns for planning conversation
            config.agent.max_budget_usd,
            &config.agent.model,
        );

        let client = ClaudeClient::new(options);

        Ok(Self {
            client,
            feature_slug,
            project_root,
            pm: pm.clone(),
            config: config.clone(),
            connected: false,
            approved_design: None,
            approved_verification: None,
            git,
        })
    }

    /// Connects the underlying `ClaudeClient` to the Claude process.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentError` if the connection fails.
    pub async fn connect(&mut self) -> Result<(), CoreError> {
        self.client
            .connect()
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;
        self.connected = true;
        debug!("PlanSession connected to Claude");
        Ok(())
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
        if !self.connected {
            self.connect().await?;
        }

        self.client
            .query(message)
            .await
            .map_err(|e| CoreError::AgentError(e.to_string()))?;

        let mut response = String::new();
        {
            let mut stream = self.client.receive_response();
            while let Some(result) = stream.next().await {
                let msg = result.map_err(|e| CoreError::AgentError(e.to_string()))?;
                match msg {
                    Message::Assistant(assistant) => {
                        for block in &assistant.message.content {
                            if let ContentBlock::Text(text) = block {
                                response.push_str(&text.text);
                            }
                        }
                    }
                    Message::Result(_) => break,
                    _ => {}
                }
            }
        }

        Ok(response)
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
        self.approved_design = Some(design.clone());
        info!("Design approved and formalized");

        // Step 2: Generate verification plan
        let checks_str = self.config.checks.join("\n");
        let verification_prompt = self.pm.render(
            "plan/verification",
            minijinja::context!(
                design_spec => &design,
                checks => &checks_str,
                feature_slug => &self.feature_slug,
            ),
        )?;

        let verification = self.send(&verification_prompt).await?;
        self.approved_verification = Some(verification.clone());
        info!("Verification plan generated");

        Ok((design, verification))
    }

    /// Returns `true` if the design has been approved via [`approve`](Self::approve).
    pub fn is_approved(&self) -> bool {
        self.approved_design.is_some()
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
    /// # Errors
    ///
    /// Returns `CoreError` if git operations, directory creation, or
    /// file writes fail.
    pub async fn finalize(&mut self) -> Result<PlanOutput, CoreError> {
        let slug = self.feature_slug.clone();
        let worktree_path = self.project_root.join(".trees").join(&slug);

        // Guard: design and verification must be approved before finalizing
        let design_content = self.approved_design.take().ok_or_else(|| {
            CoreError::PlanError(
                "Cannot finalize: design has not been approved. Use /approve first.".to_string(),
            )
        })?;
        let verification_content = self.approved_verification.take().ok_or_else(|| {
            CoreError::PlanError("Cannot finalize: verification plan is missing.".to_string())
        })?;

        // 1. Create git worktree first
        let base_branch = if self.config.git.base_branch == "auto" {
            self.git.detect_default_branch()
        } else {
            self.config.git.base_branch.clone()
        };
        let branch_name = format!("{}/{}", self.config.git.branch_prefix, slug);
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::IoError)?;
        }
        self.git
            .worktree_add(&worktree_path, &branch_name, &base_branch)?;
        info!(
            branch = %branch_name,
            worktree = %worktree_path.display(),
            "Created git worktree"
        );

        // 2. Write artifacts into worktree/.coda/<slug>/
        let coda_feature_dir = worktree_path.join(".coda").join(&slug);
        let specs_dir = coda_feature_dir.join("specs");
        std::fs::create_dir_all(&specs_dir).map_err(CoreError::IoError)?;

        // Write design spec
        let design_spec_path = specs_dir.join("design.md");
        std::fs::write(&design_spec_path, &design_content).map_err(CoreError::IoError)?;
        debug!(path = %design_spec_path.display(), "Wrote design spec");

        // Write verification plan
        let verification_path = specs_dir.join("verification.md");
        std::fs::write(&verification_path, &verification_content).map_err(CoreError::IoError)?;
        debug!(path = %verification_path.display(), "Wrote verification plan");

        // 3. Write initial state.yml
        let dev_phases = extract_dev_phases(&design_content);
        let state = build_initial_state(
            &self.feature_slug,
            &worktree_path,
            &branch_name,
            &base_branch,
            &dev_phases,
        );
        let state_path = coda_feature_dir.join("state.yml");
        let state_yaml = serde_yaml::to_string(&state)?;
        std::fs::write(&state_path, state_yaml).map_err(CoreError::IoError)?;
        debug!(path = %state_path.display(), "Wrote state.yml");

        // Disconnect client
        if self.connected {
            let _ = self.client.disconnect().await;
            self.connected = false;
        }

        info!("Planning session finalized successfully");

        Ok(PlanOutput {
            design_spec: design_spec_path,
            verification: verification_path,
            state: state_path,
            worktree: worktree_path,
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
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

/// Extracts development phase names from a design specification.
///
/// Parses headings like `### Phase 1: <name>` and converts each name
/// into a slug (lowercase, spaces replaced with hyphens, non-alphanumeric
/// characters removed). Falls back to a single `"default"` phase if
/// no phase headings are found.
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
        // Match "### Phase N: <name>" pattern
        if let Some(rest) = trimmed.strip_prefix("### Phase ") {
            // Skip the number and colon: "1: <name>" → "<name>"
            if let Some((_num, name)) = rest.split_once(':') {
                let name = name.trim();
                if !name.is_empty() {
                    let slug = slugify_phase_name(name);
                    if !slug.is_empty() {
                        phases.push(slug);
                    }
                }
            }
        }
    }

    if phases.is_empty() {
        phases.push("default".to_string());
    }

    phases
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
/// review + verify quality phases, and initialises everything to `Pending`
/// with zeroed cost/duration statistics.
pub(crate) fn build_initial_state(
    feature_slug: &str,
    worktree_path: &Path,
    branch: &str,
    base_branch: &str,
    dev_phase_names: &[String],
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
        total: TotalStats::default(),
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
        );

        // Feature info
        assert_eq!(state.feature.slug, "add-auth");
        assert!(state.feature.created_at <= chrono::Utc::now());
        assert!(state.feature.updated_at <= chrono::Utc::now());

        // Overall status
        assert_eq!(state.status, FeatureStatus::Planned);
        assert_eq!(state.current_phase, 0);

        // Git info
        assert_eq!(state.git.worktree_path, PathBuf::from(".trees/add-auth"));
        assert_eq!(state.git.branch, "feature/add-auth");
        assert_eq!(state.git.base_branch, "main");

        // Phases: 3 dev + 2 quality = 5
        assert_eq!(state.phases.len(), 5);
        let names: Vec<&str> = state.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "type-definitions",
                "transport-layer",
                "client-methods",
                "review",
                "verify"
            ]
        );

        // Check kinds
        assert_eq!(state.phases[0].kind, PhaseKind::Dev);
        assert_eq!(state.phases[1].kind, PhaseKind::Dev);
        assert_eq!(state.phases[2].kind, PhaseKind::Dev);
        assert_eq!(state.phases[3].kind, PhaseKind::Quality);
        assert_eq!(state.phases[4].kind, PhaseKind::Quality);

        for phase in &state.phases {
            assert_eq!(phase.status, PhaseStatus::Pending);
            assert!(phase.started_at.is_none());
            assert!(phase.completed_at.is_none());
            assert_eq!(phase.turns, 0);
            assert!((phase.cost_usd - 0.0).abs() < f64::EPSILON);
        }

        assert!(state.pr.is_none());
        assert_eq!(state.total.turns, 0);
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
        );

        let yaml = serde_yaml::to_string(&state).unwrap();
        assert!(yaml.contains("planned"));
        assert!(yaml.contains("new-feature"));
        assert!(yaml.contains("phase-one"));
        assert!(yaml.contains("review"));
        assert!(yaml.contains("verify"));

        // Round-trip
        let deserialized: FeatureState = serde_yaml::from_str(&yaml).unwrap();
        // 2 dev + 2 quality = 4
        assert_eq!(deserialized.phases.len(), 4);
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
        assert_eq!(phases[0], "type-definitions-day-1");
        assert_eq!(phases[1], "transport-layer-day-1-2");
        assert_eq!(phases[2], "testing-documentation-day-3");
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
        assert_eq!(
            slugify_phase_name("Transport Layer (Day 1-2)"),
            "transport-layer-day-1-2"
        );
        assert_eq!(slugify_phase_name("  spaced  "), "spaced");
    }
}
