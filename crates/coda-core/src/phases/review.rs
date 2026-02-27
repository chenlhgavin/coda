//! Review phase executor.
//!
//! Handles the code review phase with two review engines:
//! Claude (self-review) and Codex (independent review).
//! Each engine mode follows the same pattern: review → find issues → fix → repeat.

use tracing::{info, warn};

use crate::CoreError;
use crate::codex::{format_issues, is_codex_available, run_codex_review};
use crate::config::AgentBackend;
use crate::parser::parse_review_issues;
use crate::runner::RunEvent;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator, skip_disabled_phase};

/// Executes the review phase with the configured review engine.
///
/// Dispatches to Claude or Codex mode based on configuration.
/// Falls back to Claude if Codex is configured but not available.
///
/// # Example
///
/// ```no_run
/// use coda_core::phases::review::ReviewPhaseExecutor;
///
/// let executor = ReviewPhaseExecutor;
/// // executor.execute(&mut ctx, phase_idx).await?;
/// ```
pub struct ReviewPhaseExecutor;

impl PhaseExecutor for ReviewPhaseExecutor {
    async fn execute(
        &mut self,
        ctx: &mut PhaseContext,
        phase_idx: usize,
    ) -> Result<TaskResult, CoreError> {
        ctx.state_manager.mark_phase_running(phase_idx)?;

        if !ctx.config.review.enabled {
            info!("Code review disabled, skipping");
            let feature_slug = ctx.state().feature.slug.clone();
            return skip_disabled_phase(
                &mut ctx.state_manager,
                phase_idx,
                Task::Review { feature_slug },
            );
        }

        let resolved = ctx.config.resolve_review();
        info!(backend = %resolved.backend, "Starting review phase");

        match resolved.backend {
            AgentBackend::Claude => run_review_claude(ctx, phase_idx).await,
            AgentBackend::Codex => {
                if is_codex_available() {
                    run_review_codex(ctx, phase_idx, &resolved).await
                } else {
                    warn!("Codex CLI not found, falling back to Claude review");
                    run_review_claude(ctx, phase_idx).await
                }
            }
        }
    }
}

/// Runs review using only Claude (self-review).
async fn run_review_claude(
    ctx: &mut PhaseContext,
    phase_idx: usize,
) -> Result<TaskResult, CoreError> {
    let design_spec = ctx.load_spec("design.md")?;
    let max_rounds = ctx.config.review.max_review_rounds;
    let session_id = if ctx.config.agent.isolate_quality_phases {
        Some("review")
    } else {
        None
    };
    let mut acc = PhaseMetricsAccumulator::new();

    for round in 0..max_rounds {
        info!(round = round + 1, max = max_rounds, "Review round");

        let diff = ctx.get_diff().await?;
        let review_prompt = ctx.pm.render(
            "run/review",
            minijinja::context!(
                design_spec => design_spec,
                diff => diff,
            ),
        )?;

        let resp = ctx.send_and_collect(&review_prompt, session_id).await?;
        let m = ctx.metrics.record(&resp.result);
        if let Some(logger) = &mut ctx.run_logger {
            logger.log_interaction(&review_prompt, &resp, &m);
        }
        acc.record(&resp, m);

        ctx.review_summary.rounds += 1;

        let issues = parse_review_issues(&resp.text);
        let issue_count = issues.len() as u32;
        ctx.review_summary.issues_found += issue_count;

        ctx.emit_event(RunEvent::ReviewerCompleted {
            reviewer: "claude".to_string(),
            issues_found: issue_count,
        });
        ctx.emit_event(RunEvent::ReviewRound {
            round: round + 1,
            max_rounds,
            issues_found: issue_count,
        });

        if issues.is_empty() {
            info!("No critical/major issues found, review passed");
            break;
        }

        info!(issues = issue_count, "Found issues, asking agent to fix");
        ask_claude_to_fix(ctx, &issues, issue_count, session_id, &mut acc).await?;
        ctx.review_summary.issues_fix_attempted += issue_count;
    }

    finalize_review_phase(ctx, phase_idx, acc)
}

/// Runs review using only Codex, then asks Claude to fix.
async fn run_review_codex(
    ctx: &mut PhaseContext,
    phase_idx: usize,
    resolved: &crate::config::ResolvedAgentConfig,
) -> Result<TaskResult, CoreError> {
    let max_rounds = ctx.config.review.max_review_rounds;
    let codex_model = resolved.model.clone();
    let codex_effort = resolved.effort;
    let spec_path = ctx.spec_relative_path("design.md");
    let session_id = if ctx.config.agent.isolate_quality_phases {
        Some("review")
    } else {
        None
    };
    let mut acc = PhaseMetricsAccumulator::new();

    for round in 0..max_rounds {
        info!(round = round + 1, max = max_rounds, "Review round (codex)");

        let changed_files = ctx.get_changed_files().await?;
        let base_branch = ctx.state().git.base_branch.clone();
        let codex_issues = run_codex_review(
            &ctx.worktree_path,
            &base_branch,
            &spec_path,
            &changed_files,
            &codex_model,
            codex_effort,
            ctx.progress_tx.as_ref(),
        )
        .await?;
        let issue_count = codex_issues.len() as u32;

        ctx.emit_event(RunEvent::ReviewerCompleted {
            reviewer: "codex".to_string(),
            issues_found: issue_count,
        });

        ctx.review_summary.rounds += 1;
        ctx.review_summary.issues_found += issue_count;

        ctx.emit_event(RunEvent::ReviewRound {
            round: round + 1,
            max_rounds,
            issues_found: issue_count,
        });

        if codex_issues.is_empty() {
            info!("No critical/major issues found, review passed");
            break;
        }

        info!(
            issues = issue_count,
            "Codex found issues, asking Claude to fix"
        );
        let formatted = format_issues(&codex_issues);
        ask_claude_to_fix(ctx, &formatted, issue_count, session_id, &mut acc).await?;
        ctx.review_summary.issues_fix_attempted += issue_count;
    }

    finalize_review_phase(ctx, phase_idx, acc)
}

/// Asks Claude to fix a list of review issues.
async fn ask_claude_to_fix(
    ctx: &mut PhaseContext,
    issues: &[String],
    issue_count: u32,
    session_id: Option<&str>,
    acc: &mut PhaseMetricsAccumulator,
) -> Result<(), CoreError> {
    let issues_list = issues
        .iter()
        .enumerate()
        .map(|(i, issue)| format!("{}. {}", i + 1, issue))
        .collect::<Vec<_>>()
        .join("\n");
    let fix_prompt = format!(
        "The code review found {issue_count} critical/major issues that must be fixed.\n\n\
         ## Issues\n\n{issues_list}\n\n\
         ## Instructions\n\n\
         1. Fix each issue listed above\n\
         2. Run the configured checks to ensure nothing is broken\n\
         3. Commit the fixes with a descriptive message\n\n\
         Refer to the design specification provided earlier for the intended behavior.",
    );

    let fix_resp = ctx.send_and_collect(&fix_prompt, session_id).await?;
    let fm = ctx.metrics.record(&fix_resp.result);
    if let Some(logger) = &mut ctx.run_logger {
        logger.log_interaction(&fix_prompt, &fix_resp, &fm);
    }
    acc.record(&fix_resp, fm);
    Ok(())
}

/// Finalizes the review phase with accumulated metrics.
fn finalize_review_phase(
    ctx: &mut PhaseContext,
    phase_idx: usize,
    acc: PhaseMetricsAccumulator,
) -> Result<TaskResult, CoreError> {
    let outcome = acc.into_outcome(serde_json::json!({
        "rounds": ctx.review_summary.rounds,
        "issues_found": ctx.review_summary.issues_found,
        "issues_fix_attempted": ctx.review_summary.issues_fix_attempted,
    }));
    let task_result = TaskResult {
        task: Task::Review {
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

#[cfg(test)]
mod tests {
    use crate::config::{AgentBackend, CodaConfig, ReviewEngine};

    #[test]
    fn test_should_resolve_review_claude_backend() {
        let mut config = CodaConfig::default();
        config.review.engine = ReviewEngine::Claude;
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Claude);
    }

    #[test]
    fn test_should_resolve_review_codex_backend_from_legacy() {
        let config = CodaConfig::default(); // default engine = Codex
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Codex);
        assert_eq!(resolved.model, "gpt-5.3-codex");
    }

    #[test]
    fn test_should_resolve_review_explicit_agents_override() {
        let mut config = CodaConfig::default();
        config.agents.review.backend = Some(AgentBackend::Claude);
        config.agents.review.model = Some("claude-sonnet-4-6".to_string());
        let resolved = config.resolve_review();
        assert_eq!(resolved.backend, AgentBackend::Claude);
        assert_eq!(resolved.model, "claude-sonnet-4-6");
    }
}
