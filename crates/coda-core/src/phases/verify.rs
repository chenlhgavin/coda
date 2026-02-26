//! Two-tier verification phase executor.
//!
//! ## Architecture
//!
//! ```text
//! VerifyPhaseExecutor::execute()
//!   │
//!   ├── Tier 1: Deterministic Check Runner
//!   │     - Executes config.checks via tokio subprocess
//!   │     - Captures real exit codes + stdout/stderr
//!   │     - On failure: Claude (run session) fixes code
//!   │     - Retries up to max_attempts
//!   │
//!   └── Tier 2: AI-Assisted Verification (optional)
//!         - Only runs when Tier 1 passes AND ai_verification=true
//!         - Codex (verify backend) evaluates functional correctness
//!         - On findings: Claude (run session) fixes code
//! ```
//!
//! ## Role Assignment
//!
//! | Action | Executor | Rationale |
//! |--------|----------|-----------|
//! | Run checks | System (subprocess) | Deterministic, no AI needed |
//! | Fix check failures | Claude (run session) | Full development context |
//! | Tier 2 judgment | Codex (verify backend) | Independent reviewer, no self-review bias |
//! | Fix Tier 2 findings | Claude (run session) | Full development context |

use tracing::{error, info, warn};

use crate::CoreError;
use crate::check_runner::{self, CheckRunResult};
use crate::parser::parse_ai_verification;
use crate::runner::RunEvent;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator};

/// Executes the verify phase with a two-tier architecture.
///
/// **Tier 1**: Deterministic checks (build/test/lint/format) are run
/// directly as subprocesses. Failures are fixed by Claude (run session).
///
/// **Tier 2**: AI-assisted verification for functional correctness and
/// design conformance, evaluated by Codex (verify backend config).
/// Findings are fixed by Claude (run session).
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

        let verification_spec = ctx.load_spec("verification.md")?;
        let checks = ctx.config.checks.clone();
        let max_retries = ctx.config.verify.max_verify_retries;
        let max_attempts = 1 + max_retries;
        let check_timeout = ctx.config.verify.check_timeout_secs;
        let ai_verification_enabled = ctx.config.verify.ai_verification;
        let session_id = if ctx.config.agent.isolate_quality_phases {
            Some("verify-fix")
        } else {
            None
        };

        let mut acc = PhaseMetricsAccumulator::new();

        // ── Tier 1: Deterministic Check Runner ──────────────────────
        let check_result = if checks.is_empty() {
            info!("No checks configured, skipping Tier 1");
            CheckRunResult {
                checks: vec![],
                total_duration: std::time::Duration::ZERO,
            }
        } else {
            run_deterministic_checks_with_fixes(
                ctx,
                &checks,
                max_attempts,
                check_timeout,
                session_id,
                &mut acc,
            )
            .await?
        };

        let tier1_passed = check_result.all_passed();

        // Update verification summary from real data
        ctx.verification_summary.checks_total = checks.len() as u32;
        ctx.verification_summary.checks_passed = if tier1_passed {
            checks.len() as u32
        } else {
            check_result.passed_count()
        };

        // ── Tier 2: AI-Assisted Verification ────────────────────────
        if tier1_passed && ai_verification_enabled {
            run_ai_verification(ctx, &verification_spec, &check_result, session_id, &mut acc)
                .await?;
        } else if !tier1_passed {
            info!("Tier 1 checks did not all pass, skipping Tier 2 AI verification");
        } else {
            info!("AI verification disabled, skipping Tier 2");
        }

        // ── Finalize ────────────────────────────────────────────────
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

/// Runs deterministic checks with Claude-driven fix loops.
///
/// Executes all configured checks via subprocess. On failure, sends the
/// real stderr/stdout to Claude (run session) for fixing, then re-runs.
/// Repeats up to `max_attempts` times.
async fn run_deterministic_checks_with_fixes(
    ctx: &mut PhaseContext,
    checks: &[String],
    max_attempts: u32,
    check_timeout: u64,
    session_id: Option<&str>,
    acc: &mut PhaseMetricsAccumulator,
) -> Result<CheckRunResult, CoreError> {
    let total_checks = checks.len() as u32;
    let mut final_result = None;

    for attempt in 1..=max_attempts {
        let is_retry = attempt > 1;
        info!(
            attempt,
            max_attempts,
            retry = is_retry,
            "Verification attempt"
        );

        // Emit CheckStarting for each check
        for (i, check) in checks.iter().enumerate() {
            ctx.emit_event(RunEvent::CheckStarting {
                command: check.clone(),
                index: i as u32,
                total: total_checks,
            });
        }

        // Run all checks
        let result = check_runner::run_checks(&ctx.worktree_path, checks, check_timeout).await?;

        // Emit CheckCompleted for each executed check
        for check in &result.checks {
            ctx.emit_event(RunEvent::CheckCompleted {
                command: check.command.clone(),
                passed: check.passed,
                duration: check.duration,
            });
        }

        let all_passed = result.all_passed();
        ctx.emit_event(RunEvent::VerifyAttempt {
            attempt,
            max_attempts,
            passed: all_passed,
        });

        if all_passed {
            info!("All {} checks passed on attempt {attempt}", total_checks);
            final_result = Some(result);
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
            final_result = Some(result);
            break;
        }

        // Ask Claude (run session) to fix based on real error output
        info!(
            failures = result.failed_details().len(),
            "Checks failed, asking Claude to fix"
        );

        let failures = result.failed_details().join("\n\n");
        let checks_str = checks.join("`, `");
        let fix_prompt = format!(
            "The following automated checks failed:\n\n\
             {failures}\n\n\
             ## Instructions\n\n\
             1. Analyze each failure using the real error output above\n\
             2. Fix the code to address each failure\n\
             3. After fixing, ensure all checks would pass: `{checks_str}`\n\n\
             Do NOT run the checks yourself — the system will re-run them automatically.",
        );

        // Fix via Claude (run session) — NOT isolated codex session
        let fix_resp = ctx.send_and_collect(&fix_prompt, session_id).await?;
        let fm = ctx.metrics.record(&fix_resp.result);
        if let Some(logger) = &mut ctx.run_logger {
            logger.log_interaction(&fix_prompt, &fix_resp, &fm);
        }
        acc.record(&fix_resp, fm);
    }

    Ok(final_result.unwrap_or(CheckRunResult {
        checks: vec![],
        total_duration: std::time::Duration::ZERO,
    }))
}

/// Runs Tier 2 AI-assisted verification.
///
/// Uses the verify backend (typically Codex) to evaluate functional
/// correctness, design conformance, and integration points. If issues
/// are found, Claude (run session) fixes them and checks are re-run
/// to ensure fixes didn't break anything.
async fn run_ai_verification(
    ctx: &mut PhaseContext,
    verification_spec: &str,
    check_result: &CheckRunResult,
    session_id: Option<&str>,
    acc: &mut PhaseMetricsAccumulator,
) -> Result<(), CoreError> {
    info!("Starting Tier 2 AI-assisted verification");

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
            "Creating isolated verify session for Tier 2",
        );
        let mut session = ctx.create_isolated_session(&verify_resolved)?;
        session.connect().await?;
        Some(session)
    } else {
        None
    };

    // Build check results summary for context
    let check_summary: Vec<String> = check_result
        .checks
        .iter()
        .map(|c| {
            format!(
                "- `{}`: {} ({:.1}s)",
                c.command,
                if c.passed { "PASSED" } else { "FAILED" },
                c.duration.as_secs_f64(),
            )
        })
        .collect();

    let verify_prompt = ctx.pm.render(
        "run/verify",
        minijinja::context!(
            verification_spec => verification_spec,
            check_results => check_summary.join("\n"),
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
        info!("Tier 2 AI verification passed");
    } else {
        info!(
            findings = findings.len(),
            "Tier 2 AI verification found issues, asking Claude to fix"
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
             3. Ensure all configured checks still pass after your fixes\n\n\
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

    Ok(())
}
