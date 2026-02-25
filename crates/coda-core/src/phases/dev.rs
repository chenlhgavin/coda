//! Development phase executor.
//!
//! Handles dynamic dev phases from the design spec. Each dev phase renders
//! the `run/dev_phase` prompt template and sends it to the agent, then
//! validates that meaningful work was produced.

use tracing::error;

use crate::CoreError;
use crate::state::PhaseKind;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator};

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

        // Only load full design spec for the first dev phase; subsequent
        // phases reference it from conversation history to reduce token usage.
        let design_spec = if phase_idx == 0 {
            ctx.load_spec("design.md")?
        } else {
            String::new()
        };
        let checks = &ctx.config.checks;
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
        let is_first = phase_idx == 0;

        // Build resume context if resuming mid-phase
        let resume_context = if was_running {
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
                checks => checks,
                feature_slug => feature_slug,
                resume_context => resume_context,
            ),
        )?;

        let resp = ctx.send_and_collect(&prompt, None).await?;
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
