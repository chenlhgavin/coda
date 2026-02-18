# CODA Documentation Index

## Project Overview

See [`.coda.md`](../.coda.md) for a comprehensive overview of the repository structure, tech stack, and conventions.

## Per-Crate API Reference

Each library crate maintains an `API.md` file at its root that documents the crate's public interface: re-exported types, traits, structs, enums, and free functions with one-line descriptions.

| Crate | Path | Description |
|-------|------|-------------|
| `coda-core` | [`crates/coda-core/API.md`](../crates/coda-core/API.md) | Core engine, configuration, state management, git/gh abstractions |
| `coda-pm` | [`crates/coda-pm/API.md`](../crates/coda-pm/API.md) | Prompt template manager using MiniJinja |

### Maintenance

When a public API changes (adding, removing, or renaming a public item), update the corresponding `API.md` to keep it in sync with the source code. These files are intentionally manually maintained to stay flat and scannable, complementing the full `cargo doc` output.
