//! AI-assisted verification phase executor.
//!
//! Uses the verify backend (typically Codex) to evaluate functional
//! correctness, design conformance, and integration points against
//! the verification plan generated during `coda plan`. If issues are
//! found, Claude (run session) fixes them.

use tracing::{info, warn};

use crate::CoreError;
use crate::parser::parse_ai_verification;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator, skip_disabled_phase};

/// Executes the verify phase with AI-assisted verification.
///
/// Uses the verify backend (typically Codex) to evaluate functional
/// correctness, design conformance, and integration points against
/// the verification plan. Findings are fixed by Claude (run session).
///
/// # Example
///
/// ```no_run
/// use coda_core::phases::verify::VerifyPhaseExecutor;
///
/// let executor = VerifyPhaseExecutor;
/// // executor.execute(&mut ctx, phase_idx).await?;
/// ```
pub struct VerifyPhaseExecutor;

impl PhaseExecutor for VerifyPhaseExecutor {
    async fn execute(
        &mut self,
        ctx: &mut PhaseContext,
        phase_idx: usize,
    ) -> Result<TaskResult, CoreError> {
        ctx.state_manager.mark_phase_running(phase_idx)?;

        if !ctx.config.verify.enabled {
            info!("Verify phase disabled, skipping");
            let feature_slug = ctx.state().feature.slug.clone();
            return skip_disabled_phase(
                &mut ctx.state_manager,
                phase_idx,
                Task::Verify { feature_slug },
            );
        }

        let verification_spec = ctx.load_spec("verification.md")?;
        let ai_verification_enabled = ctx.config.verify.ai_verification;
        let session_id = if ctx.config.agent.isolate_quality_phases {
            Some("verify-fix")
        } else {
            None
        };

        let mut acc = PhaseMetricsAccumulator::new();

        // ── AI-Assisted Verification ──────────────────────────────
        if ai_verification_enabled {
            let passed = run_ai_verification(ctx, &verification_spec, session_id, &mut acc).await?;
            ctx.verification_summary.verified = passed;
        } else {
            info!("AI verification disabled, skipping");
            ctx.verification_summary.verified = true;
        }

        // ── Finalize ────────────────────────────────────────────────
        let outcome = acc.into_outcome(serde_json::json!({
            "ai_verified": ctx.verification_summary.verified,
        }));
        let task_result = TaskResult {
            task: Task::Verify {
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

/// Runs AI-assisted verification.
///
/// Uses the verify backend (typically Codex) to evaluate functional
/// correctness, design conformance, and integration points. If issues
/// are found, Claude (run session) fixes them.
///
/// Returns `true` if verification passed, `false` otherwise.
async fn run_ai_verification(
    ctx: &mut PhaseContext,
    verification_spec: &str,
    session_id: Option<&str>,
    acc: &mut PhaseMetricsAccumulator,
) -> Result<bool, CoreError> {
    info!("Starting AI-assisted verification");

    // Prepare isolated Codex session for judgment (same pattern as review.rs)
    let verify_resolved = ctx.config.resolve_verify();
    let run_resolved = ctx.config.resolve_run();
    let needs_isolated_subprocess = ctx.config.agent.isolate_quality_phases
        && (verify_resolved.backend != run_resolved.backend
            || verify_resolved.model != run_resolved.model);

    let mut isolated_session = if needs_isolated_subprocess {
        info!(
            verify_backend = %verify_resolved.backend,
            verify_model = %verify_resolved.model,
            "Creating isolated verify session",
        );
        let mut session = ctx.create_isolated_session(&verify_resolved)?;
        session.connect().await?;
        Some(session)
    } else {
        None
    };

    let verify_prompt = ctx.pm.render(
        "run/verify",
        minijinja::context!(
            verification_spec => verification_spec,
        ),
    )?;

    // Send to Codex (isolated) or shared session with verify session_id
    let verify_session_id = if isolated_session.is_some() {
        None
    } else if ctx.config.agent.isolate_quality_phases {
        Some("verify")
    } else {
        None
    };

    let resp = if let Some(session) = &mut isolated_session {
        session.send(&verify_prompt, None).await?
    } else {
        ctx.send_and_collect(&verify_prompt, verify_session_id)
            .await?
    };

    let m = ctx.metrics.record(&resp.result);
    if let Some(logger) = &mut ctx.run_logger {
        logger.log_interaction(&verify_prompt, &resp, &m);
    }
    acc.record(&resp, m);

    // Parse AI verification response
    let (passed, findings) = parse_ai_verification(&resp.text);

    if passed {
        info!("AI verification passed");
    } else {
        warn!(
            findings = findings.len(),
            "AI verification found issues, asking Claude to fix"
        );

        let findings_text = findings
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {f}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let fix_prompt = format!(
            "The AI verification review found the following issues:\n\n\
             {findings_text}\n\n\
             ## Instructions\n\n\
             1. Analyze each finding and determine the root cause\n\
             2. Fix the code to address each issue\n\
             3. Ensure all tests still pass after your fixes\n\n\
             Refer to the design specification and verification plan provided earlier.",
        );

        // Fix via Claude (run session)
        let fix_resp = ctx.send_and_collect(&fix_prompt, session_id).await?;
        let fm = ctx.metrics.record(&fix_resp.result);
        if let Some(logger) = &mut ctx.run_logger {
            logger.log_interaction(&fix_prompt, &fix_resp, &fm);
        }
        acc.record(&fix_resp, fm);
    }

    // Disconnect isolated session if we created one
    if let Some(session) = &mut isolated_session {
        session.disconnect().await;
    }

    Ok(passed)
}
