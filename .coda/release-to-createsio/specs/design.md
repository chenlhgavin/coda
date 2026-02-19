

# Feature: release-to-cratesio

## Overview

Publish CODA CLI to crates.io so that users can install it via `cargo install coda-cli`, which produces the `coda` binary. Three crates must be published in dependency order: `coda-pm` → `coda-core` → `coda-cli`.

The primary blocker is that built-in prompt templates are currently loaded at runtime via a path derived from `CARGO_MANIFEST_DIR`, which only exists at compile time in the source tree. After `cargo install`, this path does not exist on the user's machine, causing silent template loading failure. This must be fixed by embedding templates at compile time using `include_str!`.

## High-Level Design

```
┌──────────────────────────────────────────────────────────┐
│                   Template Loading Flow                   │
│                                                          │
│  coda-pm                                                 │
│  ├── templates/*.j2        (source files, included in    │
│  │                          crate package for builds)    │
│  └── src/builtin.rs        include_str!() all 12 .j2    │
│       │                     files at compile time        │
│       ▼                                                  │
│  PromptManager::with_builtin_templates()                 │
│       │                                                  │
│  coda-core/src/engine.rs                                 │
│       │  Replaces CARGO_MANIFEST_DIR path logic with     │
│       │  PromptManager::with_builtin_templates()         │
│       ▼                                                  │
│  extra_dirs overlay (unchanged)                          │
│       │  User custom templates still loaded from         │
│       │  filesystem at runtime                           │
│       ▼                                                  │
│  Fully loaded PromptManager                              │
└──────────────────────────────────────────────────────────┘
```

```
Publishing order (dependency chain):

  coda-pm (0.1.0)  ──►  coda-core (0.1.0)  ──►  coda-cli (0.1.0)
      │                       │                       │
      └── no deps on          └── depends on          └── depends on
          other coda              coda-pm                  coda-core,
          crates                                           coda-pm
```

## Interface Design

### `coda-pm`: New `builtin` module

```rust
// crates/coda-pm/src/builtin.rs

/// Returns a Vec of all built-in prompt templates, compiled into the binary.
pub fn builtin_templates() -> Vec<PromptTemplate> {
    vec![
        PromptTemplate::new("init/analyze_repo", include_str!("../templates/init/analyze_repo.j2")),
        PromptTemplate::new("init/system", include_str!("../templates/init/system.j2")),
        PromptTemplate::new("init/setup_project", include_str!("../templates/init/setup_project.j2")),
        PromptTemplate::new("plan/system", include_str!("../templates/plan/system.j2")),
        PromptTemplate::new("plan/approve", include_str!("../templates/plan/approve.j2")),
        PromptTemplate::new("plan/verification", include_str!("../templates/plan/verification.j2")),
        PromptTemplate::new("run/system", include_str!("../templates/run/system.j2")),
        PromptTemplate::new("run/dev_phase", include_str!("../templates/run/dev_phase.j2")),
        PromptTemplate::new("run/review", include_str!("../templates/run/review.j2")),
        PromptTemplate::new("run/verify", include_str!("../templates/run/verify.j2")),
        PromptTemplate::new("run/resume", include_str!("../templates/run/resume.j2")),
        PromptTemplate::new("run/create_pr", include_str!("../templates/run/create_pr.j2")),
    ]
}
```

### `coda-pm`: New `PromptManager` constructor

```rust
// crates/coda-pm/src/manager.rs

impl PromptManager {
    /// Creates a new PromptManager pre-loaded with all built-in templates.
    pub fn with_builtin_templates() -> Result<Self, PromptError> {
        let mut pm = Self::new();
        for template in crate::builtin::builtin_templates() {
            pm.add_template(template)?;
        }
        Ok(pm)
    }
}
```

### `coda-core`: Modified engine initialization

```rust
// crates/coda-core/src/engine.rs (lines ~142-154 replaced)

// Before:
//   let mut pm = PromptManager::new();
//   let builtin_templates_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
//       .parent().and_then(|p| p.parent())
//       .map(|p| p.join("crates/coda-pm/templates"));
//   if let Some(ref dir) = builtin_templates_dir && dir.exists() { ... }

// After:
let mut pm = PromptManager::with_builtin_templates()?;
```

## Directory Structure

```
crates/coda-pm/
├── src/
│   ├── builtin.rs              # NEW — include_str! for all 12 templates
│   ├── lib.rs                  # MODIFY — add `pub mod builtin;`
│   └── manager.rs              # MODIFY — add `with_builtin_templates()`
├── templates/                  # UNCHANGED — still needed for include_str! paths
│   └── ...                     #   and included in crate package
└── Cargo.toml                  # MODIFY — add metadata, include field

crates/coda-core/
├── src/
│   └── engine.rs               # MODIFY — replace CARGO_MANIFEST_DIR logic
└── Cargo.toml                  # MODIFY — add metadata

apps/coda-cli/
└── Cargo.toml                  # MODIFY — add metadata

Cargo.toml                      # MODIFY — update workspace metadata
LICENSE.md                      # MODIFY — update copyright holder
Makefile                        # MODIFY — add publish target
```

## Core Data Structures

No new data structures. Existing `PromptTemplate` and `PromptManager` are reused.

## Development Phases

### Phase 1: Embed built-in templates at compile time

- **Goal**: Templates are compiled into the binary so `cargo install` users get working prompts.
- **Tasks**:
  - Create `crates/coda-pm/src/builtin.rs` with `builtin_templates()` function using `include_str!` for all 12 `.j2` files
  - Add `pub mod builtin;` to `crates/coda-pm/src/lib.rs`
  - Add `PromptManager::with_builtin_templates()` constructor to `crates/coda-pm/src/manager.rs`
  - Modify `crates/coda-core/src/engine.rs` to use `PromptManager::with_builtin_templates()` instead of `CARGO_MANIFEST_DIR`-based path resolution (remove the `env!("CARGO_MANIFEST_DIR")` block entirely; keep the `extra_dirs` overlay loop unchanged)
  - Add test in `coda-pm` verifying `with_builtin_templates()` loads all 12 templates
  - Run `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`
- **Commit message**: `"feat(pm): embed built-in prompt templates at compile time"`

### Phase 2: Update publish metadata

- **Goal**: All Cargo.toml files satisfy crates.io publishing requirements.
- **Tasks**:
  - Update `Cargo.toml` (workspace):
    - `authors = ["chenlh <lehuagavin@gmail.com>"]`
    - `repository = "https://github.com/lehuagavin/coda"`
    - `homepage = "https://github.com/lehuagavin/coda"`
    - `keywords = ["claude", "ai", "agent", "development", "cli"]`
    - Remove or update `documentation` placeholder
  - Update `apps/coda-cli/Cargo.toml`:
    - Add `repository.workspace = true`, `homepage.workspace = true`, `readme = "../../README.md"`
    - Ensure `description` is meaningful for crates.io listing
  - Update `crates/coda-core/Cargo.toml`:
    - Add `repository.workspace = true`, `homepage.workspace = true`, `readme = "../../README.md"`
  - Update `crates/coda-pm/Cargo.toml`:
    - Add `repository.workspace = true`, `homepage.workspace = true`, `readme = "../../README.md"`
    - Add `include = ["src/**/*.rs", "templates/**/*.j2", "Cargo.toml", "../../LICENSE.md"]` to ensure templates are packaged
  - Update `LICENSE.md`: Update copyright holder to `chenlh` (or keep as-is if intentional)
  - Run `cargo build`, `cargo test`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`
- **Commit message**: `"chore: update crate metadata for crates.io publishing"`

### Phase 3: Configure publish settings and Makefile target

- **Goal**: Prevent accidental publishing of vendored crates; add automation target.
- **Tasks**:
  - Add `publish = false` to `vendors/claude-agent-sdk-rs/Cargo.toml` (prevent accidental publish of vendored copy)
  - Add `include` fields to `coda-core/Cargo.toml` and `coda-cli/Cargo.toml` to control packaged files
  - Add Makefile target `publish` that runs dry-run publish for all three crates in order:
    ```makefile
    .PHONY: publish publish-dry-run
    publish-dry-run:
    	cargo publish --dry-run -p coda-pm
    	cargo publish --dry-run -p coda-core
    	cargo publish --dry-run -p coda-cli

    publish:
    	cargo publish -p coda-pm
    	sleep 30
    	cargo publish -p coda-core
    	sleep 30
    	cargo publish -p coda-cli
    ```
  - Run `cargo build`, `cargo +nightly fmt`, `cargo clippy -- -D warnings`
- **Commit message**: `"chore: add publish configuration and Makefile targets"`

### Phase 4: Dry-run validation and publish

- **Goal**: Verify everything works end-to-end before and during publishing.
- **Tasks**:
  - Run `make publish-dry-run` — fix any issues
  - Run `cargo install --path apps/coda-cli` into a temp directory, verify `coda` binary starts and templates load correctly
  - Ensure crates.io authentication is configured (via `cargo login`, `CARGO_REGISTRY_TOKEN` env var, or `--token` flag)
  - Run `make publish` to publish all three crates in order
  - Verify post-publish: `cargo install coda-cli` from crates.io, test `coda` command
- **Commit message**: `"chore(release): publish v0.1.0 to crates.io"`

## Dependencies

No new crate dependencies required. `include_str!` is a Rust built-in macro.

## Risk & Trade-offs

| Risk | Impact | Mitigation |
|------|--------|------------|
| Templates embedded via `include_str!` must be maintained manually when adding/removing `.j2` files | Low — only 12 files, changes are infrequent | Add a comment in `builtin.rs` reminding to update when templates change; could add a test that verifies template count |
| `CARGO_MANIFEST_DIR` removal changes local dev experience | Low | Built-in templates are always available; `extra_dirs` config still supports filesystem overrides for development |
| crates.io index propagation delay between dependent publishes | Low | 30-second sleep between publishes in Makefile; can increase if needed |
| `coda-pm` package size increases slightly with embedded templates | Negligible | 12 small Jinja2 text files add minimal bytes |
| Post-publish bug requires `cargo yank` + new version | Medium | Phase 4 dry-run and local install verification reduces this risk significantly |