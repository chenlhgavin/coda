# CODA Documentation Index

## Project Overview

See [`.coda.md`](../.coda.md) for a comprehensive overview of the repository structure, tech stack, and conventions.

## Architecture

| Document | Description |
|----------|-------------|
| [`arch.md`](arch.md) | 系统架构文档 — 整体架构图、目录结构、核心抽象、每个 CLI 命令的详细流程图、数据流、设计决策 |

## Guides

| Document | Description |
|----------|-------------|
| [`slack-setup.md`](slack-setup.md) | Slack App creation and configuration guide for `coda-server` |

## Research

| Document | Description |
|----------|-------------|
| [`research/openai-codex-cli-research.md`](research/openai-codex-cli-research.md) | OpenAI Codex CLI research -- product status, capabilities, API/interface, code review, CI/CD integration |

## Per-Crate API Reference

Each library crate maintains an `API.md` file at its root that documents the crate's public interface: re-exported types, traits, structs, enums, and free functions with one-line descriptions.

| Crate | Path | Description |
|-------|------|-------------|
| `coda-core` | [`crates/coda-core/API.md`](../crates/coda-core/API.md) | Core engine, configuration, state management, git/gh abstractions |
| `coda-pm` | [`crates/coda-pm/API.md`](../crates/coda-pm/API.md) | Prompt template manager using MiniJinja |

### Maintenance

When a public API changes (adding, removing, or renaming a public item), update the corresponding `API.md` to keep it in sync with the source code. These files are intentionally manually maintained to stay flat and scannable, complementing the full `cargo doc` output.
