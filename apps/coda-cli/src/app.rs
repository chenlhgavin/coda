//! Application state and command dispatch logic.
//!
//! The `App` struct wraps the core [`Engine`](coda_core::Engine) and
//! dispatches CLI commands to the appropriate engine methods, providing
//! user-facing progress display and timing information.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use coda_core::{Engine, InitEvent, RunEvent};
use tracing::{error, info};

use crate::fmt_utils::{format_duration, truncate_str};
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
    /// Runs the init flow with a live TUI showing streaming AI output,
    /// phase progress, elapsed time, and cost. Uses `tokio::select!` to
    /// run the engine and UI concurrently (same pattern as `run_tui`).
    ///
    /// Generated files are **not** auto-committed. The user must commit
    /// them manually before running `coda plan`, which requires init
    /// artifacts to be present on the base branch.
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails (e.g., already initialized,
    /// agent SDK errors) or the user cancels with Ctrl+C.
    pub async fn init(&self, no_commit: bool, force: bool) -> Result<()> {
        // Pre-flight: fail fast before launching TUI
        let project_root = self.engine.project_root();
        if project_root.join(".coda").exists() && !force {
            println!("Project already initialized. .coda/ directory exists.");
            println!("Run `coda init --force` to reinitialize.");
            println!("Run `coda plan <feature-slug>` to start planning a feature.");
            return Ok(());
        }

        let project_root_display = project_root.display().to_string();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<InitEvent>();

        let mut ui = crate::init_ui::InitUi::new(&project_root_display)?;

        let engine_future = self.engine.init(no_commit, force, Some(tx));

        tokio::pin!(engine_future);

        let mut engine_result: Option<Result<(), coda_core::CoreError>> = None;

        // Run UI and engine concurrently: UI drives the event loop,
        // engine sends events through the channel.
        let ui_result = {
            let mut ui_future = std::pin::pin!(ui.run(rx));
            loop {
                tokio::select! {
                    // Bias engine branch so its result is always captured
                    // before the UI branch can break the loop.
                    biased;

                    result = &mut engine_future, if engine_result.is_none() => {
                        engine_result = Some(result);
                        // Engine done — channel sender dropped, UI will see Disconnected
                        // and finish on its own. Continue the loop to let UI drain.
                    }
                    ui_res = &mut ui_future => {
                        break ui_res;
                    }
                }
            }
        };

        // If engine completed but select! picked the UI branch first (both
        // were ready simultaneously), await the engine future now — it will
        // return immediately since it already completed.
        // Guard: skip if UI errored (e.g. Ctrl+C) to avoid blocking on the
        // engine future while the user wants to cancel.
        if engine_result.is_none() && ui_result.is_ok() {
            engine_result = Some(engine_future.await);
        }

        // Drop UI to restore terminal before printing final output
        drop(ui);

        // Print final summary to normal stdout
        match (engine_result, ui_result) {
            (Some(Ok(())), Ok(summary)) => {
                println!();
                println!("  CODA Init: {project_root_display}");
                println!("  ═══════════════════════════════════════");
                println!(
                    "  Total: {} elapsed, ${:.4} USD",
                    format_duration(summary.elapsed),
                    summary.total_cost,
                );
                println!("  ═══════════════════════════════════════");
                println!();
                if force {
                    println!("  Updated .coda/config.yml");
                    println!("  Regenerated .coda.md repository overview");
                } else {
                    println!("  Created .coda/ directory with config.yml");
                    println!("  Created .trees/ directory for worktrees");
                    println!("  Generated .coda.md repository overview");
                }
                println!();
                if no_commit {
                    println!("  Commit the init artifacts before running `coda plan`:");
                    println!("    git add .coda/ .coda.md CLAUDE.md .gitignore");
                    println!("    git commit -m \"chore: initialize CODA project\"");
                } else {
                    println!("  Init artifacts have been committed automatically.");
                }
                println!(
                    "\nNext step: run `coda plan <feature-slug>` to start planning a feature."
                );
                Ok(())
            }
            (Some(Err(e)), _) => {
                error!("Init failed: {e}");
                println!();
                println!("  CODA Init: {project_root_display} — FAILED");
                println!("  ═══════════════════════════════════════");
                println!("  Error: {e}");
                println!("  ═══════════════════════════════════════");
                println!();
                Err(e.into())
            }
            (_, Err(_)) => {
                // UI cancelled (e.g. Ctrl+C) — show friendly message
                info!("Init cancelled by user");
                println!();
                println!("  CODA Init: {project_root_display} — cancelled");
                println!("  Run `coda init` again to retry.");
                println!();
                Ok(())
            }
            (None, _) => {
                // Should never happen: engine_future is awaited above as fallback
                Err(anyhow::anyhow!("Engine did not complete"))
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
    /// Lists all features (active and merged) with their status, branch,
    /// and cost summary. Active features are shown first, followed by
    /// merged features separated by a divider.
    ///
    /// # Errors
    ///
    /// Returns an error if the feature list cannot be read.
    pub fn list(&self) -> Result<()> {
        use coda_core::state::FeatureStatus;

        let features = self.engine.list_features()?;

        if features.is_empty() {
            println!("No features found. Run `coda plan <feature-slug>` to create one.");
            return Ok(());
        }

        let (active, merged): (Vec<_>, Vec<_>) = features
            .iter()
            .partition(|f| f.status != FeatureStatus::Merged);

        println!();
        println!(
            "  {:<28} {:<14} {:<28} {:>8} {:>8}",
            "Feature", "Status", "Branch", "Turns", "Cost"
        );
        println!("  {}", "─".repeat(90));

        for f in &active {
            print_feature_row(f);
        }

        if !active.is_empty() && !merged.is_empty() {
            println!("  {}", "─".repeat(90));
        }

        for f in &merged {
            print_feature_row(f);
        }

        println!();
        let active_count = active.len();
        let merged_count = merged.len();
        let total = active_count + merged_count;
        match (active_count, merged_count) {
            (0, _) => println!("  {merged_count} merged — {total} feature(s) total"),
            (_, 0) => println!("  {active_count} active — {total} feature(s) total"),
            _ => println!(
                "  {active_count} active, {merged_count} merged — {total} feature(s) total"
            ),
        }
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
    /// When `--logs` is set, removes all feature log directories under
    /// `.coda/<slug>/logs/` and returns early without touching worktrees.
    ///
    /// Otherwise, scans all worktrees, checks their PR status, and removes
    /// worktrees whose PR has been merged or closed. Supports `--dry-run`
    /// to preview and `--yes` to skip confirmation.
    ///
    /// # Errors
    ///
    /// Returns an error if the clean operation fails.
    pub fn clean(&self, dry_run: bool, yes: bool, logs: bool) -> Result<()> {
        if logs {
            return self.clean_logs();
        }

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

    /// Handles the `coda clean --logs` command.
    ///
    /// Removes all feature log directories and prints the results.
    fn clean_logs(&self) -> Result<()> {
        println!();
        println!("  Cleaning feature log directories...");
        println!();

        let cleaned = self.engine.clean_logs()?;

        if cleaned.is_empty() {
            println!("  No feature logs to clean.");
        } else {
            for slug in &cleaned {
                println!("  [✓] Removed logs for {slug}");
            }
            println!();
            println!("  Cleaned logs for {} feature(s).", cleaned.len());
        }

        println!();
        Ok(())
    }

    /// Handles the `coda run <feature_slug>` command.
    ///
    /// Executes all remaining phases and displays real-time phase-by-phase
    /// progress. When `no_tui` is false (default), uses an interactive
    /// ratatui TUI with spinner animation and live timers. When `no_tui`
    /// is true, falls back to plain text output suitable for CI/pipelines.
    ///
    /// # Errors
    ///
    /// Returns an error if the run fails.
    pub async fn run(&self, feature_slug: &str, no_tui: bool) -> Result<()> {
        if no_tui {
            return self.run_plain(feature_slug).await;
        }

        self.run_tui(feature_slug).await
    }

    /// Runs with interactive TUI display.
    async fn run_tui(&self, feature_slug: &str) -> Result<()> {
        // Pre-load phase state from state.yml so the first TUI frame
        // already shows completed phases as green with correct metrics
        // (avoids a gray-flash and wrong totals on resume).
        let initial_phases = self
            .engine
            .feature_status(feature_slug)
            .map(|state| {
                state
                    .phases
                    .iter()
                    .map(|p| crate::run_ui::InitialPhase {
                        name: p.name.clone(),
                        completed: p.status == coda_core::state::PhaseStatus::Completed,
                        duration: Duration::from_secs(p.duration_secs),
                        turns: p.turns,
                        cost_usd: p.cost_usd,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<RunEvent>();

        let mut ui = crate::run_ui::RunUi::new(feature_slug, initial_phases)?;

        let slug = feature_slug.to_string();

        // We cannot move `engine` into a spawned task (it's borrowed),
        // so we run the engine concurrently via `tokio::select!`.
        let engine_future = self.engine.run(&slug, Some(tx));

        tokio::pin!(engine_future);

        let mut engine_result: Option<Result<Vec<coda_core::TaskResult>, coda_core::CoreError>> =
            None;

        // Run UI and engine concurrently: UI drives the event loop,
        // engine sends events through the channel.
        let ui_result = {
            let mut ui_future = std::pin::pin!(ui.run(rx));
            loop {
                tokio::select! {
                    // Bias engine branch so its result is always captured
                    // before the UI branch can break the loop.
                    biased;

                    result = &mut engine_future, if engine_result.is_none() => {
                        engine_result = Some(result);
                        // Engine done — channel sender dropped, UI will see Disconnected
                        // and finish on its own. Continue the loop to let UI drain.
                    }
                    ui_res = &mut ui_future => {
                        break ui_res;
                    }
                }
            }
        };

        // If engine completed but select! picked the UI branch first (both
        // were ready simultaneously), await the engine future now — it will
        // return immediately since it already completed.
        // Guard: skip if UI errored (e.g. Ctrl+C) to avoid blocking on the
        // engine future while the user wants to cancel.
        if engine_result.is_none() && ui_result.is_ok() {
            engine_result = Some(engine_future.await);
        }

        // Drop UI to restore terminal before printing final output
        drop(ui);

        // Print final summary to normal stdout
        match (engine_result, ui_result) {
            (Some(Ok(_results)), Ok(summary)) => {
                println!();
                if summary.success {
                    println!("  CODA Run: {feature_slug}");
                } else {
                    println!("  CODA Run: {feature_slug} — COMPLETED (PR failed)");
                }
                println!("  ═══════════════════════════════════════");
                println!(
                    "  Total: {} elapsed, {} turns, ${:.4} USD",
                    format_duration(summary.elapsed),
                    summary.total_turns,
                    summary.total_cost,
                );
                if let Some(url) = &summary.pr_url {
                    println!("  PR: {url}");
                } else if !summary.success {
                    println!("  PR creation failed. Create manually:");
                    println!("    gh pr create --base main --head feature/{feature_slug}");
                }
                println!("  ═══════════════════════════════════════");
                println!();
                Ok(())
            }
            (Some(Err(e)), _) => {
                error!("Run failed: {e}");
                println!();
                println!("  CODA Run: {feature_slug} — FAILED");
                println!("  ═══════════════════════════════════════");
                println!("  Error: {e}");
                println!("  ═══════════════════════════════════════");
                println!();
                Err(e.into())
            }
            (_, Err(_)) => {
                // UI cancelled (e.g. Ctrl+C) — show friendly message with resume hint
                info!("Run cancelled by user");
                println!();
                println!("  CODA Run: {feature_slug} — cancelled");
                println!("  Progress has been saved. Run `coda run {feature_slug}` to resume.");
                println!();
                Ok(())
            }
            (None, _) => {
                // Should never happen: engine_future is awaited above as fallback
                Err(anyhow::anyhow!("Engine did not complete"))
            }
        }
    }

    /// Runs with plain text output (for CI/pipelines).
    async fn run_plain(&self, feature_slug: &str) -> Result<()> {
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
                    RunEvent::ReviewerCompleted {
                        reviewer,
                        issues_found,
                    } => {
                        if issues_found == 0 {
                            println!("      [{reviewer}] review: passed");
                        } else {
                            println!("      [{reviewer}] review: {issues_found} issue(s)");
                        }
                    }
                    RunEvent::ReviewRound {
                        round,
                        max_rounds,
                        issues_found,
                    } => {
                        if issues_found == 0 {
                            println!("      Review round {round}/{max_rounds}: passed");
                        } else {
                            println!(
                                "      Review round {round}/{max_rounds}: {issues_found} issue(s), fixing..."
                            );
                        }
                    }
                    RunEvent::VerifyAttempt {
                        attempt,
                        max_attempts,
                        passed,
                    } => {
                        if passed {
                            println!(
                                "      Verify attempt {attempt}/{max_attempts}: all checks passed"
                            );
                        } else {
                            println!(
                                "      Verify attempt {attempt}/{max_attempts}: failures found, fixing..."
                            );
                        }
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
                    RunEvent::Connecting => {
                        println!("  [▸] Connecting to Claude...");
                    }
                    RunEvent::StderrOutput { line } => {
                        eprintln!("  [stderr] {}", line.trim());
                    }
                    RunEvent::IdleWarning {
                        attempt,
                        max_retries,
                        idle_secs,
                    } => {
                        println!(
                            "  [warn] Agent idle for {idle_secs}s — retry {attempt}/{max_retries}",
                        );
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
                let mut pr_failed = false;

                for result in &results {
                    total_turns += result.turns;
                    total_cost += result.cost_usd;
                    if matches!(&result.task, coda_core::Task::CreatePr { .. })
                        && !matches!(&result.status, coda_core::TaskStatus::Completed)
                    {
                        pr_failed = true;
                    }
                }

                println!();
                println!("  ─────────────────────────────────────");
                println!(
                    "  Total: {} elapsed, {total_turns} turns, ${total_cost:.4} USD",
                    format_duration(total_elapsed)
                );
                if pr_failed {
                    println!("  PR creation failed. Create manually:");
                    println!("    gh pr create --base main --head feature/{feature_slug}");
                }
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

fn print_feature_row(f: &coda_core::state::FeatureState) {
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

fn feature_status_icon(s: coda_core::state::FeatureStatus) -> &'static str {
    match s {
        coda_core::state::FeatureStatus::Planned => "○",
        coda_core::state::FeatureStatus::InProgress => "◐",
        coda_core::state::FeatureStatus::Completed => "●",
        coda_core::state::FeatureStatus::Failed => "✗",
        coda_core::state::FeatureStatus::Merged => "✔",
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
