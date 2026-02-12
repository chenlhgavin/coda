---
name: codex-code-review
description: Run code review using OpenAI Codex CLI in headless mode. Supports reviewing git staged changes, specific files, directories, and git diffs. Use when the user requests code review, diff/PR analysis, security audit, performance/architecture analysis, or automated code quality feedback.
---

# Codex Code Review

Perform deep code review via OpenAI Codex CLI (`codex exec`) in non-interactive mode. The agent gathers the review target, constructs a prompt with project standards, and pipes the output back to the user.

## Prerequisites

Codex CLI must be installed and authenticated:

```bash
# Install
npm i -g @openai/codex

# Authenticate (one-time)
codex login
```

## Review Workflow

### Step 1: Determine Review Target

Ask the user (or infer from context) which review type they need:

| Type | How to gather code |
|---|---|
| **Staged changes** | `git diff --cached` |
| **Uncommitted changes** | `git diff` (unstaged) or `git diff HEAD` (all uncommitted) |
| **Specific files** | `cat <file1> <file2> ...` or pass paths directly |
| **Directory** | `find <dir> -type f -name '*.rs'` (adapt extension) |
| **Branch diff** | `git diff <base>...<head>` |
| **Commit** | `git show <sha>` or `git diff <sha>~1..<sha>` |
| **PR diff** | `gh pr diff <number>` (requires GitHub CLI) |

### Step 2: Build the Codex Exec Command

Use `codex exec` with these **mandatory** flags for headless code review:

```bash
codex exec \
  --full-auto \
  --sandbox read-only \
  --search \
  "<REVIEW_PROMPT>"
```

**Flag rationale:**

- `--full-auto`: No human approval needed (safe because sandbox is read-only)
- `--sandbox read-only`: Review never modifies files
- `--search`: Enable live web search to verify dependency versions, API patterns, and best practices (reduces false positives)

**Optional flags:**

- `--model <model>`: Override model (default: `gpt-5.3-codex`)
- `-C <path>`: Set working directory if reviewing a different repo
- `--json`: Machine-readable JSONL output for CI integration

### Step 3: Construct the Review Prompt

Combine the review target with a structured prompt. The prompt MUST include:

1. **The code to review** (inline diff or file references)
2. **Project standards** (from CLAUDE.md, .cursor/rules, or AGENTS.md if present)
3. **Review focus areas** based on user request
4. **Output format specification**

Use the prompt templates below, adapting to the specific review type.

### Step 4: Execute and Present Results

1. Run the `codex exec` command via the Shell tool
2. Set `block_until_ms` high enough (120000+ for large reviews)
3. Parse the output and present findings to the user organized by severity

## Prompt Templates

### General Code Review

```
Review the following code changes. Use web search to verify any dependency versions,
API patterns, or best practices you reference.

PROJECT STANDARDS:
- [Insert relevant standards from CLAUDE.md / workspace rules]

CODE TO REVIEW:
<paste diff or file content here>

Produce a structured review with these sections:

## Summary
One-paragraph overview of the changes and their purpose.

## Findings

For each finding, use this format:
- **[CRITICAL]** / **[WARNING]** / **[SUGGESTION]** / **[NITPICK]**
  - File: <path>:<line>
  - Issue: <description>
  - Why: <explanation with references>
  - Fix: <suggested fix or code snippet>

Severity definitions:
- CRITICAL: Bugs, security vulnerabilities, data loss risks. Must fix.
- WARNING: Performance issues, error handling gaps, design violations. Should fix.
- SUGGESTION: Improvements to readability, maintainability, architecture. Consider fixing.
- NITPICK: Style, naming, minor preferences. Optional.

## Architecture & Design
Assess adherence to SOLID, DRY, KISS, YAGNI. Flag violations with rationale.

## Security
Check for: unsafe code, input validation gaps, secret exposure, injection risks.

## Testing
Assess test coverage adequacy. Suggest missing test cases.

## Dependencies
Verify dependency versions are current and appropriate. Flag known vulnerabilities.

If there are no findings in a section, write "No issues found." Do not invent problems.
```

### Security Audit

```
Perform a security-focused audit of the following code.
Use web search to check for known CVEs in dependencies and verify security best practices.

PROJECT STANDARDS:
- Never use unsafe blocks
- Use rustls with aws-lc-rs, never native-tls/OpenSSL
- Use secrecy crate for secrets, subtle for constant-time comparison
- Never log/expose sensitive data
- Validate all external input

CODE TO REVIEW:
<paste code here>

Focus on:
1. Input validation and sanitization
2. Authentication and authorization flaws
3. Injection vulnerabilities (SQL, command, path traversal)
4. Secrets handling (hardcoded credentials, logging sensitive data)
5. Cryptographic misuse
6. Dependency vulnerabilities (search web for CVEs)
7. Unsafe code usage
8. Race conditions and TOCTOU bugs

Format findings as:
- **[CRITICAL]** / **[WARNING]** / **[INFO]**
  - File: <path>:<line>
  - Vulnerability class: <CWE category if applicable>
  - Issue: <description>
  - Impact: <what could go wrong>
  - Fix: <remediation>
```

### Performance Review

```
Analyze the following code for performance issues.
Use web search to verify benchmark data and optimization patterns.

PROJECT STANDARDS:
- Profile before optimizing
- Avoid unnecessary allocations, prefer &str over String
- Use Vec::with_capacity() when size is known
- Use iterators over explicit loops
- Use SmallVec/Cow<str> where appropriate
- Use DashMap over Mutex<HashMap>

CODE TO REVIEW:
<paste code here>

Focus on:
1. Unnecessary heap allocations and cloning
2. Missing pre-allocation (Vec, HashMap)
3. Blocking operations in async context
4. Lock contention and concurrency bottlenecks
5. Inefficient data structures
6. Missing caching opportunities
7. Hot path optimization opportunities

Format as: **[HIGH]** / **[MEDIUM]** / **[LOW]** impact with estimated improvement.
```

### Architecture Review

```
Review the following code for architectural and design quality.
Use web search for current best practices in the relevant domain.

PROJECT STANDARDS:
- Actor model for subsystems, message passing over shared state
- thiserror for library errors, anyhow for application errors
- typed-builder for complex structs
- Make illegal states unrepresentable via type system
- Async traits: native async fn, async-trait only for dyn Trait

CODE TO REVIEW:
<paste code here>

Assess:
1. SOLID principle adherence (Single Responsibility, Open-Closed, Liskov, Interface Segregation, Dependency Inversion)
2. DRY violations
3. KISS compliance (is it unnecessarily complex?)
4. YAGNI violations (over-engineering, unused abstractions)
5. Module boundaries and cohesion
6. API design clarity (public interface quality)
7. Error handling strategy consistency
8. Type design (are illegal states possible?)

Format as: **[VIOLATION]** / **[CONCERN]** / **[SUGGESTION]** with principle reference.
```

## Usage Examples

### Review staged changes

```bash
DIFF=$(git diff --cached)
codex exec --full-auto --sandbox read-only --search \
  "Review these staged changes for a Rust project. Standards: [from CLAUDE.md]. Diff: $DIFF"
```

### Review specific files

```bash
codex exec --full-auto --sandbox read-only --search \
  "Review the files src/lib.rs and src/error.rs for code quality. Read them from disk."
```

### Review branch diff against main

```bash
DIFF=$(git diff main...HEAD)
codex exec --full-auto --sandbox read-only --search \
  "Review this branch diff against main: $DIFF"
```

### Review with JSON output for CI

```bash
codex exec --full-auto --sandbox read-only --search --json \
  "Review staged changes and report findings." | jq '.item.text // empty'
```

## Integration Notes

- For large diffs, let Codex read files from disk rather than inlining (it has filesystem access in read-only sandbox)
- For PR reviews, use `gh pr diff <number>` to get the diff, or let Codex run `git diff` itself
- Always include relevant project standards in the prompt to ensure consistent feedback
- The `--search` flag is essential for verifying dependency versions and avoiding false positives on API usage
- Review output is streamed to stderr during execution; final report goes to stdout
