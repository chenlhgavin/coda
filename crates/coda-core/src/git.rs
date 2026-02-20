//! Git operations abstraction.
//!
//! Defines the [`GitOps`] trait for all git CLI interactions and provides
//! [`DefaultGitOps`], the production implementation that shells out to `git`.
//! This abstraction enables unit-testing modules that depend on git without
//! requiring an actual repository.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::CoreError;

/// Abstraction over git CLI operations.
///
/// All path-taking methods expect **absolute** paths unless noted otherwise.
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks.
pub trait GitOps: Send + Sync {
    /// Creates a worktree at `path` on a new branch forked from `base`.
    ///
    /// Equivalent to `git worktree add <path> -b <branch> <base>`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn worktree_add(&self, path: &Path, branch: &str, base: &str) -> Result<(), CoreError>;

    /// Removes a worktree.
    ///
    /// Equivalent to `git worktree remove <path> [--force]`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn worktree_remove(&self, path: &Path, force: bool) -> Result<(), CoreError>;

    /// Prunes stale worktree bookkeeping entries.
    ///
    /// Equivalent to `git worktree prune`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn worktree_prune(&self) -> Result<(), CoreError>;

    /// Deletes a local branch.
    ///
    /// Equivalent to `git branch -D <branch>`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn branch_delete(&self, branch: &str) -> Result<(), CoreError>;

    /// Stages files.
    ///
    /// Equivalent to `git add <paths...>` run inside `cwd`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn add(&self, cwd: &Path, paths: &[&str]) -> Result<(), CoreError>;

    /// Returns whether the staging area has changes.
    ///
    /// Equivalent to `git diff --cached --quiet` (returns `true` when dirty).
    fn has_staged_changes(&self, cwd: &Path) -> bool;

    /// Creates a commit with the given message.
    ///
    /// Equivalent to `git commit -m <message>` run inside `cwd`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn commit(&self, cwd: &Path, message: &str) -> Result<(), CoreError>;

    /// Returns the diff between `base` and HEAD.
    ///
    /// Equivalent to `git diff <base> HEAD` run inside `cwd`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn diff(&self, cwd: &Path, base: &str) -> Result<String, CoreError>;

    /// Returns one-line log entries for commits in `range`.
    ///
    /// Equivalent to `git log <range> --oneline --no-decorate` inside `cwd`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn log_oneline(&self, cwd: &Path, range: &str) -> Result<String, CoreError>;

    /// Pushes the current branch to origin.
    ///
    /// Equivalent to `git push origin <branch>` run inside `cwd`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::GitError` if the command fails.
    fn push(&self, cwd: &Path, branch: &str) -> Result<(), CoreError>;

    /// Checks whether a file exists in a given git ref (branch, tag, commit).
    ///
    /// Equivalent to `git cat-file -e <ref>:<path>`.
    fn file_exists_in_ref(&self, git_ref: &str, path: &str) -> bool;

    /// Detects the repository's default branch.
    ///
    /// Queries `git symbolic-ref refs/remotes/origin/HEAD --short` and
    /// strips the `origin/` prefix. Falls back to `"main"`.
    fn detect_default_branch(&self) -> String;
}

/// Production [`GitOps`] implementation that shells out to the `git` binary.
#[derive(Debug)]
pub struct DefaultGitOps {
    project_root: PathBuf,
}

impl DefaultGitOps {
    /// Creates a new instance rooted at `project_root`.
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

impl GitOps for DefaultGitOps {
    fn worktree_add(&self, path: &Path, branch: &str, base: &str) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::IoError)?;
        }
        run_git(
            &self.project_root,
            &[
                "worktree",
                "add",
                &path.display().to_string(),
                "-b",
                branch,
                base,
            ],
        )?;
        Ok(())
    }

    fn worktree_remove(&self, path: &Path, force: bool) -> Result<(), CoreError> {
        let path_str = path.display().to_string();
        let mut args = vec!["worktree", "remove", &path_str];
        if force {
            args.push("--force");
        }
        run_git(&self.project_root, &args)?;
        Ok(())
    }

    fn worktree_prune(&self) -> Result<(), CoreError> {
        run_git(&self.project_root, &["worktree", "prune"])?;
        Ok(())
    }

    fn branch_delete(&self, branch: &str) -> Result<(), CoreError> {
        run_git(&self.project_root, &["branch", "-D", branch])?;
        Ok(())
    }

    fn add(&self, cwd: &Path, paths: &[&str]) -> Result<(), CoreError> {
        let mut args = vec!["add"];
        args.extend(paths);
        run_git(cwd, &args)?;
        Ok(())
    }

    fn has_staged_changes(&self, cwd: &Path) -> bool {
        run_git(cwd, &["diff", "--cached", "--quiet"]).is_err()
    }

    fn commit(&self, cwd: &Path, message: &str) -> Result<(), CoreError> {
        run_git(cwd, &["commit", "-m", message])?;
        Ok(())
    }

    fn diff(&self, cwd: &Path, base: &str) -> Result<String, CoreError> {
        run_git(cwd, &["diff", base, "HEAD"])
    }

    fn log_oneline(&self, cwd: &Path, range: &str) -> Result<String, CoreError> {
        run_git(cwd, &["log", range, "--oneline", "--no-decorate"])
    }

    fn push(&self, cwd: &Path, branch: &str) -> Result<(), CoreError> {
        run_git(cwd, &["push", "origin", branch])?;
        Ok(())
    }

    fn file_exists_in_ref(&self, git_ref: &str, path: &str) -> bool {
        std::process::Command::new("git")
            .args(["cat-file", "-e", &format!("{git_ref}:{path}")])
            .current_dir(&self.project_root)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn detect_default_branch(&self) -> String {
        let output = std::process::Command::new("git")
            .args(["symbolic-ref", "refs/remotes/origin/HEAD", "--short"])
            .current_dir(&self.project_root)
            .output();

        if let Ok(output) = output
            && output.status.success()
        {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(name) = branch.strip_prefix("origin/") {
                return name.to_string();
            }
            return branch;
        }

        "main".to_string()
    }
}

/// Runs a git command and returns its stdout.
fn run_git(cwd: &Path, args: &[&str]) -> Result<String, CoreError> {
    debug!(cwd = %cwd.display(), args = ?args, "git");
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(CoreError::IoError)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CoreError::GitError(format!(
            "git {} failed: {stderr}",
            args.join(" "),
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
