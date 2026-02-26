//! Response parsing utilities for agent output.
//!
//! Pure functions for extracting structured data (YAML blocks, PR URLs,
//! review issues, verification results) from agent text responses.
//! These are decoupled from the [`Runner`](crate::runner::Runner) to
//! keep parsing logic independently testable.

/// Extracts a YAML code block from a response string.
///
/// Tries the following strategies in order:
/// 1. Fenced `` ```yaml ... ``` `` block
/// 2. Fenced `` ``` ... ``` `` block whose content starts with `issues:` or `result:`
/// 3. Returns `None` if no recognizable block is found
///
/// # Examples
///
/// ```
/// use coda_core::parser::extract_yaml_block;
///
/// let text = "Some text\n```yaml\nissues: []\n```\nMore text";
/// assert_eq!(extract_yaml_block(text), Some("issues: []".to_string()));
/// ```
pub fn extract_yaml_block(text: &str) -> Option<String> {
    // Strategy 1: ```yaml ... ``` block
    if let Some(start) = text.find("```yaml") {
        let content_start = start + "```yaml".len();
        if let Some(end) = text[content_start..].find("```") {
            return Some(text[content_start..content_start + end].trim().to_string());
        }
    }

    // Strategy 2: unmarked ``` ... ``` block that looks like YAML
    if let Some(start) = text.find("```\n") {
        let content_start = start + "```\n".len();
        if let Some(end) = text[content_start..].find("```") {
            let content = text[content_start..content_start + end].trim();
            if content.starts_with("issues:") || content.starts_with("result:") {
                return Some(content.to_string());
            }
        }
    }

    None
}

/// Returns `true` if the given severity string represents an actionable
/// review issue (critical or major), using case-insensitive comparison.
fn is_actionable_severity(severity: &str) -> bool {
    let lower = severity.to_lowercase();
    lower == "critical" || lower == "major"
}

/// Parses review issues from the agent's YAML response.
///
/// Returns a list of issue descriptions for critical/major issues only.
/// Minor and informational severity issues are filtered out. Severity
/// matching is case-insensitive (e.g., `Critical`, `MAJOR` are accepted).
///
/// # Examples
///
/// ```
/// use coda_core::parser::parse_review_issues;
///
/// let response = "```yaml\nissues:\n  - severity: critical\n    file: src/main.rs\n    description: unwrap used\n    suggestion: use ? operator\n```";
/// let issues = parse_review_issues(response);
/// assert_eq!(issues.len(), 1);
/// ```
pub fn parse_review_issues(response: &str) -> Vec<String> {
    let yaml_content = extract_yaml_block(response);

    if let Some(yaml) = yaml_content
        && let Ok(parsed) = serde_yaml_ng::from_str::<serde_json::Value>(&yaml)
        && let Some(issues) = parsed.get("issues").and_then(|v| v.as_array())
    {
        return issues
            .iter()
            .filter_map(|issue| {
                let severity = issue.get("severity")?.as_str()?;
                if is_actionable_severity(severity) {
                    let desc = issue
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Unknown issue");
                    let file = issue
                        .get("file")
                        .and_then(|f| f.as_str())
                        .unwrap_or("unknown");
                    let suggestion = issue
                        .get("suggestion")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    Some(format!(
                        "[{severity}] {file}: {desc}. Suggestion: {suggestion}"
                    ))
                } else {
                    None
                }
            })
            .collect();
    }

    Vec::new()
}

/// Parses verification results from the agent's YAML response.
///
/// Returns `(passed_count, failed_details)`. If the response cannot be
/// parsed, returns `(0, ["Unable to parse..."])` to avoid false positives.
///
/// # Examples
///
/// ```
/// use coda_core::parser::parse_verification_result;
///
/// let response = "```yaml\nresult: passed\ntotal_count: 5\n```";
/// let (passed, failed) = parse_verification_result(response);
/// assert_eq!(passed, 5);
/// assert!(failed.is_empty());
/// ```
pub fn parse_verification_result(response: &str) -> (u32, Vec<String>) {
    let yaml_content = extract_yaml_block(response);

    if let Some(yaml) = yaml_content
        && let Ok(parsed) = serde_yaml_ng::from_str::<serde_json::Value>(&yaml)
    {
        let result = parsed
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        if result == "passed" {
            let total = parsed
                .get("total_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            return (total, Vec::new());
        }

        let mut failed = Vec::new();
        if let Some(checks) = parsed.get("checks").and_then(|v| v.as_array()) {
            let passed = checks
                .iter()
                .filter(|c| c.get("status").and_then(|s| s.as_str()) == Some("passed"))
                .count() as u32;

            for check in checks {
                if check.get("status").and_then(|s| s.as_str()) == Some("failed") {
                    let name = check
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let details = check
                        .get("details")
                        .and_then(|d| d.as_str())
                        .unwrap_or("no details");
                    failed.push(format!("{name}: {details}"));
                }
            }

            return (passed, failed);
        }
    }

    // If we can't parse, treat as a failure to avoid false positives.
    // The verification loop will ask the agent to fix / re-run, or exhaust retries.
    (
        0,
        vec![
            "Unable to parse verification result from agent response. Manual review required."
                .to_string(),
        ],
    )
}

/// Parses AI verification findings from the Tier 2 YAML response.
///
/// Extracts the `result` field and any `findings` with `status: "concern"`.
/// Returns `(passed, concern_descriptions)` where `passed` is `true` when
/// `result` is `"passed"` and no concerns exist.
///
/// Designed for the scoped verification prompt that produces:
/// ```yaml
/// result: "passed"  # or "needs_attention"
/// findings:
///   - area: "functional"
///     status: "pass"
///     description: "..."
///   - area: "design_conformance"
///     status: "concern"
///     description: "..."
/// ```
///
/// # Examples
///
/// ```
/// use coda_core::parser::parse_ai_verification;
///
/// let response = "```yaml\nresult: passed\nfindings:\n  - area: functional\n    status: pass\n    description: All good\n```";
/// let (passed, findings) = parse_ai_verification(response);
/// assert!(passed);
/// assert!(findings.is_empty());
/// ```
pub fn parse_ai_verification(response: &str) -> (bool, Vec<String>) {
    let yaml_content = extract_yaml_block(response);

    if let Some(yaml) = yaml_content
        && let Ok(parsed) = serde_yaml_ng::from_str::<serde_json::Value>(&yaml)
    {
        let result = parsed
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let concerns: Vec<String> = parsed
            .get("findings")
            .and_then(|v| v.as_array())
            .map(|findings| {
                findings
                    .iter()
                    .filter(|f| f.get("status").and_then(|s| s.as_str()) == Some("concern"))
                    .map(|f| {
                        let area = f.get("area").and_then(|a| a.as_str()).unwrap_or("unknown");
                        let desc = f
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("No description provided");
                        format!("[{area}] {desc}")
                    })
                    .collect()
            })
            .unwrap_or_default();

        let passed = result == "passed" && concerns.is_empty();
        return (passed, concerns);
    }

    // If parsing fails, treat as passed to avoid blocking on parse errors.
    // The deterministic Tier 1 checks already provide the reliable signal.
    (true, Vec::new())
}

/// Extracts a GitHub PR URL from text.
///
/// Scans each line for `https://github.com/.../pull/N` patterns.
///
/// # Examples
///
/// ```
/// use coda_core::parser::extract_pr_url;
///
/// let text = "PR created: https://github.com/org/repo/pull/42";
/// assert_eq!(
///     extract_pr_url(text),
///     Some("https://github.com/org/repo/pull/42".to_string()),
/// );
/// ```
pub fn extract_pr_url(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(start) = line.find("https://github.com/") {
            let url_part = &line[start..];
            // Find end of URL (whitespace, quote, paren, or end of line)
            let end = url_part
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ')')
                .unwrap_or(url_part.len());
            let url = &url_part[..end];
            // Must contain /pull/ AND end with a numeric PR number.
            // Rejects /pull/new/<branch> (GitHub's "create PR" page URL
            // that appears in `git push` output).
            if url.contains("/pull/") && extract_pr_number(url).is_some_and(|n| n > 0) {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Extracts the PR number from a GitHub PR URL.
///
/// # Examples
///
/// ```
/// use coda_core::parser::extract_pr_number;
///
/// assert_eq!(
///     extract_pr_number("https://github.com/org/repo/pull/42"),
///     Some(42),
/// );
/// ```
pub fn extract_pr_number(url: &str) -> Option<u32> {
    url.rsplit('/').next()?.parse().ok()
}

/// Extracts a PR title from `gh pr create` output or agent text.
///
/// Scans each line for patterns commonly produced by the `gh` CLI or
/// the agent when reporting a newly created PR. Recognizes:
///
/// - `gh pr create` output: `https://github.com/.../pull/42` preceded by
///   a `--title "..."` flag in the command
/// - Agent summary: lines like `**Title:** <title>` or `Title: <title>`
///
/// Returns `None` if no title can be extracted from the text. Callers
/// should fall back to querying `gh pr view --json title`.
///
/// # Examples
///
/// ```
/// use coda_core::parser::extract_pr_title;
///
/// let text = "I've created the PR:\n\n**Title:** feat: add user auth\n\nURL: https://github.com/org/repo/pull/42";
/// assert_eq!(extract_pr_title(text), Some("feat: add user auth".to_string()));
/// ```
pub fn extract_pr_title(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();

        // Pattern: **Title:** <title> or *Title:* <title>
        if let Some(rest) = trimmed
            .strip_prefix("**Title:**")
            .or_else(|| trimmed.strip_prefix("*Title:*"))
        {
            let title = rest.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }

        // Pattern: Title: <title> (case-insensitive prefix)
        // Use `get()` for UTF-8-safe slicing instead of direct byte indexing,
        // which would panic if a multi-byte character spans the boundary.
        if let Some(prefix) = trimmed.get(..7)
            && prefix.eq_ignore_ascii_case("title: ")
        {
            // Safe: `get(..7)` succeeded, so byte offset 7 is a valid char boundary.
            let title = trimmed[7..].trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }

        // Pattern: --title "..." or --title '...' in a gh command
        if let Some(idx) = trimmed.find("--title") {
            let after_flag = &trimmed[idx + "--title".len()..].trim_start();
            if let Some(title) = extract_quoted_string(after_flag)
                && !title.is_empty()
            {
                return Some(title);
            }
        }
    }

    None
}

/// Extracts a quoted string from the start of `s`, handling both single
/// and double quotes. Returns the content between the matching quotes.
fn extract_quoted_string(s: &str) -> Option<String> {
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &s[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_yaml_block ──────────────────────────────────────────

    #[test]
    fn test_should_find_yaml_block() {
        let text = "Some text\n```yaml\nissues: []\n```\nMore text";
        let yaml = extract_yaml_block(text);
        assert_eq!(yaml, Some("issues: []".to_string()));
    }

    #[test]
    fn test_should_return_none_for_no_yaml_block() {
        let text = "This is plain text with no code blocks at all.";
        let yaml = extract_yaml_block(text);
        assert!(yaml.is_none());
    }

    #[test]
    fn test_should_extract_first_yaml_block_from_multiple() {
        let text = r#"
First block:
```yaml
issues:
  - severity: "critical"
    description: "First issue"
```

Second block:
```yaml
result: "passed"
```
"#;

        let yaml = extract_yaml_block(text);
        assert!(yaml.is_some());
        let content = yaml.unwrap();
        assert!(content.contains("issues:"));
    }

    #[test]
    fn test_should_extract_yaml_from_unmarked_code_block() {
        let text = "Here are results:\n```\nissues:\n  - severity: critical\n```\nEnd.";
        let yaml = extract_yaml_block(text);
        assert!(yaml.is_some());
        assert!(yaml.unwrap().contains("issues:"));
    }

    #[test]
    fn test_should_return_none_for_text_without_code_blocks() {
        let text = "result: passed\ntotal_count: 3";
        let yaml = extract_yaml_block(text);
        // No code block → None (overly permissive fallback removed)
        assert!(yaml.is_none());
    }

    // ── parse_review_issues ─────────────────────────────────────────

    #[test]
    fn test_should_parse_review_issues_from_yaml() {
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
```
"#;

        let issues = parse_review_issues(response);
        assert_eq!(issues.len(), 1); // Only critical, not minor
        assert!(issues[0].contains("unwrap"));
    }

    #[test]
    fn test_should_return_empty_for_no_issues() {
        let response = r#"
```yaml
issues: []
```
"#;

        let issues = parse_review_issues(response);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_should_parse_review_major_issues() {
        let response = r#"
```yaml
issues:
  - severity: "major"
    file: "src/db.rs"
    line: 100
    description: "SQL injection vulnerability"
    suggestion: "Use parameterized queries"
  - severity: "major"
    file: "src/api.rs"
    line: 55
    description: "Missing authentication check"
    suggestion: "Add auth middleware"
```
"#;

        let issues = parse_review_issues(response);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("SQL injection"));
        assert!(issues[1].contains("authentication"));
    }

    #[test]
    fn test_should_parse_review_with_no_yaml_structure() {
        let text = "The code looks good! No issues found.";
        let issues = parse_review_issues(text);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_should_parse_review_issues_case_insensitive_severity() {
        let response = r#"
```yaml
issues:
  - severity: "Critical"
    file: "src/main.rs"
    line: 1
    description: "Security vulnerability"
    suggestion: "Fix it"
  - severity: "MAJOR"
    file: "src/lib.rs"
    line: 2
    description: "Memory leak"
    suggestion: "Free memory"
  - severity: "Minor"
    file: "src/util.rs"
    line: 3
    description: "Style issue"
    suggestion: "Reformat"
```
"#;

        let issues = parse_review_issues(response);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("Security vulnerability"));
        assert!(issues[1].contains("Memory leak"));
    }

    // ── parse_verification_result ───────────────────────────────────

    #[test]
    fn test_should_parse_verification_passed() {
        let response = r#"
```yaml
result: "passed"
total_count: 5
checks:
  - name: "cargo build"
    status: "passed"
```
"#;

        let (passed, failed) = parse_verification_result(response);
        assert_eq!(passed, 5);
        assert!(failed.is_empty());
    }

    #[test]
    fn test_should_parse_verification_failed() {
        let response = r#"
```yaml
result: "failed"
checks:
  - name: "cargo build"
    status: "passed"
  - name: "cargo test"
    status: "failed"
    details: "2 tests failed"
failed_count: 1
total_count: 2
```
"#;

        let (passed, failed) = parse_verification_result(response);
        assert_eq!(passed, 1);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains("cargo test"));
    }

    #[test]
    fn test_should_treat_unparsable_verification_as_failure() {
        let text = "All tests passed successfully!";
        let (passed, failed) = parse_verification_result(text);
        assert_eq!(passed, 0);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].contains("Unable to parse"));
    }

    // ── parse_ai_verification ───────────────────────────────────────

    #[test]
    fn test_should_parse_ai_verification_passed() {
        let response = r#"
```yaml
result: "passed"
findings:
  - area: "functional"
    status: "pass"
    description: "All scenarios verified"
  - area: "design_conformance"
    status: "pass"
    description: "Implementation matches design"
```
"#;
        let (passed, findings) = parse_ai_verification(response);
        assert!(passed);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_should_parse_ai_verification_with_concerns() {
        let response = r#"
```yaml
result: "needs_attention"
findings:
  - area: "functional"
    status: "pass"
    description: "Core scenarios work"
  - area: "design_conformance"
    status: "concern"
    description: "Missing error handling in parser"
  - area: "integration"
    status: "concern"
    description: "API contract mismatch"
```
"#;
        let (passed, findings) = parse_ai_verification(response);
        assert!(!passed);
        assert_eq!(findings.len(), 2);
        assert!(findings[0].contains("[design_conformance]"));
        assert!(findings[0].contains("Missing error handling"));
        assert!(findings[1].contains("[integration]"));
        assert!(findings[1].contains("API contract mismatch"));
    }

    #[test]
    fn test_should_treat_unparsable_ai_verification_as_passed() {
        let response = "The code looks great! No issues found.";
        let (passed, findings) = parse_ai_verification(response);
        assert!(passed);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_should_parse_ai_verification_no_findings_key() {
        let response = r#"
```yaml
result: "passed"
```
"#;
        let (passed, findings) = parse_ai_verification(response);
        assert!(passed);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_should_detect_concerns_even_with_passed_result() {
        let response = r#"
```yaml
result: "passed"
findings:
  - area: "edge_case"
    status: "concern"
    description: "Edge case not handled"
```
"#;
        let (passed, findings) = parse_ai_verification(response);
        // Even though result says "passed", concerns override
        assert!(!passed);
        assert_eq!(findings.len(), 1);
    }

    // ── extract_pr_url ──────────────────────────────────────────────

    #[test]
    fn test_should_extract_pr_url() {
        let text = "PR created: https://github.com/org/repo/pull/42\nDone!";
        assert_eq!(
            extract_pr_url(text),
            Some("https://github.com/org/repo/pull/42".to_string())
        );
    }

    #[test]
    fn test_should_return_none_when_no_pr_url_found() {
        let text = "No PR URL here, just some text.";
        assert!(extract_pr_url(text).is_none());
    }

    #[test]
    fn test_should_extract_pr_url_from_markdown_link() {
        let text = "Created [PR #42](https://github.com/org/repo/pull/42) for review.";
        let url = extract_pr_url(text);
        assert_eq!(url, Some("https://github.com/org/repo/pull/42".to_string()));
    }

    #[test]
    fn test_should_not_extract_non_pr_github_url() {
        let text = "See https://github.com/org/repo/issues/10 for details.";
        assert!(extract_pr_url(text).is_none());
    }

    #[test]
    fn test_should_reject_git_push_create_pr_page_url() {
        let text = "remote: Create a pull request for 'feature/update-doc' on GitHub by visiting:\nremote:   https://github.com/lehuagavin/coda/pull/new/feature/update-doc";
        assert!(extract_pr_url(text).is_none());
    }

    // ── extract_pr_number ───────────────────────────────────────────

    #[test]
    fn test_should_extract_pr_number() {
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/42"),
            Some(42)
        );
    }

    #[test]
    fn test_should_extract_pr_number_from_valid_url() {
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/123"),
            Some(123)
        );
        assert_eq!(
            extract_pr_number("https://github.com/my-org/my-repo/pull/1"),
            Some(1)
        );
    }

    #[test]
    fn test_should_return_none_for_non_numeric_pr_number() {
        assert_eq!(extract_pr_number("https://github.com/org/repo/pull/"), None);
        assert_eq!(
            extract_pr_number("https://github.com/org/repo/pull/abc"),
            None
        );
    }

    // ── extract_pr_title ────────────────────────────────────────────

    #[test]
    fn test_should_extract_pr_title_from_bold_markdown() {
        let text = "I've created the PR:\n\n**Title:** feat: add user auth\n\nURL: https://github.com/org/repo/pull/42";
        assert_eq!(
            extract_pr_title(text),
            Some("feat: add user auth".to_string())
        );
    }

    #[test]
    fn test_should_extract_pr_title_from_plain_label() {
        let text = "Title: refactor: decompose runner\nURL: ...";
        assert_eq!(
            extract_pr_title(text),
            Some("refactor: decompose runner".to_string())
        );
    }

    #[test]
    fn test_should_extract_pr_title_from_gh_command_double_quotes() {
        let text = r#"gh pr create --title "feat(auth): add JWT support" --body "..."
https://github.com/org/repo/pull/55"#;
        assert_eq!(
            extract_pr_title(text),
            Some("feat(auth): add JWT support".to_string())
        );
    }

    #[test]
    fn test_should_extract_pr_title_from_gh_command_single_quotes() {
        let text = "gh pr create --title 'fix: resolve null pointer' --body '...'";
        assert_eq!(
            extract_pr_title(text),
            Some("fix: resolve null pointer".to_string())
        );
    }

    #[test]
    fn test_should_return_none_when_no_pr_title_found() {
        let text = "PR created successfully at https://github.com/org/repo/pull/42";
        assert!(extract_pr_title(text).is_none());
    }

    #[test]
    fn test_should_extract_pr_title_from_italic_markdown() {
        let text = "*Title:* docs: update README\nBody: ...";
        assert_eq!(
            extract_pr_title(text),
            Some("docs: update README".to_string())
        );
    }

    #[test]
    fn test_should_not_panic_on_non_ascii_title_prefix() {
        // Multi-byte UTF-8 characters where byte offset 7 falls mid-character
        let text = "\u{1F600}\u{1F600}x: rest";
        // Should not panic and should return None (prefix doesn't match "title: ")
        assert!(extract_pr_title(text).is_none());
    }

    #[test]
    fn test_should_extract_title_case_insensitive() {
        let text = "TITLE: uppercase title";
        assert_eq!(extract_pr_title(text), Some("uppercase title".to_string()));
    }

    // ── is_actionable_severity ──────────────────────────────────────

    #[test]
    fn test_should_identify_actionable_severity_case_insensitive() {
        assert!(is_actionable_severity("critical"));
        assert!(is_actionable_severity("Critical"));
        assert!(is_actionable_severity("CRITICAL"));
        assert!(is_actionable_severity("major"));
        assert!(is_actionable_severity("Major"));
        assert!(is_actionable_severity("MAJOR"));
        assert!(!is_actionable_severity("minor"));
        assert!(!is_actionable_severity("info"));
        assert!(!is_actionable_severity("informational"));
    }
}
