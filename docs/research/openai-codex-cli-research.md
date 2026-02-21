# OpenAI Codex CLI -- Research Report

**Date:** 2026-02-21

## 1. What Is OpenAI Codex CLI?

OpenAI Codex CLI is a **real, shipping, open-source product**. It is a lightweight coding
agent that runs locally in your terminal, backed by OpenAI's Codex-series models. It is
**not** a concept or vaporware -- it is actively developed, versioned (currently v0.104.0 as
of 2026-02-18), and available for installation.

- **Repository:** <https://github.com/openai/codex>
- **License:** Apache-2.0
- **Language:** Originally TypeScript/Node.js, now **fully rewritten in Rust** (primary since
  late 2025, currently at `rust-v0.101+` range).
- **Installation:** `npm i -g @openai/codex`, `brew install --cask codex`, or download binary
  from GitHub Releases.
- **Platforms:** macOS, Linux, Windows (experimental).

### Authentication

- **ChatGPT subscription:** Sign in with ChatGPT account (Plus, Pro, Team, Edu, Enterprise).
  Zero additional cost for CLI usage.
- **API key:** Use an OpenAI API key with `CODEX_API_KEY` env var. Billed at standard API
  rates.

## 2. How Does It Work?

Codex CLI operates in two primary modes:

### Interactive Mode (TUI)

Run `codex` to launch an interactive terminal UI session. The agent can:
- Read your repository files and understand codebase structure.
- Edit files across the project.
- Execute shell commands (subject to sandbox policy).
- Plan changes before executing them.
- Attach and share images (screenshots, wireframes, diagrams).

### Non-Interactive / Headless Mode (`codex exec`)

Run `codex exec "your prompt"` for scripted/CI usage. Key characteristics:
- Processes a single prompt, executes actions automatically, exits on completion.
- Progress streams to stderr; final message prints to stdout.
- Supports `--json` flag for JSON Lines (JSONL) event streaming.
- Supports `--output-schema` for structured, schema-validated JSON responses.
- Sessions can be resumed: `codex exec resume --last "follow-up prompt"`.

### Safety Levels (Sandbox)

Three sandbox modes control what Codex can do:
- **`read-only`** -- Can read files but not modify anything (default in `exec`).
- **`workspace-write`** -- Can edit files in the workspace.
- **`danger-full-access`** -- Full system access (only for isolated environments).

Sandboxing is enforced via macOS Seatbelt and Linux Landlock kernel security.

### Project Configuration

Codex reads `AGENTS.md` files (similar to Claude Code's `CLAUDE.md`) for project-specific
instructions. Discovery walks from `~/.codex/AGENTS.md` (global) down through the project
directory tree. Configuration lives in `~/.codex/config.toml`.

## 3. Capabilities -- Including Code Review

### Code Generation & Editing
- Write features, fix bugs, refactor code.
- Make coordinated changes across multiple files.
- Understand entire codebases via agentic search.

### Code Review (`/review` command)
Codex CLI has a **built-in `/review` command** (introduced in v0.39) that:
- Analyzes diffs against base branches, uncommitted changes, or specific commits.
- Reports findings in priority order (correctness, performance, security, maintainability).
- Is **read-only** -- never touches your working tree.
- Supports **custom review instructions** (e.g., "Focus on security vulnerabilities").
- Supports a **configurable review model** via `review_model` in `config.toml`.
- Can be run iteratively with results appearing as separate turns in the transcript.

Review modes:
- **Against a base branch** -- ideal for preparing PRs.
- **Uncommitted changes** -- inspects staged, unstaged, and untracked files.

Unlike static analysis tools, Codex review:
- Matches the stated intent of a PR to the actual diff.
- Reasons over the entire codebase and dependencies.
- Can execute code and tests to validate behavior.

### Plan Mode
Streams proposed plans into a dedicated TUI view. Useful for reviewing what Codex intends
to do before execution.

### MCP (Model Context Protocol) Support
Codex can run as an MCP server (`codex mcp-server`) over stdio, allowing other tools to
connect and interact with it.

### Web Search
Live web search can be enabled with `--search` flag.

## 4. API / Interface -- Programmatic Invocation

### 4a. CLI (`codex exec`)

The primary scripting interface. Key flags:

| Flag | Purpose |
|------|---------|
| `--json` | Emit JSONL event stream |
| `--output-last-message PATH` | Write final response to file |
| `--output-schema PATH` | Validate response against JSON schema |
| `--ephemeral` | No session persistence |
| `--full-auto` | Workspace-write sandbox + on-request approvals |
| `--sandbox LEVEL` | read-only / workspace-write / danger-full-access |
| `--model STRING` | Override model |
| `--dangerously-bypass-approvals-and-sandbox` | Skip all protections |
| `--skip-git-repo-check` | Run outside git repos |

Event types in JSONL stream: `thread.started`, `turn.started`, `turn.completed`,
`turn.failed`, `item.*` (messages, reasoning, commands, file changes, MCP calls), `error`.

### 4b. TypeScript SDK (`@openai/codex-sdk`)

A programmatic library for controlling Codex agents from Node.js (18+) applications:

```typescript
import { Codex } from "@openai/codex-sdk";

const codex = new Codex();
const thread = codex.startThread();
const result = await thread.run("Review the latest changes for security issues");

// Continue conversation
await thread.run("Fix the issues you found");

// Resume a previous thread
const thread2 = codex.resumeThread("<thread-id>");
await thread2.run("Pick up where you left off");
```

More comprehensive and flexible than `codex exec`, but requires Node.js server-side.

### 4c. MCP Server Mode

```bash
codex mcp-server
```

Exposes Codex as an MCP server over stdio. Other MCP clients (e.g., OpenAI Agents SDK
agents) can connect and interact. Provides `codex-reply` tool for continuing sessions by
thread ID.

### 4d. App Server (JSON-RPC API)

A bidirectional JSON-RPC API for rich client integration. Used by the desktop app and IDE
extension internally.

### 4e. GitHub Action (`openai/codex-action@v1`)

Pre-built GitHub Action for CI/CD integration:

```yaml
- uses: openai/codex-action@v1
  with:
    prompt: "Review this PR for bugs and security issues"
    model: gpt-5.2-codex
    sandbox: read-only
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
```

Handles CLI installation, proxy configuration, security controls (drop-sudo, unprivileged
user), and output capture.

## 5. Calling from Other Tools/Scripts

Yes, Codex CLI is explicitly designed for integration:

1. **Shell scripts / CI pipelines:** Use `codex exec --json` with `jq` for parsing.
2. **Node.js applications:** Use `@openai/codex-sdk` for native integration.
3. **MCP clients:** Connect via `codex mcp-server` stdio transport.
4. **GitHub Actions:** Use `openai/codex-action@v1`.
5. **OpenAI Agents SDK:** Use Codex as a tool within multi-agent orchestration.
6. **Custom CI/CD:** Build code review pipelines with structured output schemas for
   GitHub, GitLab, and Jenkins.

### Automated Code Review in CI/CD (Example Pattern)

1. Trigger on PR events.
2. Generate unified diff.
3. Run `codex exec` with a review prompt and `--output-schema` pointing to a JSON schema
   that defines findings format (title, body, confidence, priority, file, line range).
4. Parse the structured JSON output.
5. Post inline comments via GitHub/GitLab REST API.

## 6. Current Status

### Stability: **Stable / GA-equivalent**

- **Version:** 0.104.0 (2026-02-18).
- **Rust rewrite:** Complete and primary. Rust toolchain ~1.93.
- **Release cadence:** Very active -- multiple releases per week.
- **Models:** Latest is `gpt-5.3-codex` (default for ChatGPT-authenticated sessions).
  `gpt-5.3-codex-spark` in research preview (Pro only). `gpt-5.2-codex` available via API.
- **Platform:** macOS and Linux are fully supported. Windows is experimental.

### Feature Maturity

| Feature | Status |
|---------|--------|
| Interactive TUI | Stable |
| `codex exec` (non-interactive) | Stable |
| `/review` (code review) | Stable |
| MCP server | Experimental |
| Cloud tasks (`codex cloud`) | Experimental |
| TypeScript SDK | Available |
| GitHub Action | Available |
| AGENTS.md support | Stable |
| IDE extension (VS Code, Cursor) | Available |
| Plan mode | Stable (feature-gated) |

### Known Limitations

- Orchestration for multi-agent workflows has rough edges (GitHub issue #4219).
- Windows support remains experimental.
- The SDK documentation is sparse compared to the CLI docs.
- `gpt-5.3-codex-spark` is Pro-only and not yet available via API.

## 7. Comparison with Claude Code CLI

| Dimension | Codex CLI | Claude Code |
|-----------|-----------|-------------|
| Open source | Yes (Apache 2.0) | No |
| Language | Rust | Rust |
| Config file | AGENTS.md | CLAUDE.md |
| Code review | Built-in `/review` | No dedicated command |
| SDK | TypeScript SDK | No public SDK |
| MCP | Server mode | Deeper MCP integrations |
| CI/CD | GitHub Action, `codex exec` | `claude -p` non-interactive |
| Pricing | Free with ChatGPT sub; or API rates | Requires Claude subscription |
| SWE-bench | 69.1% | 72.7% |
| Strength | Speed, cost, extensibility | Complex reasoning, consistency |

## 8. Relevance to CODA

Key takeaways for potential integration or inspiration:

1. **Code review via CLI** is a proven pattern. Codex's `/review` with structured JSON
   output and schema validation is a solid reference architecture.
2. **Non-interactive mode** with JSONL event streaming is the standard interface for
   tool-to-tool integration.
3. **AGENTS.md** is Codex's equivalent of CLAUDE.md -- hierarchical project configuration.
4. **MCP server mode** enables composability with other AI agents.
5. **The TypeScript SDK** provides a higher-level programmatic API when shell invocation
   is too limited.
6. **GitHub Action** pattern shows how to package a CLI agent for CI/CD consumption.
