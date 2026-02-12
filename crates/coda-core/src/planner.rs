//! Planning session for interactive feature planning.
//!
//! Provides `PlanSession` which wraps a `ClaudeClient` with the Planner
//! profile for multi-turn feature planning conversations, and `PlanOutput`
//! describing the artifacts produced by a successful planning session.

use std::path::{Path, PathBuf};

use claude_agent_sdk_rs::{ClaudeClient, ContentBlock, Message};
use coda_pm::PromptManager;
use futures::StreamExt;
use tracing::{debug, info};

use crate::CoreError;
use crate::config::CodaConfig;
use crate::profile::AgentProfile;
use crate::state::{
    FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseRecord, PhaseStatus, TokenCost,
    TotalStats,
};

/// Output produced by finalizing a planning session.
#[derive(Debug)]
pub struct PlanOutput {
    /// Path to the generated design spec (`.coda/<feature>/specs/design.md`).
    pub design_spec: PathBuf,

    /// Path to the generated verification plan (`.coda/<feature>/specs/verification.md`).
    pub verification: PathBuf,

    /// Path to the feature state file (`.coda/<feature>/state.yml`).
    pub state: PathBuf,

    /// Path to the git worktree (`.trees/<feature>/`).
    pub worktree: PathBuf,
}

/// Phase names used in feature execution state.
const PHASE_NAMES: &[&str] = &["setup", "implement", "test", "review", "verify"];

/// An interactive planning session wrapping a `ClaudeClient` with the
/// Planner profile for multi-turn feature planning conversations.
///
/// # Usage
///
/// 1. Create via [`PlanSession::new`]
/// 2. Call [`send`](Self::send) repeatedly for conversation turns
/// 3. Call [`finalize`](Self::finalize) to generate specs and create a worktree
pub struct PlanSession {
    client: ClaudeClient,
    feature_slug: String,
    feature_id: String,
    project_root: PathBuf,
    pm: PromptManager,
    config: CodaConfig,
    connected: bool,
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
    ) -> Result<Self, CoreError> {
        let feature_id = format!("{:04}", config.next_feature_id);

        // Load .coda.md for repository context
        let coda_md_path = project_root.join(".coda.md");
        let coda_md = std::fs::read_to_string(&coda_md_path).unwrap_or_default();

        let system_prompt = pm.render("plan/system", minijinja::context!(coda_md => coda_md))?;

        let options = AgentProfile::Planner.to_options(
            &system_prompt,
            project_root.clone(),
            50, // max turns for planning conversation
            config.agent.max_budget_usd,
        );

        let client = ClaudeClient::new(options);

        Ok(Self {
            client,
            feature_slug,
            feature_id,
            project_root,
            pm: pm.clone(),
            config: config.clone(),
            connected: false,
        })
    }

    /// Connects the underlying `ClaudeClient` to the Claude process.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::AgentSdkError` if the connection fails.
    pub async fn connect(&mut self) -> Result<(), CoreError> {
        self.client
            .connect()
            .await
            .map_err(|e| CoreError::AgentSdkError(e.to_string()))?;
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
    /// Returns `CoreError::AgentSdkError` if the query or response
    /// streaming fails.
    pub async fn send(&mut self, message: &str) -> Result<String, CoreError> {
        if !self.connected {
            self.connect().await?;
        }

        self.client
            .query(message)
            .await
            .map_err(|e| CoreError::AgentSdkError(e.to_string()))?;

        let mut response = String::new();
        {
            let mut stream = self.client.receive_response();
            while let Some(result) = stream.next().await {
                let msg = result.map_err(|e| CoreError::AgentSdkError(e.to_string()))?;
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

    /// Finalizes the planning session by generating specs and creating a worktree.
    ///
    /// This method:
    /// 1. Creates the `.coda/<feature>/specs/` directory
    /// 2. Asks the agent to generate a design spec
    /// 3. Asks the agent to generate a verification plan
    /// 4. Creates a git worktree from the main branch
    /// 5. Writes the initial `state.yml`
    ///
    /// # Errors
    ///
    /// Returns `CoreError` if directory creation, agent queries, git
    /// operations, or file writes fail.
    pub async fn finalize(&mut self) -> Result<PlanOutput, CoreError> {
        let feature_dir_name = self.feature_dir_name();
        let coda_feature_dir = self.project_root.join(".coda").join(&feature_dir_name);
        let specs_dir = coda_feature_dir.join("specs");
        let worktree_path = self.project_root.join(".trees").join(&feature_dir_name);

        // Create directories
        std::fs::create_dir_all(&specs_dir).map_err(CoreError::IoError)?;

        // Generate design spec via agent
        info!("Generating design specification...");
        let design_prompt = self.pm.render(
            "plan/design_spec",
            minijinja::context!(
                feature_slug => &self.feature_slug,
                feature_id => &self.feature_id,
                conversation_history => "Based on our conversation above",
            ),
        )?;
        let design_content = self.send(&design_prompt).await?;

        let design_spec_path = specs_dir.join("design.md");
        std::fs::write(&design_spec_path, &design_content).map_err(CoreError::IoError)?;
        debug!(path = %design_spec_path.display(), "Wrote design spec");

        // Generate verification plan via agent
        info!("Generating verification plan...");
        let checks_str = self.config.checks.join("\n");
        let verification_prompt = self.pm.render(
            "plan/verification",
            minijinja::context!(
                design_spec => &design_content,
                checks => &checks_str,
                feature_slug => &self.feature_slug,
            ),
        )?;
        let verification_content = self.send(&verification_prompt).await?;

        let verification_path = specs_dir.join("verification.md");
        std::fs::write(&verification_path, &verification_content).map_err(CoreError::IoError)?;
        debug!(path = %verification_path.display(), "Wrote verification plan");

        // Create git worktree
        let branch_name = format!("{}/{}", self.config.git.branch_prefix, feature_dir_name);
        create_worktree(&self.project_root, &worktree_path, &branch_name)?;
        info!(
            branch = %branch_name,
            worktree = %worktree_path.display(),
            "Created git worktree"
        );

        // Write initial state.yml
        let state = build_initial_state(
            &self.feature_id,
            &self.feature_slug,
            &worktree_path,
            &branch_name,
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

    /// Returns the combined directory name `<id>-<slug>`.
    pub fn feature_dir_name(&self) -> String {
        format!("{}-{}", self.feature_id, self.feature_slug)
    }

    /// Returns the feature slug.
    pub fn feature_slug(&self) -> &str {
        &self.feature_slug
    }

    /// Returns the feature ID.
    pub fn feature_id(&self) -> &str {
        &self.feature_id
    }
}

impl std::fmt::Debug for PlanSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanSession")
            .field("feature_slug", &self.feature_slug)
            .field("feature_id", &self.feature_id)
            .field("project_root", &self.project_root)
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

/// Creates a git worktree at `worktree_path` with a new branch from `main`.
fn create_worktree(
    project_root: &Path,
    worktree_path: &Path,
    branch: &str,
) -> Result<(), CoreError> {
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).map_err(CoreError::IoError)?;
    }

    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            &worktree_path.display().to_string(),
            "-b",
            branch,
            "main",
        ])
        .current_dir(project_root)
        .output()
        .map_err(CoreError::IoError)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoreError::GitError(format!(
            "Failed to create worktree: {stderr}"
        )));
    }

    Ok(())
}

/// Builds an initial `FeatureState` for a newly planned feature.
///
/// Creates a `FeatureState` with all phases set to `Pending`, a
/// `Planned` overall status, and zeroed cost/duration statistics.
pub(crate) fn build_initial_state(
    feature_id: &str,
    feature_slug: &str,
    worktree_path: &Path,
    branch: &str,
) -> FeatureState {
    let now = chrono::Utc::now();

    let phases = PHASE_NAMES
        .iter()
        .map(|name| PhaseRecord {
            name: (*name).to_string(),
            status: PhaseStatus::Pending,
            started_at: None,
            completed_at: None,
            turns: 0,
            cost_usd: 0.0,
            cost: TokenCost::default(),
            duration_secs: 0,
            details: serde_json::json!({}),
        })
        .collect();

    FeatureState {
        feature: FeatureInfo {
            id: feature_id.to_string(),
            slug: feature_slug.to_string(),
            created_at: now,
            updated_at: now,
        },
        status: FeatureStatus::Planned,
        current_phase: 0,
        git: GitInfo {
            worktree_path: worktree_path.to_path_buf(),
            branch: branch.to_string(),
            base_branch: "main".to_string(),
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
    fn test_should_build_initial_state_with_correct_structure() {
        let worktree = PathBuf::from(".trees/0001-add-auth");
        let state = build_initial_state("0001", "add-auth", &worktree, "feature/0001-add-auth");

        // Feature info
        assert_eq!(state.feature.id, "0001");
        assert_eq!(state.feature.slug, "add-auth");
        assert!(state.feature.created_at <= chrono::Utc::now());
        assert!(state.feature.updated_at <= chrono::Utc::now());

        // Overall status
        assert_eq!(state.status, FeatureStatus::Planned);
        assert_eq!(state.current_phase, 0);

        // Git info
        assert_eq!(
            state.git.worktree_path,
            PathBuf::from(".trees/0001-add-auth")
        );
        assert_eq!(state.git.branch, "feature/0001-add-auth");
        assert_eq!(state.git.base_branch, "main");

        // Phases: should have exactly 5, all pending
        assert_eq!(state.phases.len(), 5);
        let phase_names: Vec<&str> = state.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            phase_names,
            vec!["setup", "implement", "test", "review", "verify"]
        );

        for phase in &state.phases {
            assert_eq!(phase.status, PhaseStatus::Pending);
            assert!(phase.started_at.is_none());
            assert!(phase.completed_at.is_none());
            assert_eq!(phase.turns, 0);
            assert!((phase.cost_usd - 0.0).abs() < f64::EPSILON);
            assert_eq!(phase.cost.input_tokens, 0);
            assert_eq!(phase.cost.output_tokens, 0);
            assert_eq!(phase.duration_secs, 0);
        }

        // No PR yet
        assert!(state.pr.is_none());

        // Default totals
        assert_eq!(state.total.turns, 0);
        assert!((state.total.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(state.total.duration_secs, 0);
    }

    #[test]
    fn test_should_build_initial_state_serializable_to_yaml() {
        let worktree = PathBuf::from(".trees/0002-new-feature");
        let state =
            build_initial_state("0002", "new-feature", &worktree, "feature/0002-new-feature");

        let yaml = serde_yaml::to_string(&state).unwrap();
        assert!(yaml.contains("planned"));
        assert!(yaml.contains("0002"));
        assert!(yaml.contains("new-feature"));
        assert!(yaml.contains("setup"));
        assert!(yaml.contains("verify"));

        // Round-trip
        let deserialized: FeatureState = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.phases.len(), 5);
        assert_eq!(deserialized.status, FeatureStatus::Planned);
    }
}
