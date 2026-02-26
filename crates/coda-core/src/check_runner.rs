//! Deterministic check runner for Tier 1 verification.
//!
//! Executes configured check commands (build, test, lint, format) as
//! real subprocesses via `tokio::process::Command`, capturing exit codes
//! and output. This eliminates reliance on AI self-reporting for
//! deterministic pass/fail results.
//!
//! # Architecture
//!
//! The runner executes checks sequentially with fail-fast semantics:
//! the first failing check stops execution. Each command's stdout/stderr
//! is captured and truncated to [`MAX_OUTPUT_BYTES`] to prevent memory
//! exhaustion from verbose test output.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use std::path::Path;
//! use coda_core::check_runner::run_checks;
//!
//! let checks = vec!["cargo build".to_string(), "cargo test".to_string()];
//! let result = run_checks(Path::new("/tmp/project"), &checks, 600).await?;
//! if result.all_passed() {
//!     println!("All {} checks passed!", result.total_count());
//! } else {
//!     for detail in result.failed_details() {
//!         eprintln!("FAILED: {detail}");
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::path::Path;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tracing::{debug, info};

use crate::CoreError;

/// Maximum bytes captured per check command's combined stdout+stderr.
///
/// Prevents memory exhaustion from verbose test runners or build logs.
const MAX_OUTPUT_BYTES: usize = 50 * 1024; // 50 KB

/// Result of a single check command execution.
///
/// # Example
///
/// ```
/// use coda_core::check_runner::CheckResult;
/// use std::time::Duration;
///
/// let result = CheckResult {
///     command: "cargo build".to_string(),
///     passed: true,
///     exit_code: Some(0),
///     stdout: String::new(),
///     stderr: String::new(),
///     duration: Duration::from_secs(12),
/// };
/// assert!(result.passed);
/// ```
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// The command string that was executed.
    pub command: String,
    /// Whether the check passed (exit code 0).
    pub passed: bool,
    /// Process exit code, `None` if killed by signal or timeout.
    pub exit_code: Option<i32>,
    /// Captured stdout (truncated to [`MAX_OUTPUT_BYTES`]).
    pub stdout: String,
    /// Captured stderr (truncated to [`MAX_OUTPUT_BYTES`]).
    pub stderr: String,
    /// Wall-clock duration of the check execution.
    pub duration: Duration,
}

/// Aggregated result of running all configured checks.
///
/// # Example
///
/// ```
/// use coda_core::check_runner::CheckRunResult;
/// use std::time::Duration;
///
/// let result = CheckRunResult {
///     checks: vec![],
///     total_duration: Duration::ZERO,
/// };
/// assert!(result.all_passed());
/// assert_eq!(result.total_count(), 0);
/// ```
#[derive(Debug, Clone)]
pub struct CheckRunResult {
    /// Individual results for each check that was executed.
    ///
    /// Due to fail-fast semantics, this may contain fewer entries
    /// than the total number of configured checks.
    pub checks: Vec<CheckResult>,
    /// Total wall-clock duration across all executed checks.
    pub total_duration: Duration,
}

impl CheckRunResult {
    /// Returns `true` if all executed checks passed.
    pub fn all_passed(&self) -> bool {
        self.checks.iter().all(|c| c.passed)
    }

    /// Returns the number of checks that passed.
    pub fn passed_count(&self) -> u32 {
        self.checks.iter().filter(|c| c.passed).count() as u32
    }

    /// Returns the total number of checks that were executed.
    pub fn total_count(&self) -> u32 {
        self.checks.len() as u32
    }

    /// Returns formatted failure details for each failed check.
    ///
    /// Each entry includes the command name, exit code, and captured
    /// stderr output for diagnostic context.
    pub fn failed_details(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| {
                let exit_info = c
                    .exit_code
                    .map_or("killed/timeout".to_string(), |code| format!("exit {code}"));

                let output = if !c.stderr.is_empty() {
                    c.stderr.clone()
                } else if !c.stdout.is_empty() {
                    c.stdout.clone()
                } else {
                    "(no output captured)".to_string()
                };

                format!(
                    "### `{}`\n\n**Status**: {exit_info}\n\n```\n{output}\n```",
                    c.command
                )
            })
            .collect()
    }
}

/// Executes check commands sequentially with fail-fast semantics.
///
/// Each command is run via `sh -c` in the specified working directory.
/// Execution stops at the first failure. Output is truncated to
/// [`MAX_OUTPUT_BYTES`] per command.
///
/// # Arguments
///
/// * `cwd` - Working directory for command execution (typically the worktree).
/// * `checks` - Ordered list of shell commands to execute.
/// * `timeout_secs` - Per-command timeout in seconds.
///
/// # Errors
///
/// Returns `CoreError::IoError` if a command cannot be spawned.
pub async fn run_checks(
    cwd: &Path,
    checks: &[String],
    timeout_secs: u64,
) -> Result<CheckRunResult, CoreError> {
    let overall_start = Instant::now();
    let mut results = Vec::with_capacity(checks.len());

    for check in checks {
        let start = Instant::now();
        info!(command = %check, "Running check");

        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            Command::new("sh")
                .arg("-c")
                .arg(check)
                .current_dir(cwd)
                .output(),
        )
        .await;

        let result = match output {
            Ok(Ok(output)) => {
                let exit_code = output.status.code();
                let passed = output.status.success();
                let stdout = truncate_output(&output.stdout);
                let stderr = truncate_output(&output.stderr);
                let duration = start.elapsed();

                debug!(
                    command = %check,
                    passed,
                    exit_code = ?exit_code,
                    stdout_len = stdout.len(),
                    stderr_len = stderr.len(),
                    duration_ms = duration.as_millis(),
                    "Check completed",
                );

                CheckResult {
                    command: check.clone(),
                    passed,
                    exit_code,
                    stdout,
                    stderr,
                    duration,
                }
            }
            Ok(Err(e)) => {
                return Err(CoreError::IoError(e));
            }
            Err(_elapsed) => {
                info!(command = %check, timeout_secs, "Check timed out");
                CheckResult {
                    command: check.clone(),
                    passed: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Command timed out after {timeout_secs}s"),
                    duration: start.elapsed(),
                }
            }
        };

        let failed = !result.passed;
        results.push(result);

        // Fail-fast: stop at first failure
        if failed {
            info!(command = %check, "Check failed, stopping (fail-fast)");
            break;
        }
    }

    Ok(CheckRunResult {
        checks: results,
        total_duration: overall_start.elapsed(),
    })
}

/// Converts raw bytes to a UTF-8 string, truncating at [`MAX_OUTPUT_BYTES`].
///
/// Uses lossy conversion to handle non-UTF-8 output from build tools.
fn truncate_output(bytes: &[u8]) -> String {
    let limited = if bytes.len() > MAX_OUTPUT_BYTES {
        &bytes[..MAX_OUTPUT_BYTES]
    } else {
        bytes
    };
    let mut s = String::from_utf8_lossy(limited).into_owned();
    if bytes.len() > MAX_OUTPUT_BYTES {
        s.push_str("\n... [output truncated]");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_report_all_passed_for_empty_checks() {
        let result = CheckRunResult {
            checks: vec![],
            total_duration: Duration::ZERO,
        };
        assert!(result.all_passed());
        assert_eq!(result.passed_count(), 0);
        assert_eq!(result.total_count(), 0);
        assert!(result.failed_details().is_empty());
    }

    #[test]
    fn test_should_count_passed_and_failed() {
        let result = CheckRunResult {
            checks: vec![
                CheckResult {
                    command: "cargo build".to_string(),
                    passed: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                    duration: Duration::from_secs(1),
                },
                CheckResult {
                    command: "cargo test".to_string(),
                    passed: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "test failed".to_string(),
                    duration: Duration::from_secs(2),
                },
            ],
            total_duration: Duration::from_secs(3),
        };
        assert!(!result.all_passed());
        assert_eq!(result.passed_count(), 1);
        assert_eq!(result.total_count(), 2);

        let details = result.failed_details();
        assert_eq!(details.len(), 1);
        assert!(details[0].contains("cargo test"));
        assert!(details[0].contains("test failed"));
    }

    #[test]
    fn test_should_format_timeout_failure_details() {
        let result = CheckRunResult {
            checks: vec![CheckResult {
                command: "cargo build".to_string(),
                passed: false,
                exit_code: None,
                stdout: String::new(),
                stderr: "Command timed out after 600s".to_string(),
                duration: Duration::from_secs(600),
            }],
            total_duration: Duration::from_secs(600),
        };
        let details = result.failed_details();
        assert_eq!(details.len(), 1);
        assert!(details[0].contains("killed/timeout"));
        assert!(details[0].contains("timed out"));
    }

    #[test]
    fn test_should_truncate_long_output() {
        let long_bytes = vec![b'x'; MAX_OUTPUT_BYTES + 100];
        let truncated = truncate_output(&long_bytes);
        assert!(truncated.len() <= MAX_OUTPUT_BYTES + 30); // + truncation message
        assert!(truncated.contains("... [output truncated]"));
    }

    #[test]
    fn test_should_preserve_short_output() {
        let short = b"hello world";
        let result = truncate_output(short);
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn test_should_run_passing_check() {
        let result = run_checks(Path::new("/tmp"), &["true".to_string()], 10)
            .await
            .unwrap();
        assert!(result.all_passed());
        assert_eq!(result.total_count(), 1);
        assert_eq!(result.passed_count(), 1);
    }

    #[tokio::test]
    async fn test_should_run_failing_check() {
        let result = run_checks(Path::new("/tmp"), &["false".to_string()], 10)
            .await
            .unwrap();
        assert!(!result.all_passed());
        assert_eq!(result.total_count(), 1);
        assert_eq!(result.passed_count(), 0);
    }

    #[tokio::test]
    async fn test_should_fail_fast_on_first_failure() {
        let checks = vec![
            "true".to_string(),
            "false".to_string(),
            "echo should-not-run".to_string(),
        ];
        let result = run_checks(Path::new("/tmp"), &checks, 10).await.unwrap();
        // Should only execute 2 checks (true passes, false fails, third skipped)
        assert_eq!(result.total_count(), 2);
        assert!(!result.all_passed());
    }

    #[tokio::test]
    async fn test_should_capture_stdout_and_stderr() {
        let checks = vec!["echo hello && echo error >&2".to_string()];
        let result = run_checks(Path::new("/tmp"), &checks, 10).await.unwrap();
        assert!(result.all_passed());
        assert!(result.checks[0].stdout.contains("hello"));
        assert!(result.checks[0].stderr.contains("error"));
    }

    #[tokio::test]
    async fn test_should_handle_timeout() {
        let checks = vec!["sleep 30".to_string()];
        let result = run_checks(Path::new("/tmp"), &checks, 1).await.unwrap();
        assert!(!result.all_passed());
        assert!(result.checks[0].exit_code.is_none());
        assert!(result.checks[0].stderr.contains("timed out"));
    }

    #[tokio::test]
    async fn test_should_return_empty_for_no_checks() {
        let result = run_checks(Path::new("/tmp"), &[], 10).await.unwrap();
        assert!(result.all_passed());
        assert_eq!(result.total_count(), 0);
    }
}
