

# Feature: update-doc

## Overview

Each library crate in the CODA workspace (`coda-core`, `coda-pm`) will have an `API.md` file at its crate root that documents the crate's public interface — re-exported types, traits, structs, enums, and free functions — with one-line descriptions. These files serve as a quick-reference index for both human developers and AI agents, complementing the full `cargo doc` output with a scannable, manually-maintained summary.

The motivation is discoverability: a developer or agent can open a single file to understand what a crate exposes and how its modules are organized, without reading source code or generating rustdoc.

## High-Level Design

This is a documentation-only change with no impact on code, compilation, or runtime behavior.

```
crates/
├── coda-core/
│   ├── API.md          ← NEW: public interface reference
│   └── src/
│       └── lib.rs      (source of truth for re-exports)
├── coda-pm/
│   ├── API.md          ← NEW: public interface reference
│   └── src/
│       └── lib.rs      (source of truth for re-exports)
docs/
└── index.md            ← UPDATED: note about API.md convention
```

Each `API.md` follows a two-level structure:

1. **Re-exports table** — items available directly from `use coda_core::X` or `use coda_pm::X`. This is the primary consumer-facing surface.
2. **Module sections** — for each `pub mod`, a table (or code block for complex traits) listing the public items within that module. This is useful for contributors and for understanding internal organization.

Traits with multiple methods use fenced Rust code blocks showing the full trait definition rather than squeezing signatures into table cells.

## Interface Design

No code changes. The deliverable is Markdown documentation only.

The standardized `API.md` template is:

```markdown
# <crate-name> API Reference

> <One-line crate purpose from `//!` doc comment.>

## Re-exports

| Item | Kind | Description |
|------|------|-------------|
| `TypeName` | struct/trait/enum/fn | One-line description |

## Modules

### `mod module_name`

| Item | Kind | Description |
|------|------|-------------|
| `TypeName` | struct/trait/enum/fn | One-line description |

For complex traits:

​```rust
pub trait TraitName {
    fn method(&self, arg: Type) -> ReturnType;
}
​```
```

## Directory Structure

| Path | Action | Description |
|------|--------|-------------|
| `crates/coda-pm/API.md` | **Create** | Public interface reference for `coda-pm` |
| `crates/coda-core/API.md` | **Create** | Public interface reference for `coda-core` |
| `docs/index.md` | **Update** | Add section about per-crate `API.md` convention |

## Core Data Structures

Not applicable — this feature introduces no new data structures. The deliverables are Markdown files only.

## Development Phases

### Phase 1: `coda-pm/API.md`
- **Goal**: Establish the `API.md` template using the smaller crate as the first example.
- **Tasks**:
  - Read all public items from `coda-pm/src/lib.rs`, `manager.rs`, `template.rs`, `error.rs`, and `loader.rs`.
  - Create `crates/coda-pm/API.md` documenting: `PromptManager`, `PromptTemplate`, `PromptError`, and the `loader` module's public items.
  - Follow the re-exports table + module sections format defined above.
- **Commit message**: `"docs(coda-pm): add API.md with public interface reference"`

### Phase 2: `coda-core/API.md`
- **Goal**: Document the larger crate following the same template.
- **Tasks**:
  - Read all public items from `coda-core/src/lib.rs` and each public module (`config`, `engine`, `error`, `gh`, `git`, `parser`, `planner`, `profile`, `project`, `reviewer`, `runner`, `scanner`, `state`, `task`).
  - Create `crates/coda-core/API.md` documenting all re-exports and per-module public items.
  - Use Rust code blocks for complex traits (`GitOps`, `GhOps`) instead of table rows.
- **Commit message**: `"docs(coda-core): add API.md with public interface reference"`

### Phase 3: Update `docs/index.md`
- **Goal**: Make the `API.md` convention discoverable project-wide.
- **Tasks**:
  - Add a section to `docs/index.md` explaining that each library crate maintains an `API.md` at its root.
  - Note the maintenance expectation: when a public API changes, update the corresponding `API.md`.
- **Commit message**: `"docs: document per-crate API.md convention in docs/index.md"`

## Dependencies

None. This is a documentation-only feature with no new crate dependencies.

## Risk & Trade-offs

| Risk | Severity | Mitigation |
|------|----------|------------|
| **Documentation drift** — `API.md` goes stale as code evolves | Medium | Add a note in `CLAUDE.md` or PR review checklist: "if you change a public API, update `API.md`". A CI lint could be added later if drift becomes a recurring problem. |
| **Duplication with `cargo doc`** — overlapping content with generated rustdoc | Low | Different purposes: `API.md` is a flat, scannable index optimized for quick context loading; rustdoc is the full rendered reference with examples and cross-links. They complement rather than replace each other. |
| **Table format limitations** — complex signatures don't fit cleanly in Markdown tables | Low | Convention allows falling back to fenced Rust code blocks for traits with multiple methods or complex generics. |
| **Maintenance burden** — extra step for every public API change | Low | The files are small and changes are mechanical. The cost is proportional to the API change itself. |

## Guidelines

- Each phase MUST result in code that compiles and passes existing tests
- Phases should be ordered from foundational to feature-complete
- Keep phases small — ideally 1-3 files changed per phase
- Use conventional commit messages (feat:, fix:, refactor:, test:, docs:)
- Reference existing patterns in the codebase where applicable