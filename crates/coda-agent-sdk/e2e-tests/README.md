# End-to-End Tests for Code Agent SDK (Rust)

End-to-end tests that run against the actual Claude API.

## Requirements

### API Key (REQUIRED)

These tests require a valid Anthropic API key. They will **fail** if `ANTHROPIC_API_KEY` is not set.

```bash
export ANTHROPIC_API_KEY="your-api-key-here"
```

## Running the Tests

### Run all e2e tests (requires API key):

```bash
cargo test --test test_e2e -- --ignored
```

### Run a specific e2e test:

```bash
cargo test --test test_e2e test_sdk_mcp_tool_execution -- --ignored
```

### Run unit tests only (skip e2e):

```bash
cargo test
```

## Cost Considerations

⚠️ **Important**: These tests make actual API calls to Claude, which incur costs.

- Each test typically uses 1-3 API calls
- Tests use simple prompts to minimize token usage
- The complete test suite should cost less than $0.10 to run

## Test Coverage

- **test_sdk_mcp_tools**: SDK MCP tool execution, permission enforcement
- **test_hooks**: PreToolUse, PostToolUse hooks with permission decision
- **test_dynamic_control**: set_permission_mode, set_model, interrupt
