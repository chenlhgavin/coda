//! Documentation update phase executor.
//!
//! Regenerates `.coda.md` and updates `README.md` in the worktree,
//! validating that both files exist and are non-empty afterwards.

use std::fs;
use std::future::Future;
use std::path::Path;

use tracing::info;

use crate::CoreError;
use crate::async_ops::commit_coda_artifacts_async;
use crate::task::{Task, TaskResult, TaskStatus};

use super::{PhaseContext, PhaseExecutor, PhaseMetricsAccumulator};

/// Executes the update-docs phase.
///
/// Sends the `run/update_docs` prompt to the agent and validates that
/// both `.coda.md` and `README.md` exist and are non-empty afterwards.
/// Retries up to `config.agent.max_retries` times on validation failure.
///
/// # Example
///
/// ```no_run
/// use coda_core::phases::docs::DocsPhaseExecutor;
///
/// let executor = DocsPhaseExecutor;
/// // executor.execute(&mut ctx, phase_idx).await?;
/// ```
pub struct DocsPhaseExecutor;

impl PhaseExecutor for DocsPhaseExecutor {
    async fn execute(
        &mut self,
        ctx: &mut PhaseContext,
        phase_idx: usize,
    ) -> Result<TaskResult, CoreError> {
        ctx.state_manager.mark_phase_running(phase_idx)?;

        let design_spec = ctx.load_spec("design.md")?;
        let max_retries = ctx.config.agent.max_retries;
        let session_id = if ctx.config.agent.isolate_quality_phases {
            Some("update-docs")
        } else {
            None
        };
        let mut acc = PhaseMetricsAccumulator::new();
        let mut docs_valid = false;

        // Send the initial full prompt.
        let state_snapshot = ctx.state().clone();
        let prompt = ctx.pm.render(
            "run/update_docs",
            minijinja::context!(
                design_spec => design_spec,
                state => &state_snapshot,
            ),
        )?;

        let resp = ctx.send_and_collect(&prompt, session_id).await?;
        let m = ctx.metrics.record(&resp.result);
        if let Some(logger) = &mut ctx.run_logger {
            logger.log_interaction(&prompt, &resp, &m);
        }
        acc.record(&resp, m);

        let mut missing = validate_doc_files(&ctx.worktree_path);
        if missing.is_empty() {
            info!("Documentation files validated successfully");
            docs_valid = true;
        }

        // Retry with targeted fix prompts, validating after each response.
        for attempt in 0..max_retries {
            if docs_valid {
                break;
            }

            info!(
                attempt = attempt + 1,
                max = max_retries,
                missing = ?missing,
                "Documentation validation failed, asking agent to fix"
            );

            let fix_prompt = build_doc_fix_prompt(&missing);

            let fix_resp = ctx.send_and_collect(&fix_prompt, session_id).await?;
            let fm = ctx.metrics.record(&fix_resp.result);
            if let Some(logger) = &mut ctx.run_logger {
                logger.log_interaction(&fix_prompt, &fix_resp, &fm);
            }
            acc.record(&fix_resp, fm);

            missing = validate_doc_files(&ctx.worktree_path);
            if missing.is_empty() {
                info!("Documentation files validated successfully");
                docs_valid = true;
            }
        }

        if !docs_valid {
            return Err(CoreError::AgentError(
                "update-docs phase failed: .coda.md and/or README.md missing or empty after all retries".to_string(),
            ));
        }

        commit_doc_updates(ctx).await?;

        let outcome = acc.into_outcome(serde_json::json!({
            "docs_updated": true,
        }));
        let task_result = TaskResult {
            task: Task::UpdateDocs {
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

/// Commits documentation file updates (`.coda.md` and `README.md`).
///
/// Stages both files and creates a commit. Silently succeeds if
/// neither file has changes (e.g., the agent already committed them).
///
/// Returns an owned future (via `async move`) that does not capture
/// `&ctx`, so the non-`Send` `&PhaseContext` borrow ends before the
/// caller's `.await`.
fn commit_doc_updates(ctx: &PhaseContext) -> impl Future<Output = Result<(), CoreError>> + Send {
    let git = ctx.git.clone();
    let cwd = ctx.worktree_path.clone();
    let msg = format!(
        "docs({}): update .coda.md and README.md",
        ctx.state().feature.slug,
    );
    async move { commit_coda_artifacts_async(&git, &cwd, &[".coda.md", "README.md"], &msg).await }
}

/// Validates that required documentation files exist and are non-empty.
///
/// Returns a list of file names that are missing or empty.
/// An empty list means all required documentation files are valid.
fn validate_doc_files(worktree: &Path) -> Vec<&'static str> {
    let coda_md = worktree.join(".coda.md");
    let readme = worktree.join("README.md");

    let coda_ok = coda_md.is_file() && fs::metadata(&coda_md).is_ok_and(|m| m.len() > 0);
    let readme_ok = readme.is_file() && fs::metadata(&readme).is_ok_and(|m| m.len() > 0);

    let mut missing = Vec::new();
    if !coda_ok {
        missing.push(".coda.md");
    }
    if !readme_ok {
        missing.push("README.md");
    }
    missing
}

/// Builds a fix prompt for the agent when documentation validation fails.
fn build_doc_fix_prompt(missing: &[&str]) -> String {
    format!(
        "Documentation update validation failed. The following files are missing or empty:\n\n\
         {}\n\n\
         Please create or fix these files and ensure they contain valid, non-empty Markdown content.\n\
         Refer to the instructions from the previous prompt.",
        missing.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_doc_files tests ────────────────────────────────

    #[test]
    fn test_should_report_both_missing_for_empty_dir() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let missing = validate_doc_files(dir.path());
        assert_eq!(missing, vec![".coda.md", "README.md"]);
    }

    #[test]
    fn test_should_report_empty_when_both_exist_and_nonempty() {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(dir.path().join(".coda.md"), "# Project").expect("write .coda.md");
        fs::write(dir.path().join("README.md"), "# README").expect("write README.md");
        let missing = validate_doc_files(dir.path());
        assert!(
            missing.is_empty(),
            "expected no missing files, got {missing:?}"
        );
    }

    #[test]
    fn test_should_detect_empty_coda_md() {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(dir.path().join(".coda.md"), "").expect("write empty .coda.md");
        fs::write(dir.path().join("README.md"), "# README").expect("write README.md");
        let missing = validate_doc_files(dir.path());
        assert_eq!(missing, vec![".coda.md"]);
    }

    #[test]
    fn test_should_detect_empty_readme() {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(dir.path().join(".coda.md"), "# Project").expect("write .coda.md");
        fs::write(dir.path().join("README.md"), "").expect("write empty README.md");
        let missing = validate_doc_files(dir.path());
        assert_eq!(missing, vec!["README.md"]);
    }

    #[test]
    fn test_should_detect_missing_readme_only() {
        let dir = tempfile::tempdir().expect("create temp dir");
        fs::write(dir.path().join(".coda.md"), "# Project").expect("write .coda.md");
        let missing = validate_doc_files(dir.path());
        assert_eq!(missing, vec!["README.md"]);
    }

    // ── build_doc_fix_prompt tests ──────────────────────────────

    #[test]
    fn test_should_include_both_missing_files_in_fix_prompt() {
        let prompt = build_doc_fix_prompt(&[".coda.md", "README.md"]);
        assert!(prompt.contains(".coda.md, README.md"));
        assert!(prompt.contains("missing or empty"));
    }

    #[test]
    fn test_should_include_single_file_in_fix_prompt() {
        let prompt = build_doc_fix_prompt(&["README.md"]);
        assert!(prompt.contains("README.md"));
        // The file list itself should not be comma-separated with one file
        assert!(!prompt.contains(".coda.md"));
    }
}
