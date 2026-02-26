//! Verification phase executor.
//!
//! Runs the verification plan (build, format, lint, test) and retries
//! with agent-driven fixes when checks fail. The initial attempt is
//! always executed; retries are limited by `config.verify.max_verify_retries`.

use tracing::{error, info, warn};

use crate::CoreError;
use crate::parser::parse_verification_result;
use crate::runner::RunEvent;
use crate::session::AgentResponse;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator};

/// Executes the verify phase with a fix loop.
///
/// Runs an initial verification attempt. If any check fails, asks the
/// agent to fix the issue and re-verifies up to `max_verify_retries`
/// additional times. Total attempts = 1 + `max_verify_retries`.
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

impl VerifyPhaseExecutor {
    /// Sends a prompt using the appropriate session (isolated or shared).
    ///
    /// When an isolated verify session exists, uses it directly (no session_id
    /// needed since the whole subprocess is dedicated). Otherwise falls back to
    /// the shared session with optional session_id isolation.
    async fn send(
        ctx: &mut PhaseContext,
        isolated_session: &mut Option<crate::session::AgentSession>,
        session_id: Option<&str>,
        prompt: &str,
    ) -> Result<AgentResponse, CoreError> {
        if let Some(session) = isolated_session {
            session.send(prompt, None).await
        } else {
            ctx.send_and_collect(prompt, session_id).await
        }
    }
}

impl PhaseExecutor for VerifyPhaseExecutor {
    async fn execute(
        &mut self,
        ctx: &mut PhaseContext,
        phase_idx: usize,
    ) -> Result<TaskResult, CoreError> {
        ctx.state_manager.mark_phase_running(phase_idx)?;

        let verification_spec = ctx.load_spec("verification.md")?;
        let checks = ctx.config.checks.clone();
        let max_retries = ctx.config.verify.max_verify_retries;

        // Determine whether we need an isolated subprocess for verify.
        // When isolate_quality_phases is enabled AND verify config differs
        // from run config (different backend or model), create a dedicated
        // AgentSession so the verify phase uses its own agent subprocess.
        let verify_resolved = ctx.config.resolve_verify();
        let run_resolved = ctx.config.resolve_run();
        let needs_isolated_subprocess = ctx.config.agent.isolate_quality_phases
            && (verify_resolved.backend != run_resolved.backend
                || verify_resolved.model != run_resolved.model);

        let (mut isolated_session, session_id) = if needs_isolated_subprocess {
            info!(
                verify_backend = %verify_resolved.backend,
                verify_model = %verify_resolved.model,
                "Creating isolated verify session (differs from run config)",
            );
            let session = ctx.create_isolated_session(&verify_resolved)?;
            (Some(session), None)
        } else if ctx.config.agent.isolate_quality_phases {
            (None, Some("verify"))
        } else {
            (None, None)
        };

        // Total attempts = 1 initial + max_retries
        let max_attempts = 1 + max_retries;
        let mut acc = PhaseMetricsAccumulator::new();

        for attempt in 1..=max_attempts {
            let is_retry = attempt > 1;
            info!(
                attempt,
                max_attempts,
                retry = is_retry,
                "Verification attempt"
            );

            let verify_prompt = ctx.pm.render(
                "run/verify",
                minijinja::context!(
                    verification_spec => verification_spec,
                    checks => &checks,
                ),
            )?;

            let resp = Self::send(ctx, &mut isolated_session, session_id, &verify_prompt).await?;
            let m = ctx.metrics.record(&resp.result);
            if let Some(logger) = &mut ctx.run_logger {
                logger.log_interaction(&verify_prompt, &resp, &m);
            }
            acc.record(&resp, m);

            // Parse verification result
            let (passed, failed_details) = parse_verification_result(&resp.text);
            ctx.verification_summary.checks_total = passed + failed_details.len() as u32;
            ctx.verification_summary.checks_passed = passed;

            let all_passed = failed_details.is_empty();
            ctx.emit_event(RunEvent::VerifyAttempt {
                attempt,
                max_attempts,
                passed: all_passed,
            });

            if all_passed {
                info!("All verification checks passed");
                break;
            }

            if attempt == max_attempts {
                if ctx.config.verify.fail_on_max_attempts {
                    error!(
                        "Max verification attempts reached with failing checks \
                         (fail_on_max_attempts=true)",
                    );
                    return Err(CoreError::AgentError(
                        "Verification failed: checks still failing after all retry \
                         attempts (fail_on_max_attempts is enabled)"
                            .to_string(),
                    ));
                }
                warn!("Max verification attempts reached, proceeding with failures");
                break;
            }

            info!(
                failures = failed_details.len(),
                "Verification failed, asking agent to fix"
            );

            let failures = failed_details.join("\n");
            let checks_str = checks.join("`, `");
            let fix_prompt = format!(
                "Verification failed. The following checks did not pass:\n\n\
                 ## Failed Checks\n\n{failures}\n\n\
                 ## Instructions\n\n\
                 1. Analyze each failure and identify the root cause\n\
                 2. Fix the code to address each failure\n\
                 3. Re-run all checks: `{checks_str}`\n\
                 4. Ensure ALL checks pass before reporting back\n\n\
                 Refer to the design specification and verification plan provided earlier.",
            );

            let fix_resp = Self::send(ctx, &mut isolated_session, session_id, &fix_prompt).await?;
            let fm = ctx.metrics.record(&fix_resp.result);
            if let Some(logger) = &mut ctx.run_logger {
                logger.log_interaction(&fix_prompt, &fix_resp, &fm);
            }
            acc.record(&fix_resp, fm);
        }

        let outcome = acc.into_outcome(serde_json::json!({
            "max_verify_retries": max_retries,
            "checks_passed": ctx.verification_summary.checks_passed,
            "checks_total": ctx.verification_summary.checks_total,
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
