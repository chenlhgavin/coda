# Review Prompt Reference

Extended prompt templates and customization guidance for Codex code review.

## Prompt Construction Rules

1. **Always include project standards** - Extract relevant sections from CLAUDE.md or workspace rules
2. **Always specify output format** - Structured findings with severity levels
3. **Keep code context focused** - Only include relevant files/diffs, not the entire repo
4. **Specify language/framework** - Helps Codex apply correct idioms
5. **Request web search explicitly** - "Use web search to verify..." reduces false positives

## Composable Prompt Blocks

### Block: Project Context

```
PROJECT CONTEXT:
- Language: Rust (2024 edition)
- Async runtime: Tokio
- Error handling: thiserror (library) / anyhow (application)
- Serialization: serde with camelCase JSON
- Logging: tracing crate
- Testing: rstest for parameterized, proptest for property-based
- Concurrency: Actor model, message passing, DashMap, ArcSwap
```

### Block: Review Scope Constraint

```
SCOPE:
- Only review the code provided. Do not suggest changes to files outside the diff.
- Focus on correctness first, then security, then performance, then style.
- Do not flag issues that are clearly intentional design choices unless they violate safety.
- If uncertain about a pattern, use web search before flagging.
```

### Block: Rust-Specific Checks

```
RUST-SPECIFIC CHECKS:
- No unwrap()/expect() in production code
- No unsafe blocks
- No todo!() macros
- All public items have doc comments
- Error types use thiserror with #[source]
- Serde: rename_all = "camelCase", skip_serializing_if for Option
- Imports ordered: std, deps, local
- Functions under 150 lines
- Debug implemented for all types
```

### Block: Output Format (Markdown)

```
OUTPUT FORMAT:
Return a markdown report with these sections:
1. **Summary** - One paragraph overview
2. **Findings** - Grouped by severity (CRITICAL > WARNING > SUGGESTION > NITPICK)
3. **Metrics** - Count of findings per severity
4. **Verdict** - APPROVE / REQUEST_CHANGES / NEEDS_DISCUSSION
```

### Block: Output Format (JSON)

Use with `--output-schema` for CI integration:

```json
{
  "type": "object",
  "properties": {
    "summary": { "type": "string" },
    "verdict": { "enum": ["approve", "request_changes", "needs_discussion"] },
    "findings": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "severity": { "enum": ["critical", "warning", "suggestion", "nitpick"] },
          "file": { "type": "string" },
          "line": { "type": "integer" },
          "issue": { "type": "string" },
          "fix": { "type": "string" }
        },
        "required": ["severity", "issue"]
      }
    },
    "metrics": {
      "type": "object",
      "properties": {
        "critical": { "type": "integer" },
        "warning": { "type": "integer" },
        "suggestion": { "type": "integer" },
        "nitpick": { "type": "integer" }
      }
    }
  },
  "required": ["summary", "verdict", "findings", "metrics"],
  "additionalProperties": false
}
```

## Custom Review Focus

Users may request specific focus areas. Append these to the base prompt:

| User Request | Append to Prompt |
|---|---|
| "Focus on error handling" | "Pay special attention to error propagation, context messages, and recovery strategies." |
| "Check async correctness" | "Focus on: blocking in async, missing cancellation safety, spawn without JoinHandle, channel misuse." |
| "Review API design" | "Focus on public API ergonomics, type safety, documentation completeness, and backward compatibility." |
| "Check for breaking changes" | "Compare with the previous version. Flag any public API changes that break semver compatibility." |
| "Review test quality" | "Focus on test coverage, edge cases, assertion quality, test isolation, and naming conventions." |

## Full Command Examples

### Staged changes with Rust context

```bash
codex exec --full-auto --sandbox read-only --search \
  "You are reviewing a Rust project. Read CLAUDE.md for project standards. \
   Run 'git diff --cached' to see staged changes. \
   Review the changes following the project standards. \
   Use web search to verify dependency versions and API patterns. \
   Output findings grouped by severity: CRITICAL, WARNING, SUGGESTION, NITPICK. \
   End with a verdict: APPROVE, REQUEST_CHANGES, or NEEDS_DISCUSSION."
```

### PR diff via GitHub CLI

```bash
codex exec --full-auto --sandbox read-only --search \
  "Run 'gh pr diff 42' to get the PR diff. \
   Read CLAUDE.md for project standards. \
   Review the diff for correctness, security, performance, and style. \
   Use web search to check dependency versions mentioned in the diff. \
   Produce a structured review with severity-tagged findings."
```

### Directory-wide review

```bash
codex exec --full-auto --sandbox read-only --search \
  "Review all Rust source files in crates/coda-core/src/. \
   Read CLAUDE.md for project standards. \
   Focus on architecture, error handling, and type design. \
   Flag SOLID/DRY/KISS violations."
```

### JSON output for CI pipeline

```bash
codex exec --full-auto --sandbox read-only --search \
  --output-schema ./review-schema.json \
  -o ./review-results.json \
  "Review staged changes. Read CLAUDE.md for standards. \
   Output structured findings."
```
