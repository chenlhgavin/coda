//! Async wrappers for blocking git and GitHub CLI operations.
//!
//! The [`GitOps`](crate::git::GitOps) and [`GhOps`](crate::gh::GhOps) traits
//! shell out to `git` and `gh` binaries, which block the calling thread.
//! When called from async contexts this violates the "never block the
//! executor" rule and can starve other tasks.
//!
//! [`AsyncGitOps`] and [`AsyncGhOps`] wrap `Arc<dyn GitOps>` /
//! `Arc<dyn GhOps>` and delegate every operation through
//! [`tokio::task::spawn_blocking`], keeping the Tokio runtime responsive.
//!
//! Sync callers (e.g. `Engine::scan_cleanable_worktrees`) continue using
//! the underlying traits directly via [`AsyncGitOps::inner`].

use std::path::Path;
use std::sync::Arc;

use crate::CoreError;
use crate::gh::{GhOps, PrStatus};
use crate::git::GitOps;

// ── Helpers ─────────────────────────────────────────────────────────

/// Converts a `tokio::task::JoinError` into a `CoreError`.
fn join_err(e: tokio::task::JoinError) -> CoreError {
    CoreError::AgentError(format!("spawn_blocking join error: {e}"))
}

// ── AsyncGitOps ─────────────────────────────────────────────────────

/// Async wrapper around [`GitOps`](crate::git::GitOps).
///
/// Every method clones the inner `Arc` and the path/string arguments
/// into a `'static` closure passed to [`tokio::task::spawn_blocking`].
///
/// # Example
///
/// ```no_run
/// # use std::sync::Arc;
/// # use coda_core::git::DefaultGitOps;
/// # use coda_core::async_ops::AsyncGitOps;
/// let git = AsyncGitOps::new(Arc::new(DefaultGitOps::new("/tmp".into())));
/// // git.diff(...).await
/// ```
#[derive(Clone)]
pub struct AsyncGitOps {
    inner: Arc<dyn GitOps>,
}

impl AsyncGitOps {
    /// Creates a new async wrapper around the given [`GitOps`] implementation.
    pub fn new(inner: Arc<dyn GitOps>) -> Self {
        Self { inner }
    }

    /// Returns a reference to the underlying sync [`GitOps`] implementation.
    ///
    /// Useful when a sync caller needs direct access (e.g., from
    /// [`commit_coda_artifacts`](crate::engine::commit_coda_artifacts)).
    pub fn inner(&self) -> &Arc<dyn GitOps> {
        &self.inner
    }

    /// Async version of [`GitOps::worktree_add`].
    pub async fn worktree_add(
        &self,
        path: &Path,
        branch: &str,
        base: &str,
    ) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let path = path.to_path_buf();
        let branch = branch.to_string();
        let base = base.to_string();
        tokio::task::spawn_blocking(move || git.worktree_add(&path, &branch, &base))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::add`].
    pub async fn add(&self, cwd: &Path, paths: &[&str]) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let owned: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            git.add(&cwd, &refs)
        })
        .await
        .map_err(join_err)?
    }

    /// Async version of [`GitOps::has_staged_changes`].
    pub async fn has_staged_changes(&self, cwd: &Path) -> bool {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        tokio::task::spawn_blocking(move || git.has_staged_changes(&cwd))
            .await
            .unwrap_or(false)
    }

    /// Async version of [`GitOps::commit`].
    pub async fn commit(&self, cwd: &Path, message: &str) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let message = message.to_string();
        tokio::task::spawn_blocking(move || git.commit(&cwd, &message))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::commit_internal`].
    pub async fn commit_internal(&self, cwd: &Path, message: &str) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let message = message.to_string();
        tokio::task::spawn_blocking(move || git.commit_internal(&cwd, &message))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::diff`].
    pub async fn diff(&self, cwd: &Path, base: &str) -> Result<String, CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let base = base.to_string();
        tokio::task::spawn_blocking(move || git.diff(&cwd, &base))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::diff_name_only`].
    pub async fn diff_name_only(&self, cwd: &Path, base: &str) -> Result<Vec<String>, CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let base = base.to_string();
        tokio::task::spawn_blocking(move || git.diff_name_only(&cwd, &base))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::log_oneline`].
    pub async fn log_oneline(&self, cwd: &Path, range: &str) -> Result<String, CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let range = range.to_string();
        tokio::task::spawn_blocking(move || git.log_oneline(&cwd, &range))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::push`].
    pub async fn push(&self, cwd: &Path, branch: &str) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let branch = branch.to_string();
        tokio::task::spawn_blocking(move || git.push(&cwd, &branch))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::merge_base`].
    pub async fn merge_base(&self, cwd: &Path, base: &str) -> Result<String, CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let base = base.to_string();
        tokio::task::spawn_blocking(move || git.merge_base(&cwd, &base))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::reset_soft`].
    pub async fn reset_soft(&self, cwd: &Path, target: &str) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let target = target.to_string();
        tokio::task::spawn_blocking(move || git.reset_soft(&cwd, &target))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::commit_amend_no_edit_internal`].
    pub async fn commit_amend_no_edit_internal(&self, cwd: &Path) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        tokio::task::spawn_blocking(move || git.commit_amend_no_edit_internal(&cwd))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::push_force_with_lease`].
    pub async fn push_force_with_lease(&self, cwd: &Path, branch: &str) -> Result<(), CoreError> {
        let git = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        let branch = branch.to_string();
        tokio::task::spawn_blocking(move || git.push_force_with_lease(&cwd, &branch))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GitOps::detect_default_branch`].
    pub async fn detect_default_branch(&self) -> String {
        let git = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || git.detect_default_branch())
            .await
            .unwrap_or_else(|_| "main".to_string())
    }

    /// Async version of [`GitOps::file_exists_in_ref`].
    pub async fn file_exists_in_ref(&self, git_ref: &str, path: &str) -> bool {
        let git = Arc::clone(&self.inner);
        let git_ref = git_ref.to_string();
        let path = path.to_string();
        tokio::task::spawn_blocking(move || git.file_exists_in_ref(&git_ref, &path))
            .await
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for AsyncGitOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncGitOps").finish_non_exhaustive()
    }
}

// ── AsyncGhOps ──────────────────────────────────────────────────────

/// Async wrapper around [`GhOps`](crate::gh::GhOps).
///
/// Every method clones the inner `Arc` and owned copies of arguments
/// into a `'static` closure passed to [`tokio::task::spawn_blocking`].
///
/// # Example
///
/// ```no_run
/// # use std::sync::Arc;
/// # use coda_core::gh::DefaultGhOps;
/// # use coda_core::async_ops::AsyncGhOps;
/// let gh = AsyncGhOps::new(Arc::new(DefaultGhOps::new("/tmp".into())));
/// // gh.pr_url_for_branch(...).await
/// ```
#[derive(Clone)]
pub struct AsyncGhOps {
    inner: Arc<dyn GhOps>,
}

impl AsyncGhOps {
    /// Creates a new async wrapper around the given [`GhOps`] implementation.
    pub fn new(inner: Arc<dyn GhOps>) -> Self {
        Self { inner }
    }

    /// Returns a reference to the underlying sync [`GhOps`] implementation.
    pub fn inner(&self) -> &Arc<dyn GhOps> {
        &self.inner
    }

    /// Async version of [`GhOps::pr_view_state`].
    pub async fn pr_view_state(&self, pr_number: u32) -> Result<Option<PrStatus>, CoreError> {
        let gh = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || gh.pr_view_state(pr_number))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GhOps::pr_list_by_branch`].
    pub async fn pr_list_by_branch(&self, branch: &str) -> Result<Option<PrStatus>, CoreError> {
        let gh = Arc::clone(&self.inner);
        let branch = branch.to_string();
        tokio::task::spawn_blocking(move || gh.pr_list_by_branch(&branch))
            .await
            .map_err(join_err)?
    }

    /// Async version of [`GhOps::pr_url_for_branch`].
    pub async fn pr_url_for_branch(&self, branch: &str, cwd: &Path) -> Option<String> {
        let gh = Arc::clone(&self.inner);
        let branch = branch.to_string();
        let cwd = cwd.to_path_buf();
        tokio::task::spawn_blocking(move || gh.pr_url_for_branch(&branch, &cwd))
            .await
            .ok()
            .flatten()
    }

    /// Async version of [`GhOps::pr_view_title`].
    pub async fn pr_view_title(&self, pr_number: u32, cwd: &Path) -> Option<String> {
        let gh = Arc::clone(&self.inner);
        let cwd = cwd.to_path_buf();
        tokio::task::spawn_blocking(move || gh.pr_view_title(pr_number, &cwd))
            .await
            .ok()
            .flatten()
    }
}

impl std::fmt::Debug for AsyncGhOps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncGhOps").finish_non_exhaustive()
    }
}

/// Async version of [`commit_coda_artifacts`](crate::engine::commit_coda_artifacts).
///
/// Wraps the blocking git add + commit sequence in
/// [`tokio::task::spawn_blocking`].
///
/// # Errors
///
/// Returns `CoreError::GitError` if staging or committing fails,
/// or `CoreError::AgentError` if the blocking task panics.
pub async fn commit_coda_artifacts_async(
    git: &AsyncGitOps,
    cwd: &Path,
    paths: &[&str],
    message: &str,
) -> Result<(), CoreError> {
    let inner = Arc::clone(git.inner());
    let cwd = cwd.to_path_buf();
    let paths: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
    let message = message.to_string();
    tokio::task::spawn_blocking(move || {
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        crate::engine::commit_coda_artifacts(inner.as_ref(), &cwd, &path_refs, &message)
    })
    .await
    .map_err(join_err)?
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::gh::PrStatus;

    // ── Mock GitOps ──────────────────────────────────────────────

    /// Minimal mock that records calls and returns canned responses.
    #[derive(Debug)]
    struct MockGitOps {
        diff_result: String,
    }

    impl MockGitOps {
        fn new(diff_result: &str) -> Self {
            Self {
                diff_result: diff_result.to_string(),
            }
        }
    }

    impl GitOps for MockGitOps {
        fn worktree_add(&self, _: &Path, _: &str, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn worktree_remove(&self, _: &Path, _: bool) -> Result<(), CoreError> {
            Ok(())
        }
        fn worktree_prune(&self) -> Result<(), CoreError> {
            Ok(())
        }
        fn branch_delete(&self, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn add(&self, _: &Path, _: &[&str]) -> Result<(), CoreError> {
            Ok(())
        }
        fn has_staged_changes(&self, _: &Path) -> bool {
            false
        }
        fn commit(&self, _: &Path, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn diff(&self, _: &Path, _: &str) -> Result<String, CoreError> {
            Ok(self.diff_result.clone())
        }
        fn diff_name_only(&self, _: &Path, _: &str) -> Result<Vec<String>, CoreError> {
            Ok(vec!["src/main.rs".to_string()])
        }
        fn log_oneline(&self, _: &Path, _: &str) -> Result<String, CoreError> {
            Ok("abc1234 feat: test\n".to_string())
        }
        fn push(&self, _: &Path, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn file_exists_in_ref(&self, _: &str, _: &str) -> bool {
            true
        }
        fn detect_default_branch(&self) -> String {
            "main".to_string()
        }
        fn merge_base(&self, _: &Path, _: &str) -> Result<String, CoreError> {
            Ok("deadbeef".to_string())
        }
        fn reset_soft(&self, _: &Path, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
        fn commit_amend_no_edit_internal(&self, _: &Path) -> Result<(), CoreError> {
            Ok(())
        }
        fn push_force_with_lease(&self, _: &Path, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    // ── Mock GhOps ───────────────────────────────────────────────

    #[derive(Debug)]
    struct MockGhOps {
        pr_title: Option<String>,
    }

    impl MockGhOps {
        fn new(pr_title: Option<&str>) -> Self {
            Self {
                pr_title: pr_title.map(String::from),
            }
        }
    }

    impl GhOps for MockGhOps {
        fn pr_view_state(&self, _: u32) -> Result<Option<PrStatus>, CoreError> {
            Ok(Some(PrStatus {
                state: crate::gh::PrState::Open,
                number: 1,
                url: Some("https://github.com/org/repo/pull/1".to_string()),
            }))
        }
        fn pr_list_by_branch(&self, _: &str) -> Result<Option<PrStatus>, CoreError> {
            Ok(None)
        }
        fn pr_url_for_branch(&self, _: &str, _: &Path) -> Option<String> {
            Some("https://github.com/org/repo/pull/1".to_string())
        }
        fn pr_view_title(&self, _: u32, _: &Path) -> Option<String> {
            self.pr_title.clone()
        }
    }

    // ── Tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_should_delegate_diff_to_sync_git_ops() {
        let mock = MockGitOps::new("+added line\n-removed line");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        let result = async_git.diff(Path::new("/tmp"), "main").await;
        assert!(result.is_ok());
        assert!(result.as_ref().is_ok_and(|d| d.contains("+added line")));
    }

    #[tokio::test]
    async fn test_should_delegate_diff_name_only_to_sync_git_ops() {
        let mock = MockGitOps::new("");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        let files = async_git.diff_name_only(Path::new("/tmp"), "main").await;
        assert!(files.is_ok());
        assert!(files.is_ok_and(|f| f.len() == 1));
    }

    #[tokio::test]
    async fn test_should_delegate_log_oneline_to_sync_git_ops() {
        let mock = MockGitOps::new("");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        let log = async_git.log_oneline(Path::new("/tmp"), "main..HEAD").await;
        assert!(log.is_ok());
        assert!(log.as_ref().is_ok_and(|l| l.contains("abc1234")));
    }

    #[tokio::test]
    async fn test_should_delegate_merge_base_to_sync_git_ops() {
        let mock = MockGitOps::new("");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        let mb = async_git.merge_base(Path::new("/tmp"), "main").await;
        assert!(mb.is_ok_and(|b| b == "deadbeef"));
    }

    #[tokio::test]
    async fn test_should_delegate_detect_default_branch_to_sync_git_ops() {
        let mock = MockGitOps::new("");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        let branch = async_git.detect_default_branch().await;
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn test_should_delegate_file_exists_in_ref_to_sync_git_ops() {
        let mock = MockGitOps::new("");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        let exists = async_git
            .file_exists_in_ref("main", ".coda/config.yml")
            .await;
        assert!(exists);
    }

    #[tokio::test]
    async fn test_should_delegate_pr_view_state_to_sync_gh_ops() {
        let mock = MockGhOps::new(Some("feat: test"));
        let async_gh = AsyncGhOps::new(Arc::new(mock));

        let status = async_gh.pr_view_state(1).await;
        assert!(status.is_ok());
        let pr = status.as_ref().ok().and_then(|s| s.as_ref());
        assert_eq!(pr.map(|p| p.state), Some(crate::gh::PrState::Open));
    }

    #[tokio::test]
    async fn test_should_delegate_pr_url_for_branch_to_sync_gh_ops() {
        let mock = MockGhOps::new(None);
        let async_gh = AsyncGhOps::new(Arc::new(mock));

        let url = async_gh
            .pr_url_for_branch("feature/test", Path::new("/tmp"))
            .await;
        assert!(url.is_some());
        assert!(url.as_deref().is_some_and(|u| u.contains("/pull/")));
    }

    #[tokio::test]
    async fn test_should_delegate_pr_view_title_to_sync_gh_ops() {
        let mock = MockGhOps::new(Some("feat: my feature"));
        let async_gh = AsyncGhOps::new(Arc::new(mock));

        let title = async_gh.pr_view_title(1, Path::new("/tmp")).await;
        assert_eq!(title.as_deref(), Some("feat: my feature"));
    }

    #[tokio::test]
    async fn test_should_return_none_when_pr_title_not_available() {
        let mock = MockGhOps::new(None);
        let async_gh = AsyncGhOps::new(Arc::new(mock));

        let title = async_gh.pr_view_title(1, Path::new("/tmp")).await;
        assert!(title.is_none());
    }

    #[tokio::test]
    async fn test_should_expose_inner_sync_git_ops() {
        let mock = MockGitOps::new("test");
        let async_git = AsyncGitOps::new(Arc::new(mock));

        // Inner should be accessible for sync callers
        let inner = async_git.inner();
        let branch = inner.detect_default_branch();
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn test_should_expose_inner_sync_gh_ops() {
        let mock = MockGhOps::new(None);
        let async_gh = AsyncGhOps::new(Arc::new(mock));

        let inner = async_gh.inner();
        let result = inner.pr_list_by_branch("feature/x");
        assert!(result.is_ok());
    }
}
