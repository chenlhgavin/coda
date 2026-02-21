//! Codex CLI integration for independent code review.
//!
//! Provides a wrapper around the `codex` CLI tool to perform read-only
//! code reviews using a different LLM (e.g., GPT-5.3 Codex). Issues from
//! Codex reviews can be combined with Claude's reviews for a hybrid
//! review that catches more problems.
//!
//! # Architecture
//!
//! - [`ReviewIssue`] is the normalized issue representation shared by both engines
//! - [`run_codex_review`] spawns `codex exec` as a subprocess and parses its output
//! - [`deduplicate_issues`] merges overlapping issues from multiple reviewers
//! - Falls back gracefully when `codex` is not installed

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::CoreError;
use crate::runner::RunEvent;

/// Source of a review issue — which reviewer found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSource {
    /// Found by Claude (self-review).
    Claude,
    /// Found by Codex CLI (independent review).
    Codex,
    /// Found by both reviewers (merged duplicate).
    Both,
}

impl std::fmt::Display for ReviewSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Claude => write!(f, "Claude"),
            Self::Codex => write!(f, "Codex"),
            Self::Both => write!(f, "Claude+Codex"),
        }
    }
}

/// A normalized code review issue from any review engine.
///
/// Shared representation that allows issues from Claude and Codex to be
/// compared, merged, and presented uniformly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    /// Severity level: `"critical"` or `"major"`.
    pub severity: String,
    /// File path where the issue was found.
    pub file: String,
    /// Description of the problem.
    pub description: String,
    /// Suggested fix.
    pub suggestion: String,
    /// Which reviewer found this issue.
    pub source: ReviewSource,
}

/// Checks whether the `codex` CLI binary is available on PATH.
///
/// # Examples
///
/// ```
/// use coda_core::codex::is_codex_available;
///
/// // Returns true if `codex` is installed and on PATH
/// let available = is_codex_available();
/// ```
pub fn is_codex_available() -> bool {
    std::process::Command::new("which")
        .arg("codex")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs a Codex CLI review on the given diff.
///
/// Spawns `codex exec --full-auto --sandbox read-only --json` with a review
/// prompt piped via stdin. The `--json` flag suppresses TTY output and emits
/// JSONL events on stdout. Agent message text is extracted from
/// `item.completed` events in real time and streamed to the TUI via
/// `progress_tx` as [`RunEvent::AgentTextDelta`] events.
///
/// The `reasoning_effort` parameter controls the model's reasoning depth
/// and is passed via `-c model_reasoning_effort=<value>`. Valid values:
/// `"minimal"`, `"low"`, `"medium"`, `"high"`, `"xhigh"`.
///
/// # Errors
///
/// Returns `CoreError::AgentError` if:
/// - The `codex` process cannot be spawned
/// - The process exits with a non-zero status
/// - The output cannot be parsed
pub async fn run_codex_review(
    worktree: &Path,
    diff: &str,
    design_spec: &str,
    model: &str,
    reasoning_effort: &str,
    progress_tx: Option<&UnboundedSender<RunEvent>>,
) -> Result<Vec<ReviewIssue>, CoreError> {
    let prompt = build_codex_review_prompt(diff, design_spec);

    info!(
        model = model,
        reasoning_effort = reasoning_effort,
        worktree = %worktree.display(),
        "Starting Codex review",
    );

    let effort_config = format!("model_reasoning_effort=\"{reasoning_effort}\"");

    // Pass the prompt via stdin (with `-`) to avoid E2BIG (os error 7) when
    // the diff + design spec exceeds the OS execve() argument-list limit.
    // Use `--json` to suppress TTY output so codex doesn't corrupt CODA's TUI.
    let mut child = tokio::process::Command::new("codex")
        .args([
            "exec",
            "--full-auto",
            "--sandbox",
            "read-only",
            "--json",
            "--model",
            model,
            "-c",
            &effort_config,
            "-C",
            &worktree.to_string_lossy(),
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| CoreError::AgentError(format!("Failed to spawn codex: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| CoreError::AgentError(format!("Failed to write prompt to codex: {e}")))?;
        // Drop stdin to signal EOF so codex starts processing.
    }

    // Stream stdout line-by-line, extracting agent messages in real time.
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::AgentError("Failed to capture codex stdout".to_string()))?;
    let mut lines = BufReader::new(stdout_pipe).lines();
    let mut agent_texts: Vec<String> = Vec::new();

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| CoreError::AgentError(format!("Failed to read codex output: {e}")))?
    {
        if let Some(text) = extract_agent_text_from_jsonl_event(&line) {
            if let Some(tx) = progress_tx {
                let _ = tx.send(RunEvent::AgentTextDelta { text: text.clone() });
            }
            agent_texts.push(text);
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| CoreError::AgentError(format!("Failed to wait for codex: {e}")))?;

    if !status.success() {
        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use tokio::io::AsyncReadExt;
            let _ = stderr.read_to_string(&mut stderr_buf).await;
        }
        warn!(
            status = ?status,
            stderr = %stderr_buf,
            "Codex review failed",
        );
        return Err(CoreError::AgentError(format!(
            "Codex exited with {status}: {stderr_buf}",
        )));
    }

    let agent_text = agent_texts.join("\n");
    debug!(agent_text_len = agent_text.len(), "Codex review completed");

    let issues = parse_review_issues_structured(&agent_text, ReviewSource::Codex);

    info!(issues = issues.len(), "Codex review found issues");
    Ok(issues)
}

/// Parses review issues from a YAML response into structured [`ReviewIssue`]s.
///
/// Extracts a YAML code block and parses the `issues` array, filtering for
/// `critical` and `major` severity only.
///
/// # Examples
///
/// ```
/// use coda_core::codex::{parse_review_issues_structured, ReviewSource};
///
/// let response = "```yaml\nissues:\n  - severity: critical\n    file: src/main.rs\n    description: unwrap used\n    suggestion: use ? operator\n```";
/// let issues = parse_review_issues_structured(response, ReviewSource::Claude);
/// assert_eq!(issues.len(), 1);
/// assert_eq!(issues[0].severity, "critical");
/// ```
pub fn parse_review_issues_structured(response: &str, source: ReviewSource) -> Vec<ReviewIssue> {
    let yaml_content = crate::parser::extract_yaml_block(response);

    let Some(yaml) = yaml_content else {
        return Vec::new();
    };

    let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&yaml) else {
        return Vec::new();
    };

    let Some(issues) = parsed.get("issues").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    issues
        .iter()
        .filter_map(|issue| {
            let severity = issue.get("severity")?.as_str()?;
            if severity != "critical" && severity != "major" {
                return None;
            }
            Some(ReviewIssue {
                severity: severity.to_string(),
                file: issue
                    .get("file")
                    .and_then(|f| f.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                description: issue
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("Unknown issue")
                    .to_string(),
                suggestion: issue
                    .get("suggestion")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: source.clone(),
            })
        })
        .collect()
}

/// Deduplicates review issues from multiple sources.
///
/// Groups issues by file and compares descriptions using word-overlap
/// (Jaccard similarity). When similarity exceeds 0.6, keeps the issue
/// with the longer (more detailed) description and marks it as
/// [`ReviewSource::Both`].
///
/// # Examples
///
/// ```
/// use coda_core::codex::{ReviewIssue, ReviewSource, deduplicate_issues};
///
/// let issues = vec![
///     ReviewIssue {
///         severity: "critical".into(),
///         file: "src/main.rs".into(),
///         description: "unwrap used in production code".into(),
///         suggestion: "use ? operator".into(),
///         source: ReviewSource::Claude,
///     },
///     ReviewIssue {
///         severity: "critical".into(),
///         file: "src/main.rs".into(),
///         description: "unwrap used in production code path".into(),
///         suggestion: "replace with ? operator instead".into(),
///         source: ReviewSource::Codex,
///     },
/// ];
///
/// let deduped = deduplicate_issues(issues);
/// assert_eq!(deduped.len(), 1);
/// assert_eq!(deduped[0].source, ReviewSource::Both);
/// ```
pub fn deduplicate_issues(issues: Vec<ReviewIssue>) -> Vec<ReviewIssue> {
    if issues.is_empty() {
        return issues;
    }

    // Group by file
    let mut by_file: std::collections::HashMap<String, Vec<ReviewIssue>> =
        std::collections::HashMap::new();
    for issue in issues {
        by_file.entry(issue.file.clone()).or_default().push(issue);
    }

    let mut result = Vec::new();

    for file_issues in by_file.values() {
        let mut merged = Vec::new();
        let mut consumed: HashSet<usize> = HashSet::new();

        for i in 0..file_issues.len() {
            if consumed.contains(&i) {
                continue;
            }

            let mut current = file_issues[i].clone();

            for (j, candidate) in file_issues.iter().enumerate().skip(i + 1) {
                if consumed.contains(&j) {
                    continue;
                }

                let similarity = jaccard_similarity(&current.description, &candidate.description);
                if similarity > 0.6 {
                    consumed.insert(j);
                    // Keep the longer description
                    if candidate.description.len() > current.description.len() {
                        current.description = candidate.description.clone();
                        current.suggestion = candidate.suggestion.clone();
                    }
                    // Mark as found by both
                    if current.source != candidate.source {
                        current.source = ReviewSource::Both;
                    }
                }
            }

            merged.push(current);
        }

        result.extend(merged);
    }

    result
}

/// Formats structured review issues into the string format expected by
/// the fix prompt.
///
/// Produces lines like:
/// `[critical] src/main.rs: unwrap used. Suggestion: use ? operator [Codex]`
///
/// # Examples
///
/// ```
/// use coda_core::codex::{ReviewIssue, ReviewSource, format_issues};
///
/// let issues = vec![ReviewIssue {
///     severity: "critical".into(),
///     file: "src/main.rs".into(),
///     description: "unwrap used".into(),
///     suggestion: "use ?".into(),
///     source: ReviewSource::Codex,
/// }];
///
/// let formatted = format_issues(&issues);
/// assert_eq!(formatted.len(), 1);
/// assert!(formatted[0].contains("[critical]"));
/// assert!(formatted[0].contains("[Codex]"));
/// ```
pub fn format_issues(issues: &[ReviewIssue]) -> Vec<String> {
    issues
        .iter()
        .map(|issue| {
            format!(
                "[{}] {}: {}. Suggestion: {} [{}]",
                issue.severity, issue.file, issue.description, issue.suggestion, issue.source,
            )
        })
        .collect()
}

/// Computes Jaccard similarity between two strings based on word overlap.
///
/// Returns a value between 0.0 (no overlap) and 1.0 (identical word sets).
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let words_a: HashSet<&str> = a.split_whitespace().collect();
    let words_b: HashSet<&str> = b.split_whitespace().collect();

    if words_a.is_empty() && words_b.is_empty() {
        return 1.0;
    }

    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();

    if union == 0 {
        return 0.0;
    }

    intersection as f64 / union as f64
}

/// Extracts agent message text from a single JSONL event line.
///
/// Returns `Some(text)` if the line is an `item.completed` event with
/// `item.type == "agent_message"`, otherwise `None`.
fn extract_agent_text_from_jsonl_event(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let event: serde_json::Value = serde_json::from_str(line).ok()?;
    if event.get("type")?.as_str()? != "item.completed" {
        return None;
    }
    let item = event.get("item")?;
    if item.get("type")?.as_str()? != "agent_message" {
        return None;
    }
    item.get("text")?.as_str().map(String::from)
}

/// Extracts agent message text from `codex exec --json` JSONL output.
///
/// Scans for `item.completed` events where `item.type` is `"agent_message"`
/// and concatenates their `item.text` fields.
///
/// # Examples
///
/// ```
/// use coda_core::codex::extract_agent_message_from_jsonl;
///
/// let jsonl = r#"{"type":"thread.started","thread_id":"abc"}
/// {"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hello"}}
/// {"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#;
///
/// assert_eq!(extract_agent_message_from_jsonl(jsonl), "hello");
/// ```
pub fn extract_agent_message_from_jsonl(jsonl: &str) -> String {
    jsonl
        .lines()
        .filter_map(extract_agent_text_from_jsonl_event)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Builds the review prompt for the Codex CLI.
///
/// Contains the diff, design spec, and output format instructions
/// matching the same YAML schema used by Claude's review prompt.
fn build_codex_review_prompt(diff: &str, design_spec: &str) -> String {
    format!(
        r#"You are a senior code reviewer. Review the following changes for a new feature.

## Design Specification

{design_spec}

## Code Changes (diff)

```diff
{diff}
```

## Task

Perform a thorough code review focusing on:
1. Correctness — Does the code match the design spec?
2. Error Handling — Are all error cases handled properly?
3. Security — Any input validation issues or sensitive data exposure?
4. Performance — Unnecessary allocations or blocking in async?
5. API Design — Are public interfaces clean and consistent?
6. Testing — Are critical paths tested?

## Output Format

Respond with a YAML block listing only **critical** and **major** issues:

```yaml
issues:
  - severity: "critical"
    file: "path/to/file.rs"
    description: "Brief description of the issue"
    suggestion: "How to fix it"
```

If there are no critical or major issues, respond with:

```yaml
issues: []
```

Be precise and avoid false positives. Only report issues that are objectively problematic."#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_parse_structured_issues_from_yaml() {
        let response = r#"
Here are the review findings:

```yaml
issues:
  - severity: "critical"
    file: "src/main.rs"
    line: 42
    description: "Use of unwrap in production code"
    suggestion: "Replace with ? operator"
  - severity: "minor"
    file: "src/lib.rs"
    line: 10
    description: "Missing doc comment"
    suggestion: "Add /// documentation"
  - severity: "major"
    file: "src/db.rs"
    line: 100
    description: "SQL injection vulnerability"
    suggestion: "Use parameterized queries"
```
"#;

        let issues = parse_review_issues_structured(response, ReviewSource::Codex);
        assert_eq!(issues.len(), 2); // Only critical + major
        assert_eq!(issues[0].severity, "critical");
        assert_eq!(issues[0].file, "src/main.rs");
        assert_eq!(issues[0].source, ReviewSource::Codex);
        assert_eq!(issues[1].severity, "major");
        assert_eq!(issues[1].file, "src/db.rs");
    }

    #[test]
    fn test_should_return_empty_for_no_issues() {
        let response = "```yaml\nissues: []\n```";
        let issues = parse_review_issues_structured(response, ReviewSource::Claude);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_should_return_empty_for_no_yaml_block() {
        let response = "The code looks great! No issues found.";
        let issues = parse_review_issues_structured(response, ReviewSource::Claude);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_should_deduplicate_overlapping_issues() {
        let issues = vec![
            ReviewIssue {
                severity: "critical".into(),
                file: "src/main.rs".into(),
                description: "unwrap used in production code".into(),
                suggestion: "use ? operator".into(),
                source: ReviewSource::Claude,
            },
            ReviewIssue {
                severity: "critical".into(),
                file: "src/main.rs".into(),
                description: "unwrap used in production code path is dangerous".into(),
                suggestion: "replace with ? operator instead".into(),
                source: ReviewSource::Codex,
            },
        ];

        let deduped = deduplicate_issues(issues);
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].source, ReviewSource::Both);
        // Should keep the longer description
        assert!(deduped[0].description.contains("dangerous"));
    }

    #[test]
    fn test_should_keep_distinct_issues() {
        let issues = vec![
            ReviewIssue {
                severity: "critical".into(),
                file: "src/main.rs".into(),
                description: "unwrap used in production code".into(),
                suggestion: "use ? operator".into(),
                source: ReviewSource::Claude,
            },
            ReviewIssue {
                severity: "major".into(),
                file: "src/main.rs".into(),
                description: "missing error handling for network timeout".into(),
                suggestion: "add timeout configuration".into(),
                source: ReviewSource::Codex,
            },
        ];

        let deduped = deduplicate_issues(issues);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_should_keep_issues_in_different_files() {
        let issues = vec![
            ReviewIssue {
                severity: "critical".into(),
                file: "src/main.rs".into(),
                description: "unwrap used in production code".into(),
                suggestion: "use ? operator".into(),
                source: ReviewSource::Claude,
            },
            ReviewIssue {
                severity: "critical".into(),
                file: "src/lib.rs".into(),
                description: "unwrap used in production code".into(),
                suggestion: "use ? operator".into(),
                source: ReviewSource::Codex,
            },
        ];

        let deduped = deduplicate_issues(issues);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_should_handle_empty_issues() {
        let deduped = deduplicate_issues(vec![]);
        assert!(deduped.is_empty());
    }

    #[test]
    fn test_should_format_issues_with_source() {
        let issues = vec![
            ReviewIssue {
                severity: "critical".into(),
                file: "src/main.rs".into(),
                description: "unwrap used".into(),
                suggestion: "use ?".into(),
                source: ReviewSource::Codex,
            },
            ReviewIssue {
                severity: "major".into(),
                file: "src/lib.rs".into(),
                description: "missing docs".into(),
                suggestion: "add docs".into(),
                source: ReviewSource::Both,
            },
        ];

        let formatted = format_issues(&issues);
        assert_eq!(formatted.len(), 2);
        assert_eq!(
            formatted[0],
            "[critical] src/main.rs: unwrap used. Suggestion: use ? [Codex]",
        );
        assert_eq!(
            formatted[1],
            "[major] src/lib.rs: missing docs. Suggestion: add docs [Claude+Codex]",
        );
    }

    #[test]
    fn test_should_extract_agent_message_from_jsonl() {
        let jsonl = r#"{"type":"thread.started","thread_id":"019c7fd5"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"```yaml\nissues:\n  - severity: critical\n    file: src/main.rs\n    description: unwrap used\n    suggestion: use ?\n```"}}
{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":50}}"#;

        let text = extract_agent_message_from_jsonl(jsonl);
        assert!(text.contains("issues:"));
        assert!(text.contains("critical"));
    }

    #[test]
    fn test_should_return_empty_for_no_agent_message() {
        let jsonl = r#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#;

        assert!(extract_agent_message_from_jsonl(jsonl).is_empty());
    }

    #[test]
    fn test_should_concatenate_multiple_agent_messages() {
        let jsonl = r#"{"type":"item.completed","item":{"id":"0","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"second"}}"#;

        assert_eq!(extract_agent_message_from_jsonl(jsonl), "first\nsecond");
    }

    #[test]
    fn test_should_skip_non_agent_message_items() {
        let jsonl = r#"{"type":"item.completed","item":{"id":"0","type":"tool_call","text":"ignored"}}
{"type":"item.completed","item":{"id":"1","type":"agent_message","text":"kept"}}"#;

        assert_eq!(extract_agent_message_from_jsonl(jsonl), "kept");
    }

    #[test]
    fn test_should_compute_jaccard_similarity_identical() {
        let sim = jaccard_similarity("hello world", "hello world");
        assert!((sim - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_compute_jaccard_similarity_no_overlap() {
        let sim = jaccard_similarity("hello world", "foo bar");
        assert!((sim - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_should_compute_jaccard_similarity_partial() {
        // "unwrap used in production code" vs "unwrap used in production code path"
        // words_a: {unwrap, used, in, production, code} = 5
        // words_b: {unwrap, used, in, production, code, path} = 6
        // intersection = 5, union = 6
        // similarity = 5/6 ≈ 0.833
        let sim = jaccard_similarity(
            "unwrap used in production code",
            "unwrap used in production code path",
        );
        assert!(sim > 0.8);
    }

    #[test]
    fn test_should_handle_empty_strings_in_jaccard() {
        assert!((jaccard_similarity("", "") - 1.0).abs() < f64::EPSILON);
        assert!((jaccard_similarity("hello", "") - 0.0).abs() < f64::EPSILON);
    }
}
