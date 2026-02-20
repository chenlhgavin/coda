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
use tracing::{debug, info, warn};

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
    /// Accumulated API cost in USD across all planning turns.
    planning_cost_usd: f64,
    /// Accumulated conversation turns across the entire planning session.
    planning_turns: u32,
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
        let coda_md_path = project_root.join(".coda.md");
        let coda_md = match std::fs::read_to_string(&coda_md_path) {
            Ok(content) => content,
            Err(_) => {
                warn!(
                    path = %coda_md_path.display(),
                    "Missing .coda.md — planning agent will lack repository context. \
                     Run `coda init` to generate it."
                );
                String::new()
            }
        };

        let system_prompt = pm.render("plan/system", minijinja::context!(coda_md => coda_md))?;

        let options = AgentProfile::Planner.to_options(
            &system_prompt,
            project_root.clone(),
            config.agent.max_turns,
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
            planning_cost_usd: 0.0,
            planning_turns: 0,
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

    /// Disconnects the underlying `ClaudeClient`.
    ///
    /// Safe to call multiple times or when not connected.
    pub async fn disconnect(&mut self) {
        if self.connected {
            let _ = self.client.disconnect().await;
            self.connected = false;
            debug!("PlanSession disconnected from Claude");
        }
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
                    Message::Result(result_msg) => {
                        if let Some(cost) = result_msg.total_cost_usd {
                            self.planning_cost_usd += cost;
                        }
                        self.planning_turns += result_msg.num_turns;
                        break;
                    }
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
            self.git.detect_default_branch()
        } else {
            self.config.git.base_branch.clone()
        };

        // Pre-flight: verify CODA config is committed to the base branch.
        // Without this, the new worktree would be missing .coda/config.yml,
        // .coda.md, and other init artifacts.
        if !self
            .git
            .file_exists_in_ref(&base_branch, ".coda/config.yml")
        {
            return Err(CoreError::PlanError(format!(
                "CODA init files are not committed to '{base_branch}'. \
                 The worktree would be missing project configuration.\n  \
                 Please commit them first:\n    \
                 git add .coda/ .coda.md CLAUDE.md .gitignore && \
                 git commit -m \"chore: initialize CODA project\"\n  \
                 Or re-run `coda init` (it auto-commits by default)."
            )));
        }

        let branch_name = format!("{}/{}", self.config.git.branch_prefix, slug);
        if let Some(parent) = worktree_abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(CoreError::IoError)?;
        }
        self.git
            .worktree_add(&worktree_abs, &branch_name, &base_branch)?;
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
        self.git.add(&worktree_abs, &[".coda/"])?;
        if self.git.has_staged_changes(&worktree_abs) {
            self.git.commit(
                &worktree_abs,
                &format!("feat({slug}): initialize planning artifacts"),
            )?;
            info!("Committed initial planning artifacts");
        }

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
            .field("connected", &self.connected)
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
/// review + verify quality phases, and initialises everything to `Pending`
/// with zeroed cost/duration statistics. Planning session costs are
/// recorded in `total` so they appear in status reports.
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

        assert_eq!(state.phases[0].kind, PhaseKind::Dev);
        assert_eq!(state.phases[3].kind, PhaseKind::Quality);
        assert_eq!(state.phases[4].kind, PhaseKind::Quality);

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

        let deserialized: FeatureState = serde_yaml::from_str(&yaml).unwrap();
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
}
