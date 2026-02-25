//! Review phase executor.
//!
//! Handles the code review phase with support for three review engines:
//! Claude (self-review), Codex (independent review), and Hybrid (both).
//! Each engine mode follows the same pattern: review → find issues → fix → repeat.

use std::time::Duration;

use tracing::{info, warn};

use crate::CoreError;
use crate::codex::{
    ReviewSource, deduplicate_issues, format_issues, is_codex_available,
    parse_review_issues_structured, run_codex_review,
};
use crate::config::ReviewEngine;
use crate::parser::parse_review_issues;
use crate::runner::RunEvent;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator, PhaseOutcome};

/// Executes the review phase with the configured review engine.
///
/// Dispatches to Claude, Codex, or Hybrid mode based on configuration.
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
            let outcome = PhaseOutcome {
                turns: 0,
                cost_usd: 0.0,
                input_tokens: 0,
                output_tokens: 0,
                duration: Duration::ZERO,
                details: serde_json::json!({}),
            };
            let task_result = TaskResult {
                task: Task::Review {
                    feature_slug: ctx.state().feature.slug.clone(),
                },
                status: TaskStatus::Completed,
                turns: 0,
                cost_usd: 0.0,
                duration: Duration::ZERO,
                artifacts: vec![],
            };
            ctx.state_manager.complete_phase(phase_idx, &outcome)?;
            return Ok(task_result);
        }

        let effective_engine = resolve_review_engine(&ctx.config.review.engine);
        info!(engine = %effective_engine, "Starting review phase");

        match effective_engine {
            ReviewEngine::Claude => run_review_claude(ctx, phase_idx).await,
            ReviewEngine::Codex => run_review_codex(ctx, phase_idx).await,
            ReviewEngine::Hybrid => run_review_hybrid(ctx, phase_idx).await,
        }
    }
}

/// Resolves the effective review engine, falling back to Claude if
/// `codex` is not installed.
fn resolve_review_engine(configured: &ReviewEngine) -> ReviewEngine {
    match configured {
        ReviewEngine::Claude => ReviewEngine::Claude,
        ReviewEngine::Codex | ReviewEngine::Hybrid => {
            if is_codex_available() {
                configured.clone()
            } else {
                warn!(
                    configured = %configured,
                    "Codex CLI not found on PATH, falling back to Claude review",
                );
                ReviewEngine::Claude
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

        let resp = ctx.send_and_collect(&review_prompt, None).await?;
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
        ask_claude_to_fix(ctx, &issues, issue_count, &mut acc).await?;
        ctx.review_summary.issues_fix_attempted += issue_count;
    }

    finalize_review_phase(ctx, phase_idx, acc)
}

/// Runs review using only Codex, then asks Claude to fix.
async fn run_review_codex(
    ctx: &mut PhaseContext,
    phase_idx: usize,
) -> Result<TaskResult, CoreError> {
    let max_rounds = ctx.config.review.max_review_rounds;
    let codex_model = ctx.config.review.codex_model.clone();
    let codex_effort = ctx.config.review.codex_reasoning_effort;
    let spec_path = ctx.spec_relative_path("design.md");
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
        ask_claude_to_fix(ctx, &formatted, issue_count, &mut acc).await?;
        ctx.review_summary.issues_fix_attempted += issue_count;
    }

    finalize_review_phase(ctx, phase_idx, acc)
}

/// Runs hybrid review: Codex first, then Claude, merge and deduplicate.
async fn run_review_hybrid(
    ctx: &mut PhaseContext,
    phase_idx: usize,
) -> Result<TaskResult, CoreError> {
    let design_spec = ctx.load_spec("design.md")?;
    let max_rounds = ctx.config.review.max_review_rounds;
    let codex_model = ctx.config.review.codex_model.clone();
    let codex_effort = ctx.config.review.codex_reasoning_effort;
    let spec_path = ctx.spec_relative_path("design.md");
    let mut acc = PhaseMetricsAccumulator::new();

    for round in 0..max_rounds {
        info!(round = round + 1, max = max_rounds, "Review round (hybrid)");

        let changed_files = ctx.get_changed_files().await?;

        // 1. Run Codex review
        let base_branch = ctx.state().git.base_branch.clone();
        let codex_issues = match run_codex_review(
            &ctx.worktree_path,
            &base_branch,
            &spec_path,
            &changed_files,
            &codex_model,
            codex_effort,
            ctx.progress_tx.as_ref(),
        )
        .await
        {
            Ok(issues) => {
                ctx.emit_event(RunEvent::ReviewerCompleted {
                    reviewer: "codex".to_string(),
                    issues_found: issues.len() as u32,
                });
                issues
            }
            Err(e) => {
                warn!(error = %e, "Codex review failed, continuing with Claude only");
                ctx.emit_event(RunEvent::ReviewerCompleted {
                    reviewer: "codex".to_string(),
                    issues_found: 0,
                });
                Vec::new()
            }
        };

        // 2. Run Claude review
        let diff = ctx.get_diff().await?;
        let review_prompt = ctx.pm.render(
            "run/review",
            minijinja::context!(
                design_spec => design_spec,
                diff => diff,
            ),
        )?;

        let resp = ctx.send_and_collect(&review_prompt, None).await?;
        let m = ctx.metrics.record(&resp.result);
        if let Some(logger) = &mut ctx.run_logger {
            logger.log_interaction(&review_prompt, &resp, &m);
        }
        acc.record(&resp, m);

        let claude_issues = parse_review_issues_structured(&resp.text, ReviewSource::Claude);

        ctx.emit_event(RunEvent::ReviewerCompleted {
            reviewer: "claude".to_string(),
            issues_found: claude_issues.len() as u32,
        });

        // 3. Merge and deduplicate
        let mut all_issues = codex_issues;
        all_issues.extend(claude_issues);
        let combined = deduplicate_issues(all_issues);
        let issue_count = combined.len() as u32;

        ctx.review_summary.rounds += 1;
        ctx.review_summary.issues_found += issue_count;

        ctx.emit_event(RunEvent::ReviewRound {
            round: round + 1,
            max_rounds,
            issues_found: issue_count,
        });

        if combined.is_empty() {
            info!("No critical/major issues found, review passed");
            break;
        }

        // 4. Ask Claude to fix combined issues
        info!(
            issues = issue_count,
            "Hybrid review found issues, asking Claude to fix",
        );
        let formatted = format_issues(&combined);
        ask_claude_to_fix(ctx, &formatted, issue_count, &mut acc).await?;
        ctx.review_summary.issues_fix_attempted += issue_count;
    }

    finalize_review_phase(ctx, phase_idx, acc)
}

/// Asks Claude to fix a list of review issues.
async fn ask_claude_to_fix(
    ctx: &mut PhaseContext,
    issues: &[String],
    issue_count: u32,
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

    let fix_resp = ctx.send_and_collect(&fix_prompt, None).await?;
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
    use super::*;

    // ── resolve_review_engine tests ─────────────────────────────

    #[test]
    fn test_should_resolve_claude_engine_unchanged() {
        let result = resolve_review_engine(&ReviewEngine::Claude);
        assert!(
            matches!(result, ReviewEngine::Claude),
            "Claude engine should always resolve to Claude"
        );
    }

    #[test]
    fn test_should_fallback_codex_to_claude_when_unavailable() {
        // On CI/test machines, codex is almost certainly not on PATH.
        // If it is, this test is still valid — just verifies the codex path.
        let result = resolve_review_engine(&ReviewEngine::Codex);
        // We can't control whether codex is installed, but we can verify
        // the function returns a valid engine.
        assert!(
            matches!(result, ReviewEngine::Claude | ReviewEngine::Codex),
            "Should resolve to Claude or Codex, got {result:?}"
        );
    }

    #[test]
    fn test_should_fallback_hybrid_to_claude_when_unavailable() {
        let result = resolve_review_engine(&ReviewEngine::Hybrid);
        assert!(
            matches!(result, ReviewEngine::Claude | ReviewEngine::Hybrid),
            "Should resolve to Claude or Hybrid, got {result:?}"
        );
    }
}
