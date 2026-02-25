//! Pull request creation phase executor.
//!
//! Creates a pull request after all dev/quality phases complete.
//! Handles squash preparation, PR URL extraction from agent response
//! and `gh` CLI fallback.

use std::future::Future;
use std::path::Path;
use std::time::Instant;

use tracing::{info, warn};

use crate::CoreError;
use crate::async_ops::AsyncGhOps;
use crate::parser::{extract_pr_number, extract_pr_title, extract_pr_url};
use crate::state::PrInfo;
use crate::task::{Task, TaskResult, TaskStatus};

use super::PhaseContext;

/// Creates a pull request after all phases complete.
///
/// Sends a PR creation prompt to the agent, then extracts the PR URL from:
/// 1. Assistant text response
/// 2. Tool result output (bash stdout from `gh pr create`)
/// 3. Fallback: queries `gh pr list --head <branch>` directly
///
/// The PR title is extracted from the agent response text (looking for
/// `**Title:**`, `Title:`, or `--title "..."` patterns). If extraction
/// fails and a PR number is available, falls back to querying
/// `gh pr view --json title`. A generated fallback title is used as a
/// last resort.
///
/// Returns `TaskStatus::Failed` if no PR could be found after all attempts.
///
/// # Example
///
/// ```no_run
/// use coda_core::phases::pr::create_pr;
///
/// // let result = create_pr(&mut ctx).await?;
/// ```
pub async fn create_pr(ctx: &mut PhaseContext) -> Result<TaskResult, CoreError> {
    let design_spec = ctx.load_spec("design.md")?;
    let checks = &ctx.config.checks;
    let start = Instant::now();

    let squashed = !ctx.pre_squash_commits.is_empty();
    let commits = if squashed {
        ctx.pre_squash_commits.clone()
    } else {
        ctx.get_commits().await?
    };

    let all_checks_passed = ctx.verification_summary.checks_passed
        == ctx.verification_summary.checks_total
        && ctx.verification_summary.checks_total > 0;
    let is_draft = !all_checks_passed;
    let model = &ctx.config.agent.model;
    let coda_version = env!("CARGO_PKG_VERSION");

    let state_snapshot = ctx.state().clone();
    let pr_prompt = ctx.pm.render(
        "run/create_pr",
        minijinja::context!(
            design_spec => design_spec,
            commits => commits,
            squashed => squashed,
            original_commits => if squashed { &ctx.pre_squash_commits } else { &commits },
            state => &state_snapshot,
            checks => checks,
            review_summary => &ctx.review_summary,
            verification_summary => &ctx.verification_summary,
            all_checks_passed => all_checks_passed,
            is_draft => is_draft,
            model => model,
            coda_version => coda_version,
        ),
    )?;

    let resp = ctx.send_and_collect(&pr_prompt, Some("create-pr")).await?;
    let pr_metrics = ctx.metrics.record(&resp.result);
    if let Some(logger) = &mut ctx.run_logger {
        logger.log_interaction(&pr_prompt, &resp, &pr_metrics);
    }

    // Try to extract PR URL from all collected text (assistant text + tool output)
    let all_text = resp.all_text();
    let url_from_text = extract_pr_url(&all_text);

    let url_from_gh = if url_from_text.is_none() {
        info!("PR URL not found in agent response, checking via gh CLI...");
        check_pr_exists_via_gh(ctx).await
    } else {
        None
    };

    let pr_url = url_from_text.clone().or(url_from_gh.clone());

    if let Some(logger) = &mut ctx.run_logger {
        logger.log_pr_extraction(
            url_from_text.as_deref(),
            url_from_gh.as_deref(),
            pr_url.as_deref(),
        );
    }

    let feature_slug = ctx.state().feature.slug.clone();
    let status = if let Some(ref url) = pr_url {
        info!(url = %url, "PR created");
        let pr_number = extract_pr_number(url).unwrap_or(0);
        let title = resolve_pr_title(
            &all_text,
            pr_number,
            &ctx.gh,
            &ctx.worktree_path,
            &feature_slug,
        )
        .await;
        ctx.state_manager.set_pr(PrInfo {
            url: url.clone(),
            number: pr_number,
            title,
        });
        ctx.state_manager.save()?;
        TaskStatus::Completed
    } else {
        let msg = "PR creation failed: no PR URL found in agent response or via gh CLI";
        warn!(msg);
        TaskStatus::Failed {
            error: msg.to_string(),
        }
    };

    Ok(TaskResult {
        task: Task::CreatePr { feature_slug },
        status,
        turns: resp.result.as_ref().map_or(1, |r| r.num_turns),
        cost_usd: pr_metrics.cost_usd,
        duration: start.elapsed(),
        artifacts: vec![],
    })
}

/// Resolves the PR title using a multi-strategy approach.
///
/// 1. Extract from agent response text (`**Title:**`, `Title:`, `--title "..."`)
/// 2. Query `gh pr view --json title` (if a valid PR number is available)
/// 3. Fall back to a generated `feat(<slug>): ...` title
///
/// Takes individual fields instead of `&PhaseContext` so the returned
/// future does not capture a non-`Send` reference.
async fn resolve_pr_title(
    agent_text: &str,
    pr_number: u32,
    gh: &AsyncGhOps,
    worktree_path: &Path,
    feature_slug: &str,
) -> String {
    // Strategy 1: extract from agent response text
    if let Some(title) = extract_pr_title(agent_text) {
        info!(title = %title, "PR title extracted from agent response");
        return title;
    }

    // Strategy 2: query gh CLI (via spawn_blocking)
    if pr_number > 0
        && let Some(title) = gh.pr_view_title(pr_number, worktree_path).await
    {
        info!(title = %title, "PR title extracted via gh pr view");
        return title;
    }

    // Strategy 3: generated fallback
    let fallback = format!("feat({feature_slug}): feature implementation");
    info!(title = %fallback, "Using generated fallback PR title");
    fallback
}

/// Squashes all feature branch commits into the staging area.
///
/// Collects the original commit list (for PR body context), finds
/// the merge-base with the base branch, and soft-resets HEAD back
/// to that point. After this call, all feature changes are staged
/// but uncommitted — the agent will create a single commit with a
/// concise message during the `create_pr` phase.
///
/// Git operations are run via `spawn_blocking` to avoid blocking
/// the async runtime.
///
/// # Errors
///
/// Returns `CoreError::GitError` if `merge-base` or `reset --soft` fails.
pub async fn prepare_squash(ctx: &mut PhaseContext) -> Result<(), CoreError> {
    ctx.pre_squash_commits = ctx.get_commits().await?;
    if ctx.pre_squash_commits.is_empty() {
        info!("No commits to squash");
        return Ok(());
    }

    let base = ctx.state().git.base_branch.clone();
    let merge_base = ctx.git.merge_base(&ctx.worktree_path, &base).await?;
    info!(
        merge_base = %merge_base,
        commits = ctx.pre_squash_commits.len(),
        "Squashing commits before PR"
    );
    ctx.git.reset_soft(&ctx.worktree_path, &merge_base).await?;
    Ok(())
}

/// Checks whether a PR exists for the given branch using `gh pr list`.
///
/// Falls back to querying the GitHub CLI directly when the agent's text
/// response does not contain an extractable PR URL.
///
/// Returns an owned future (via `async move`) that does not capture
/// `&ctx`, so the non-`Send` `&PhaseContext` borrow ends before the
/// caller's `.await`.
fn check_pr_exists_via_gh(ctx: &PhaseContext) -> impl Future<Output = Option<String>> + Send {
    let gh = ctx.gh.clone();
    let branch = ctx.state().git.branch.clone();
    let cwd = ctx.worktree_path.clone();
    async move { gh.pr_url_for_branch(&branch, &cwd).await }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::gh::GhOps;

    use super::*;

    /// Stub `GhOps` that always returns `None`.
    #[derive(Debug)]
    struct StubGhOps;

    impl GhOps for StubGhOps {
        fn pr_view_state(&self, _pr_number: u32) -> Result<Option<crate::gh::PrStatus>, CoreError> {
            Ok(None)
        }

        fn pr_list_by_branch(
            &self,
            _branch: &str,
        ) -> Result<Option<crate::gh::PrStatus>, CoreError> {
            Ok(None)
        }

        fn pr_url_for_branch(&self, _branch: &str, _cwd: &Path) -> Option<String> {
            None
        }

        fn pr_view_title(&self, _pr_number: u32, _cwd: &Path) -> Option<String> {
            None
        }
    }

    /// Stub `GhOps` that returns a title for any PR number.
    #[derive(Debug)]
    struct TitledGhOps;

    impl GhOps for TitledGhOps {
        fn pr_view_state(&self, _pr_number: u32) -> Result<Option<crate::gh::PrStatus>, CoreError> {
            Ok(None)
        }

        fn pr_list_by_branch(
            &self,
            _branch: &str,
        ) -> Result<Option<crate::gh::PrStatus>, CoreError> {
            Ok(None)
        }

        fn pr_url_for_branch(&self, _branch: &str, _cwd: &Path) -> Option<String> {
            None
        }

        fn pr_view_title(&self, _pr_number: u32, _cwd: &Path) -> Option<String> {
            Some("Title from gh CLI".to_string())
        }
    }

    #[tokio::test]
    async fn test_should_extract_title_from_agent_text() {
        let gh = AsyncGhOps::new(Arc::new(StubGhOps));
        let text = "I created the PR.\n**Title:** feat: add awesome feature\nDone.";
        let title = resolve_pr_title(text, 42, &gh, Path::new("/tmp"), "awesome").await;
        assert_eq!(title, "feat: add awesome feature");
    }

    #[tokio::test]
    async fn test_should_fallback_to_gh_cli_when_text_has_no_title() {
        let gh = AsyncGhOps::new(Arc::new(TitledGhOps));
        let text = "I created the PR, but didn't mention the title.";
        let title = resolve_pr_title(text, 42, &gh, Path::new("/tmp"), "awesome").await;
        assert_eq!(title, "Title from gh CLI");
    }

    #[tokio::test]
    async fn test_should_use_generated_fallback_when_all_strategies_fail() {
        let gh = AsyncGhOps::new(Arc::new(StubGhOps));
        let text = "No title here.";
        let title = resolve_pr_title(text, 0, &gh, Path::new("/tmp"), "my-feature").await;
        assert_eq!(title, "feat(my-feature): feature implementation");
    }

    #[tokio::test]
    async fn test_should_skip_gh_cli_when_pr_number_is_zero() {
        // Even with TitledGhOps, pr_number=0 should skip strategy 2
        let gh = AsyncGhOps::new(Arc::new(TitledGhOps));
        let text = "No title here.";
        let title = resolve_pr_title(text, 0, &gh, Path::new("/tmp"), "test-feat").await;
        assert_eq!(title, "feat(test-feat): feature implementation");
    }
}
