//! Application state and command dispatch logic.
//!
//! The `App` struct wraps the core [`Engine`](coda_core::Engine) and
//! dispatches CLI commands to the appropriate engine methods, providing
//! user-facing progress display and timing information.

use std::io::Write;
use std::time::Instant;

use anyhow::Result;
use coda_core::{Engine, RunEvent};
use tracing::error;

use crate::ui::PlanUi;

/// Application state holding the core engine.
pub struct App {
    /// Core execution engine for CODA operations.
    engine: Engine,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").field("engine", &self.engine).finish()
    }
}

impl App {
    /// Creates a new application instance by discovering the project root
    /// and initializing the engine.
    ///
    /// # Errors
    ///
    /// Returns an error if the project root cannot be found or the engine
    /// fails to initialize.
    pub async fn new() -> Result<Self> {
        let project_root = coda_core::find_project_root().or_else(|_| {
            std::env::current_dir().map_err(|e| {
                coda_core::CoreError::ConfigError(format!("Cannot determine project root: {e}"))
            })
        })?;

        let engine = Engine::new(project_root).await?;
        Ok(Self { engine })
    }

    /// Handles the `coda init` command.
    ///
    /// Runs the init flow (analyze repo + setup project) and prints a
    /// success message.
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails (e.g., already initialized,
    /// agent SDK errors).
    pub async fn init(&self) -> Result<()> {
        println!(
            "Initializing CODA in {}...",
            self.engine.project_root().display()
        );
        match self.engine.init().await {
            Ok(()) => {
                println!("CODA project initialized successfully!");
                println!("  Created .coda/ directory with config.yml");
                println!("  Created .trees/ directory for worktrees");
                println!("  Generated .coda.md repository overview");
                println!(
                    "\nNext step: run `coda plan <feature-slug>` to start planning a feature."
                );
                Ok(())
            }
            Err(e) => {
                error!("Init failed: {e}");
                Err(e.into())
            }
        }
    }

    /// Handles the `coda plan <feature_slug>` command.
    ///
    /// Opens an interactive ratatui chat interface for multi-turn
    /// conversation with the planning agent. When the user types `/done`,
    /// the session is finalized and artifacts are generated.
    ///
    /// # Errors
    ///
    /// Returns an error if the planning session or UI fails.
    pub async fn plan(&self, feature_slug: &str) -> Result<()> {
        let mut session = self.engine.plan(feature_slug)?;
        let mut ui = PlanUi::new()?;

        match ui.run_plan(&mut session).await? {
            Some(output) => {
                // UI is dropped here (restores terminal)
                drop(ui);
                println!("Planning complete!");
                println!("  Design spec: {}", output.design_spec.display());
                println!("  Verification: {}", output.verification.display());
                println!("  State: {}", output.state.display());
                println!("  Worktree: {}", output.worktree.display());
                println!("\nNext step: run `coda run {feature_slug}` to execute the plan.");
            }
            None => {
                drop(ui);
                println!("Planning cancelled.");
            }
        }

        Ok(())
    }

    /// Handles the `coda list` command.
    ///
    /// Lists all planned features with their status, branch, and cost summary.
    ///
    /// # Errors
    ///
    /// Returns an error if the feature list cannot be read.
    pub fn list(&self) -> Result<()> {
        let features = self.engine.list_features()?;

        if features.is_empty() {
            println!("No features found. Run `coda plan <feature-slug>` to create one.");
            return Ok(());
        }

        println!();
        println!(
            "  {:<28} {:<14} {:<28} {:>8} {:>8}",
            "Feature", "Status", "Branch", "Turns", "Cost"
        );
        println!("  {}", "─".repeat(90));

        for f in &features {
            let status_icon = feature_status_icon(f.status);

            println!(
                "  {:<28} {status_icon} {:<12} {:<28} {:>8} {:>8}",
                truncate_str(&f.feature.slug, 28),
                f.status,
                truncate_str(&f.git.branch, 28),
                f.total.turns,
                format!("${:.4}", f.total.cost_usd),
            );
        }

        println!();
        println!("  {} feature(s) total", features.len());
        println!();

        Ok(())
    }

    /// Handles the `coda status <feature_slug>` command.
    ///
    /// Shows detailed information about a specific feature including
    /// git info, phase progress, cost breakdown, and PR info.
    ///
    /// # Errors
    ///
    /// Returns an error if the feature is not found.
    pub fn status(&self, feature_slug: &str) -> Result<()> {
        let state = self.engine.feature_status(feature_slug)?;

        let status_icon = feature_status_icon(state.status);

        println!();
        println!("  Feature: {}", state.feature.slug);
        println!("  ═══════════════════════════════════════");
        println!();
        println!("  Status:     {status_icon} {}", state.status);
        println!(
            "  Created:    {}",
            state.feature.created_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!(
            "  Updated:    {}",
            state.feature.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        println!();
        println!("  Git");
        println!("  ─────────────────────────────────────");
        println!("  Branch:     {}", state.git.branch);
        println!("  Base:       {}", state.git.base_branch);
        println!("  Worktree:   {}", state.git.worktree_path.display());

        if !state.phases.is_empty() {
            println!();
            println!("  Phases");
            println!("  ─────────────────────────────────────");
            println!(
                "  {:<12} {:<12} {:>8} {:>8} {:>10}",
                "Phase", "Status", "Turns", "Cost", "Duration"
            );

            for phase in &state.phases {
                let phase_icon = phase_status_icon(phase.status);

                println!(
                    "  {:<12} {phase_icon} {:<10} {:>8} {:>8} {:>10}",
                    phase.name,
                    phase.status,
                    phase.turns,
                    format!("${:.4}", phase.cost_usd),
                    format_duration(std::time::Duration::from_secs(phase.duration_secs)),
                );
            }
        }

        if let Some(ref pr) = state.pr {
            println!();
            println!("  Pull Request");
            println!("  ─────────────────────────────────────");
            println!("  #{}: {}", pr.number, pr.title);
            println!("  URL: {}", pr.url);
        }

        println!();
        println!("  Summary");
        println!("  ─────────────────────────────────────");
        println!("  Total turns:    {}", state.total.turns);
        println!("  Total cost:     ${:.4} USD", state.total.cost_usd);
        println!(
            "  Total duration: {}",
            format_duration(std::time::Duration::from_secs(state.total.duration_secs))
        );
        println!(
            "  Tokens:         {} in / {} out",
            state.total.cost.input_tokens, state.total.cost.output_tokens
        );
        println!("  ═══════════════════════════════════════");
        println!();

        Ok(())
    }

    /// Handles the `coda clean` command.
    ///
    /// Scans all worktrees, checks their PR status, and removes worktrees
    /// whose PR has been merged or closed. Supports `--dry-run` to preview
    /// and `--yes` to skip confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error if the clean operation fails.
    pub fn clean(&self, dry_run: bool, yes: bool) -> Result<()> {
        println!();
        println!("  Scanning worktrees for merged/closed PRs...");
        println!();

        let candidates = self.engine.scan_cleanable_worktrees()?;

        if candidates.is_empty() {
            println!("  No worktrees to clean up.");
            println!();
            return Ok(());
        }

        for c in &candidates {
            let pr_info = c
                .pr_number
                .map_or_else(String::new, |n| format!(" (PR #{n})"));
            println!("  [~] {:<28} {:<10}{pr_info}", c.slug, c.pr_state,);
        }

        if dry_run {
            println!();
            println!(
                "  {} worktree(s) would be removed. Run without --dry-run to proceed.",
                candidates.len()
            );
            println!();
            return Ok(());
        }

        if !yes {
            println!();
            print!("  Remove {} worktree(s)? [y/N] ", candidates.len());
            std::io::stdout().flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                println!("  Aborted.");
                println!();
                return Ok(());
            }
        }

        let removed = self.engine.remove_worktrees(&candidates)?;

        println!();
        for result in &removed {
            let pr_info = result
                .pr_number
                .map_or_else(String::new, |n| format!(" (PR #{n})"));
            println!(
                "  [✓] Removed {:<28} {:<10}{pr_info}",
                result.slug, result.pr_state,
            );
        }

        println!();
        println!("  Cleaned {} worktree(s).", removed.len());
        println!();
        Ok(())
    }

    /// Handles the `coda run <feature_slug>` command.
    ///
    /// Executes all remaining phases and displays real-time phase-by-phase
    /// progress with timing information and a final summary.
    ///
    /// # Errors
    ///
    /// Returns an error if the run fails.
    pub async fn run(&self, feature_slug: &str) -> Result<()> {
        let run_start = Instant::now();

        println!();
        println!("  CODA Run: {feature_slug}");
        println!("  ═══════════════════════════════════════");
        println!();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RunEvent>();

        // Spawn a lightweight task to display progress events in real-time
        let display_handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    RunEvent::RunStarting { phases } => {
                        let pipeline = phases
                            .iter()
                            .map(String::as_str)
                            .chain(std::iter::once("PR"))
                            .collect::<Vec<_>>()
                            .join(" → ");
                        println!("  Phases: {pipeline}");
                        println!();
                    }
                    RunEvent::PhaseStarting { name, index, total } => {
                        println!("  [▸] {name:<24} Running...  ({}/{})", index + 1, total);
                    }
                    RunEvent::PhaseCompleted {
                        name,
                        duration,
                        turns,
                        cost_usd,
                        ..
                    } => {
                        println!(
                            "  [✓] {name:<24} {duration:>8}  {turns:>3} turns  ${cost_usd:.4}",
                            duration = format_duration(duration),
                        );
                    }
                    RunEvent::PhaseFailed { name, error, .. } => {
                        println!("  [✗] {name:<24} Failed");
                        println!("      Error: {error}");
                    }
                    RunEvent::CreatingPr => {
                        println!("  [▸] create-pr              Running...");
                    }
                    RunEvent::PrCreated { url } => {
                        if let Some(url) = url {
                            println!("  [✓] create-pr              PR: {url}");
                        } else {
                            println!("  [✗] create-pr              No PR created");
                            println!("      Try manually: gh pr create --head <branch>");
                        }
                    }
                    _ => {}
                }
            }
        });

        // Run the engine in the current task (sender is dropped when done)
        let run_result = self.engine.run(feature_slug, Some(tx)).await;

        // Wait for all progress events to be displayed
        let _ = display_handle.await;

        match run_result {
            Ok(results) => {
                let total_elapsed = run_start.elapsed();
                let mut total_turns = 0u32;
                let mut total_cost = 0.0f64;

                for result in &results {
                    total_turns += result.turns;
                    total_cost += result.cost_usd;
                }

                println!();
                println!("  ─────────────────────────────────────");
                println!(
                    "  Total: {} elapsed, {total_turns} turns, ${total_cost:.4} USD",
                    format_duration(total_elapsed)
                );
                println!("  ═══════════════════════════════════════");
                println!();

                Ok(())
            }
            Err(e) => {
                let elapsed = run_start.elapsed();
                error!("Run failed after {}: {e}", format_duration(elapsed));
                println!();
                println!("  Run failed after {}", format_duration(elapsed));
                println!("  ═══════════════════════════════════════");
                println!();
                Err(e.into())
            }
        }
    }
}

fn feature_status_icon(s: coda_core::state::FeatureStatus) -> &'static str {
    match s {
        coda_core::state::FeatureStatus::Planned => "○",
        coda_core::state::FeatureStatus::InProgress => "◐",
        coda_core::state::FeatureStatus::Completed => "●",
        coda_core::state::FeatureStatus::Failed => "✗",
        _ => "?",
    }
}

fn phase_status_icon(s: coda_core::state::PhaseStatus) -> &'static str {
    match s {
        coda_core::state::PhaseStatus::Pending => "○",
        coda_core::state::PhaseStatus::Running => "◐",
        coda_core::state::PhaseStatus::Completed => "●",
        coda_core::state::PhaseStatus::Failed => "✗",
        _ => "?",
    }
}

/// Formats a `Duration` into a human-readable string (e.g., `"1m 23s"`, `"45s"`).
fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    if total_secs >= 3600 {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        let secs = total_secs % 60;
        format!("{hours}h {mins}m {secs}s")
    } else if total_secs >= 60 {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{mins}m {secs}s")
    } else {
        format!("{total_secs}s")
    }
}

/// Truncates a string to fit within `max_len` characters, appending `…` if needed.
///
/// Uses character boundaries instead of byte offsets to avoid panics on
/// multi-byte UTF-8 sequences (e.g., CJK characters, emoji).
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
