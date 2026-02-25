//! Claude CLI discovery and version checking.

use crate::error::{Error, Result};
use crate::options::AgentOptions;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

#[allow(dead_code)]
const MINIMUM_CLAUDE_CODE_VERSION: &str = "2.0.0";

/// Find the Claude CLI binary path using a prioritized search strategy.
///
/// Search order:
/// 1. Explicit `cli_path` from options
/// 2. Bundled CLI in executable directory (`_bundled/claude`)
/// 3. PATH environment variable
/// 4. Common installation paths
///
/// # Errors
///
/// Returns [`Error::CliNotFound`] if no Claude CLI binary can be located.
pub fn find_cli(options: &AgentOptions) -> Result<String> {
    if let Some(ref p) = options.cli_path {
        return Ok(p.to_string_lossy().to_string());
    }

    if let Some(bundled) = find_bundled_cli() {
        return Ok(bundled);
    }

    if let Some(path) = which_cli() {
        return Ok(path);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let locations = [
        format!("{}/.npm-global/bin/claude", home),
        "/usr/local/bin/claude".to_string(),
        format!("{}/.local/bin/claude", home),
        format!("{}/node_modules/.bin/claude", home),
        format!("{}/.yarn/bin/claude", home),
        format!("{}/.claude/local/claude", home),
    ];

    for path in &locations {
        if Path::new(path).exists() {
            return Ok(path.clone());
        }
    }

    Err(Error::CliNotFound(
        "Claude Code not found. Install with:\n  npm install -g @anthropic-ai/claude-code\n\n\
         Or provide the path via AgentOptions::cli_path()"
            .to_string(),
    ))
}

fn find_bundled_cli() -> Option<String> {
    let cli_name = if cfg!(target_os = "windows") {
        "claude.exe"
    } else {
        "claude"
    };

    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let bundled = dir.join("_bundled").join(cli_name);

    if bundled.exists() {
        Some(bundled.to_string_lossy().to_string())
    } else {
        None
    }
}

fn which_cli() -> Option<String> {
    std::env::var_os("PATH").and_then(|paths| {
        for path in std::env::split_paths(&paths) {
            let full = path.join(if cfg!(target_os = "windows") {
                "claude.exe"
            } else {
                "claude"
            });
            if full.is_file() {
                return Some(full.to_string_lossy().to_string());
            }
        }
        None
    })
}

/// Check the Claude CLI version and warn if below minimum.
pub async fn check_claude_version(cli_path: &str) {
    let result: std::result::Result<(), ()> = async {
        let child = Command::new(cli_path)
            .arg("-v")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ())?;

        let output = child.wait_with_output().await.map_err(|_| ())?;
        let version_output = String::from_utf8_lossy(&output.stdout).trim().to_string();

        let version_re_match: Option<&str> = version_output
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .next()
            .filter(|s| s.contains('.'));

        if let Some(version_str) = version_re_match {
            let version_parts: Vec<u32> = version_str
                .split('.')
                .filter_map(|s| s.parse().ok())
                .collect();
            let min_parts: Vec<u32> = MINIMUM_CLAUDE_CODE_VERSION
                .split('.')
                .filter_map(|s| s.parse().ok())
                .collect();

            if version_parts < min_parts {
                let warning = format!(
                    "Warning: Claude Code version {} is unsupported in the Agent SDK. \
                     Minimum required version is {}. Some features may not work correctly.",
                    version_str, MINIMUM_CLAUDE_CODE_VERSION
                );
                tracing::warn!("{}", warning);
                eprintln!("{}", warning);
            }
        }
        Ok(())
    }
    .await;

    let _ = result;
}
