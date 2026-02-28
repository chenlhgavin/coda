//! Development phase executor.
//!
//! Handles dynamic dev phases from the design spec. Each dev phase renders
//! the `run/dev_phase` prompt template and sends it to the agent, then
//! validates that meaningful work was produced.

use tracing::error;

use crate::CoreError;
use crate::planner::extract_structured_phases;
use crate::state::PhaseKind;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator};

/// Per-phase goal and expected file changes extracted from the design spec.
#[derive(Debug, Default)]
struct PhaseSection {
    goal: String,
    files: Vec<String>,
}

/// Extracts the goal and expected file changes for a specific dev phase.
///
/// Prefers the structured `yaml-phases` block (matching by 0-based index).
/// Falls back to parsing the `### Phase N:` heading section for `**Goal**:`
/// and `**Expected file changes**:` fields.
fn extract_phase_section(
    design_content: &str,
    _phase_name: &str,
    phase_number: usize,
) -> PhaseSection {
    // Try structured extraction first (phase_number is 1-based)
    if let Some(phases) = extract_structured_phases(design_content) {
        let idx = phase_number.saturating_sub(1);
        if let Some(phase) = phases.get(idx) {
            return PhaseSection {
                goal: phase.goal.clone(),
                files: phase.files.clone(),
            };
        }
    }

    // Fall back to heading-based extraction
    let heading_prefix = format!("Phase {phase_number}:");
    let mut in_section = false;
    let mut goal = String::new();
    let mut files = Vec::new();

    for line in design_content.lines() {
        let trimmed = line.trim();

        // Detect start of our phase heading
        if !in_section {
            let is_phase_heading = (trimmed.starts_with("## ")
                || trimmed.starts_with("### ")
                || trimmed.starts_with("#### "))
                && trimmed.contains(&heading_prefix);
            if is_phase_heading {
                in_section = true;
            }
            continue;
        }

        // End of section at next heading of same or higher level
        if (trimmed.starts_with("## ") || trimmed.starts_with("### "))
            && !trimmed.contains(&heading_prefix)
        {
            break;
        }

        // Extract goal
        if let Some(rest) = trimmed.strip_prefix("- **Goal**:") {
            goal = rest.trim().to_string();
        }

        // Extract expected file changes
        if let Some(rest) = trimmed.strip_prefix("- **Expected file changes**:") {
            files = rest
                .split(',')
                .map(|s| s.trim().trim_matches('`').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    PhaseSection { goal, files }
}

/// Executes a single development phase from the design spec.
///
/// Renders the `run/dev_phase` prompt template with the phase name,
/// index, and design spec, then sends it to the agent. Validates that
/// the agent produced meaningful work (non-zero cost and tool usage).
///
/// # Example
///
/// ```no_run
/// use coda_core::phases::dev::DevPhaseExecutor;
///
/// let executor = DevPhaseExecutor;
/// // executor.execute(&mut ctx, phase_idx).await?;
/// ```
pub struct DevPhaseExecutor;

impl PhaseExecutor for DevPhaseExecutor {
    async fn execute(
        &mut self,
        ctx: &mut PhaseContext,
        phase_idx: usize,
    ) -> Result<TaskResult, CoreError> {
        let was_running =
            ctx.state().phases[phase_idx].status == crate::state::PhaseStatus::Running;
        ctx.state_manager.mark_phase_running(phase_idx)?;

        let mut acc = PhaseMetricsAccumulator::new();

        // Always load the full spec for phase section extraction.
        let full_spec = ctx.load_spec("design.md")?;

        // When isolate_dev_phases is enabled and this is not the first
        // phase, use an independent session ID so conversation history
        // does not accumulate across phases. The design spec and resume
        // context are injected explicitly.
        let use_isolated_session = ctx.config.agent.isolate_dev_phases && phase_idx > 0;
        let session_id = if use_isolated_session {
            Some(format!("dev-phase-{phase_idx}"))
        } else {
            None
        };

        // Inject full design spec for the first phase, or when using
        // isolated sessions (since each session starts fresh).
        let design_spec = if phase_idx == 0 || use_isolated_session {
            full_spec.clone()
        } else {
            String::new()
        };
        let feature_slug = ctx.state().feature.slug.clone();
        let phase_name = ctx.state().phases[phase_idx].name.clone();

        // Determine the 1-based phase number among dev phases
        let dev_phase_number = ctx
            .state()
            .phases
            .iter()
            .take(phase_idx + 1)
            .filter(|p| p.kind == PhaseKind::Dev)
            .count();
        let total_dev_phases = ctx
            .state()
            .phases
            .iter()
            .filter(|p| p.kind == PhaseKind::Dev)
            .count();
        let is_first = phase_idx == 0 || use_isolated_session;

        // Extract per-phase goal and expected file changes
        let section = extract_phase_section(&full_spec, &phase_name, dev_phase_number);

        // Build resume context if resuming mid-phase or isolated session
        let resume_context = if was_running || use_isolated_session {
            ctx.build_resume_context()?
        } else {
            String::new()
        };

        let prompt = ctx.pm.render(
            "run/dev_phase",
            minijinja::context!(
                design_spec => design_spec,
                phase_name => phase_name,
                phase_number => dev_phase_number,
                total_dev_phases => total_dev_phases,
                is_first => is_first,
                feature_slug => feature_slug,
                resume_context => resume_context,
                phase_goal => section.goal,
                phase_files => section.files,
            ),
        )?;

        let resp = ctx.send_and_collect(&prompt, session_id.as_deref()).await?;
        let incremental = ctx.metrics.record(&resp.result);
        if let Some(logger) = &mut ctx.run_logger {
            logger.log_interaction(&prompt, &resp, &incremental);
        }
        acc.record(&resp, incremental);

        // Validate that the agent did meaningful work
        if incremental.cost_usd == 0.0
            && incremental.input_tokens == 0
            && incremental.output_tokens == 0
            && resp.tool_output.is_empty()
        {
            let preview = resp.text.chars().take(200).collect::<String>();
            error!(
                phase = %phase_name,
                text_preview = %preview,
                "Dev phase produced no meaningful work (zero cost, no tool usage)",
            );
            return Err(CoreError::AgentError(format!(
                "Dev phase '{phase_name}' produced no meaningful work \
                 (zero cost, no tool usage). The agent session may be \
                 non-functional. Response: {preview}",
            )));
        }

        let outcome = acc.into_outcome(serde_json::json!({}));
        let task_result = TaskResult {
            task: Task::DevPhase {
                name: phase_name,
                feature_slug: ctx.state().feature.slug.clone(),
            },
            status: TaskStatus::Completed,
            turns: outcome.turns,
            cost_usd: outcome.cost_usd,
            duration: outcome.duration,
            artifacts: vec![],
        };
        ctx.state_manager.complete_phase(phase_idx, &outcome)?;

        Ok(task_result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_extract_phase_section_from_yaml_phases() {
        let spec = r#"
## Development Phases

### Phase 1: Config Extension
- **Goal**: Add new config fields
- **Expected file changes**: `src/config.rs`, `src/lib.rs`

```yaml-phases
phases:
  - name: "Config Extension"
    goal: "Add isolate_dev_phases and dev_phase_checks fields"
    files: ["src/config.rs", "src/lib.rs"]
  - name: "Parsing"
    goal: "Add structured phase extraction"
    files: ["src/planner.rs"]
```
"#;
        let section = extract_phase_section(spec, "config-extension", 1);
        assert_eq!(
            section.goal,
            "Add isolate_dev_phases and dev_phase_checks fields"
        );
        assert_eq!(section.files, vec!["src/config.rs", "src/lib.rs"]);

        let section2 = extract_phase_section(spec, "parsing", 2);
        assert_eq!(section2.goal, "Add structured phase extraction");
        assert_eq!(section2.files, vec!["src/planner.rs"]);
    }

    #[test]
    fn test_should_fallback_to_heading_extraction() {
        let spec = r#"
## Development Phases

### Phase 1: Config Extension
- **Goal**: Add new config fields
- **Expected file changes**: `src/config.rs`, `src/lib.rs`
- **Tasks**:
  - Add field A
  - Add field B

### Phase 2: Parsing
- **Goal**: Add structured parsing
- **Expected file changes**: `src/planner.rs`
"#;
        let section = extract_phase_section(spec, "config-extension", 1);
        assert_eq!(section.goal, "Add new config fields");
        assert_eq!(section.files, vec!["src/config.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_should_return_empty_section_for_missing_phase() {
        let spec = "## Some other content\n\nNo phases here.";
        let section = extract_phase_section(spec, "nonexistent", 1);
        assert!(section.goal.is_empty());
        assert!(section.files.is_empty());
    }

    #[test]
    fn test_should_handle_out_of_range_yaml_phase_index() {
        let spec = r#"
```yaml-phases
phases:
  - name: "Only Phase"
    goal: "Single phase"
    files: ["src/main.rs"]
```
"#;
        // Phase 2 is out of range, should fall back to heading extraction
        // (which also finds nothing), returning empty
        let section = extract_phase_section(spec, "nonexistent", 5);
        assert!(section.goal.is_empty());
    }
}
