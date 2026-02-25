//! Agent profile configuration for Claude Agent SDK.
//!
//! Defines `AgentProfile` which maps CODA's task types to
//! `ClaudeAgentOptions` configurations. Two profiles exist:
//!
//! - **Planner**: Read-only tools for analysis and planning.
//! - **Coder**: Full tool access with safety hooks for development.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use code_agent_sdk::options::{HookCallback, HookJSONOutput, SystemPromptConfig, ToolsConfig};
use code_agent_sdk::{ClaudeAgentOptions, HookEvent, HookMatcher, PermissionMode};
use regex::Regex;
use tracing::debug;

/// Agent profile controlling tool access and SDK configuration.
///
/// Hardcoded by the engine (convention-based), not user-configurable.
/// Tool requirements are inherent properties of each task type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentProfile {
    /// Read-only profile for analysis and planning.
    /// Tools: `Read`, `Glob`, `Grep`.
    Planner,

    /// Full-access profile for coding, testing, and deployment.
    /// Tools: `Read`, `Write`, `Bash`, `Glob`, `Grep` + safety hooks.
    Coder,
}

impl AgentProfile {
    /// Converts this profile into `ClaudeAgentOptions` for the SDK.
    ///
    /// Both profiles use the `claude_code` system prompt preset with
    /// custom appended instructions, and `BypassPermissions` mode.
    pub fn to_options(
        &self,
        system_append: &str,
        cwd: PathBuf,
        max_turns: u32,
        max_budget_usd: f64,
        model: &str,
    ) -> ClaudeAgentOptions {
        let system_prompt = SystemPromptConfig::Preset {
            preset: "claude_code".to_string(),
            append: Some(system_append.to_string()),
        };

        let mut options = match self {
            Self::Planner => ClaudeAgentOptions::builder()
                .permission_mode(PermissionMode::BypassPermissions)
                .cwd(cwd)
                .max_turns(max_turns)
                .max_budget_usd(max_budget_usd)
                .model(model.to_string())
                .tools(ToolsConfig::from(["Read", "Glob", "Grep"]))
                .build(),

            Self::Coder => ClaudeAgentOptions::builder()
                .permission_mode(PermissionMode::BypassPermissions)
                .cwd(cwd)
                .max_turns(max_turns)
                .max_budget_usd(max_budget_usd)
                .model(model.to_string())
                .tools(ToolsConfig::from(["Read", "Write", "Bash", "Glob", "Grep"]))
                .hooks(build_safety_hooks())
                .build(),
        };
        // Set system prompt directly (builder only supports String, not Preset)
        options.system_prompt = Some(system_prompt);
        options
    }
}

/// Dangerous command patterns that the safety hook will deny.
const DANGEROUS_PATTERNS: &[&str] = &[
    r"rm\s+-rf\s+/",
    r"git\s+push\s+--force",
    r"git\s+push\s+-f\b",
    r"DROP\s+TABLE",
    r"DROP\s+DATABASE",
    r"mkfs\.",
    r"dd\s+if=.+of=/dev/",
    r">\s*/dev/sda",
    r"chmod\s+-R\s+777\s+/",
    r":\(\)\s*\{\s*:\|:\s*&\s*\}\s*;",
];

/// Builds safety hooks for the Coder profile.
///
/// - **`PreToolUse`** with `"Bash"` matcher: Checks commands against
///   dangerous patterns and denies them.
/// - **`PostToolUse`** (all tools): Logs tool name and result via `tracing::debug!`.
pub fn build_safety_hooks() -> HashMap<HookEvent, Vec<HookMatcher>> {
    let mut hooks = HashMap::new();

    // PreToolUse: intercept dangerous Bash commands
    let pre_hook: HookCallback = Arc::new(
        |input: serde_json::Value, _tool_use_id: Option<String>, _context| {
            Box::pin(async move {
                let command = input
                    .get("tool_input")
                    .and_then(|v| v.get("command"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if is_dangerous_command(command) {
                    debug!(command = command, "Blocked dangerous Bash command");
                    return Ok(HookJSONOutput::Sync {
                        decision: Some("deny".to_string()),
                        reason: Some(format!("Command blocked by safety hook: {command}")),
                        hook_specific_output: Some(serde_json::json!({
                            "permission_decision": "deny",
                            "permission_decision_reason":
                                "Dangerous command detected by CODA safety hook",
                        })),
                        continue_: None,
                        suppress_output: None,
                        stop_reason: None,
                        system_message: None,
                    });
                }

                Ok(HookJSONOutput::Sync {
                    continue_: None,
                    suppress_output: None,
                    stop_reason: None,
                    decision: None,
                    system_message: None,
                    reason: None,
                    hook_specific_output: None,
                })
            })
        },
    );

    hooks.insert(
        HookEvent::PreToolUse,
        vec![HookMatcher {
            matcher: Some("Bash".to_string()),
            hooks: vec![pre_hook],
            timeout: None,
        }],
    );

    // PostToolUse: log all tool executions
    let post_hook: HookCallback = Arc::new(
        |input: serde_json::Value, _tool_use_id: Option<String>, _context| {
            Box::pin(async move {
                if let Some(tool_name) = input.get("tool_name").and_then(|v| v.as_str()) {
                    debug!(tool_name, "Tool execution completed");
                }

                Ok(HookJSONOutput::Sync {
                    continue_: None,
                    suppress_output: None,
                    stop_reason: None,
                    decision: None,
                    system_message: None,
                    reason: None,
                    hook_specific_output: None,
                })
            })
        },
    );

    hooks.insert(
        HookEvent::PostToolUse,
        vec![HookMatcher {
            matcher: None,
            hooks: vec![post_hook],
            timeout: None,
        }],
    );

    hooks
}

/// Pre-compiled dangerous command regexes, initialized once on first access.
static DANGEROUS_REGEXES: std::sync::LazyLock<Vec<Regex>> = std::sync::LazyLock::new(|| {
    DANGEROUS_PATTERNS
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
});

/// Checks whether a command matches any dangerous pattern.
fn is_dangerous_command(command: &str) -> bool {
    DANGEROUS_REGEXES.iter().any(|re| re.is_match(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_detect_dangerous_rm_rf() {
        assert!(is_dangerous_command("rm -rf /"));
        assert!(is_dangerous_command("sudo rm -rf / --no-preserve-root"));
    }

    #[test]
    fn test_should_detect_dangerous_git_force_push() {
        assert!(is_dangerous_command("git push --force"));
        assert!(is_dangerous_command("git push -f origin main"));
    }

    #[test]
    fn test_should_detect_dangerous_drop_table() {
        assert!(is_dangerous_command("DROP TABLE users"));
        assert!(is_dangerous_command("DROP DATABASE production"));
    }

    #[test]
    fn test_should_allow_safe_commands() {
        assert!(!is_dangerous_command("cargo build"));
        assert!(!is_dangerous_command("git status"));
        assert!(!is_dangerous_command("ls -la"));
        assert!(!is_dangerous_command("echo hello"));
    }

    #[test]
    fn test_should_build_safety_hooks() {
        let hooks = build_safety_hooks();
        assert!(hooks.contains_key(&HookEvent::PreToolUse));
        assert!(hooks.contains_key(&HookEvent::PostToolUse));

        // PreToolUse should have a Bash matcher
        let pre_matchers = &hooks[&HookEvent::PreToolUse];
        assert_eq!(pre_matchers.len(), 1);
        assert_eq!(pre_matchers[0].matcher, Some("Bash".to_string()));

        // PostToolUse should have no matcher (matches all)
        let post_matchers = &hooks[&HookEvent::PostToolUse];
        assert_eq!(post_matchers.len(), 1);
        assert_eq!(post_matchers[0].matcher, None);
    }

    #[test]
    fn test_should_create_planner_options() {
        let profile = AgentProfile::Planner;
        let options = profile.to_options(
            "Test append",
            PathBuf::from("/tmp"),
            10,
            5.0,
            "claude-opus-4-6",
        );

        assert_eq!(options.max_turns, Some(10));
        assert_eq!(options.max_budget_usd, Some(5.0));
        assert_eq!(options.model, Some("claude-opus-4-6".to_string()));
        assert_eq!(
            options.permission_mode,
            Some(PermissionMode::BypassPermissions)
        );
        assert!(options.hooks.is_none());

        match options.tools {
            Some(ToolsConfig::List(tools)) => {
                assert!(tools.contains(&"Read".to_string()));
                assert!(tools.contains(&"Glob".to_string()));
                assert!(tools.contains(&"Grep".to_string()));
                assert!(!tools.contains(&"Write".to_string()));
                assert!(!tools.contains(&"Bash".to_string()));
            }
            _ => panic!("Expected ToolsConfig::List for Planner"),
        }
    }

    #[test]
    fn test_should_create_coder_options() {
        let profile = AgentProfile::Coder;
        let options = profile.to_options(
            "Test append",
            PathBuf::from("/tmp"),
            20,
            10.0,
            "claude-opus-4-6",
        );

        assert_eq!(options.max_turns, Some(20));
        assert_eq!(options.max_budget_usd, Some(10.0));
        assert_eq!(options.model, Some("claude-opus-4-6".to_string()));
        assert!(options.hooks.is_some());

        match options.tools {
            Some(ToolsConfig::List(tools)) => {
                assert!(tools.contains(&"Read".to_string()));
                assert!(tools.contains(&"Write".to_string()));
                assert!(tools.contains(&"Bash".to_string()));
                assert!(tools.contains(&"Glob".to_string()));
                assert!(tools.contains(&"Grep".to_string()));
            }
            _ => panic!("Expected ToolsConfig::List for Coder"),
        }
    }
}
