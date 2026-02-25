//! Transport tests - corresponds to Python test_transport.py
//!
//! NOTE: SubprocessCliTransport and _build_command are not yet implemented.
//! These tests verify options structure for transport configuration.

use coda_agent_sdk::AgentOptions;
use std::path::Path;

fn make_options(cli_path: &str) -> AgentOptions {
    AgentOptions::builder().cli_path(cli_path).build()
}

#[test]
fn test_options_with_cwd() {
    let options = AgentOptions::builder()
        .cli_path("/usr/bin/claude")
        .cwd("/custom/path")
        .build();
    assert_eq!(
        options.cli_path.as_deref(),
        Some(Path::new("/usr/bin/claude"))
    );
    assert_eq!(options.cwd.as_deref(), Some(Path::new("/custom/path")));
}

#[test]
fn test_cli_path_accepts_pathbuf() {
    let path = std::path::PathBuf::from("/usr/bin/claude");
    let options = AgentOptions::builder().cli_path(path.clone()).build();
    assert_eq!(options.cli_path.as_ref(), Some(&path));
}

#[test]
fn test_build_command_options_structure() {
    // Verify options needed for _build_command are available
    let options = make_options("/usr/bin/claude");
    assert_eq!(
        options.cli_path.as_deref(),
        Some(Path::new("/usr/bin/claude"))
    );
    assert!(options.allowed_tools.is_empty());
    assert!(options.system_prompt.is_none());
}

#[test]
fn test_options_with_all_transport_params() {
    let options = AgentOptions::builder()
        .cli_path("/usr/bin/claude")
        .cwd("/project")
        .system_prompt("Be helpful")
        .allowed_tools(["Read", "Write"])
        .disallowed_tools(["Bash"])
        .model("claude-sonnet-4-5")
        .permission_mode("acceptEdits")
        .max_turns(5)
        .build();

    assert_eq!(options.allowed_tools, vec!["Read", "Write"]);
    assert_eq!(options.disallowed_tools, vec!["Bash"]);
    assert_eq!(options.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(
        options.permission_mode,
        Some(coda_agent_sdk::PermissionMode::AcceptEdits)
    );
    assert_eq!(options.max_turns, Some(5));
}
