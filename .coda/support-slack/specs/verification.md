# Verification Plan: support-slack

## Automated Checks

- [ ] All pre-configured checks pass:
  - [ ] `cargo build`
  - [ ] `cargo +nightly fmt -- --check`
  - [ ] `cargo clippy -- -D warnings`
- [ ] All new tests pass (`cargo test -p coda-server`)
- [ ] No regression in existing tests (`cargo test --workspace`)
- [ ] `cargo deny check` passes with new dependencies

## Functional Verification

### Config & Binding

- [ ] **Scenario: Load valid config**
  - Input: `~/.coda-server/config.yml` with valid `app_token`, `bot_token`, and bindings
  - Expected: Server starts, logs config loaded, connects to Slack

- [ ] **Scenario: Load config with pre-existing bindings**
  - Input: Config with `bindings: { C123: "/path/to/repo" }`
  - Expected: `BindingStore` initializes with the mapping; `/coda list` in channel C123 works immediately

- [ ] **Scenario: `/coda bind /path/to/repo`**
  - Input: Slash command in a channel
  - Expected: Binding persisted to config.yml, reply confirms `✅ Bound to /path/to/repo`

- [ ] **Scenario: `/coda bind` with non-existent path**
  - Input: `/coda bind /no/such/path`
  - Expected: Error reply indicating the path does not exist; binding not saved

- [ ] **Scenario: `/coda unbind`**
  - Input: Slash command in a bound channel
  - Expected: Binding removed from config.yml, reply confirms removal

- [ ] **Scenario: `/coda unbind` in unbound channel**
  - Input: Slash command in a channel with no binding
  - Expected: Reply indicating no binding exists

### Socket Mode Connection

- [ ] **Scenario: Successful connection**
  - Input: Valid `app_token`
  - Expected: `apps.connections.open` returns WSS URL, WebSocket connects, `hello` received, logs `Connected`

- [ ] **Scenario: Reconnect after disconnect**
  - Input: WebSocket connection drops (network interruption)
  - Expected: Server detects disconnect, waits with exponential backoff, reconnects, logs reconnection

- [ ] **Scenario: Envelope acknowledgment**
  - Input: Any slash command envelope arrives
  - Expected: `envelope_id` sent back within 3 seconds, command processed asynchronously

- [ ] **Scenario: Invalid app_token**
  - Input: Malformed or expired `xapp-` token
  - Expected: `apps.connections.open` fails, server logs error, retries with backoff

### Synchronous Commands

- [ ] **Scenario: `/coda list` with features**
  - Input: Channel bound to a repo with planned features in `.trees/`
  - Expected: Block Kit message with feature table (slug, status, branch, turns, cost)

- [ ] **Scenario: `/coda list` with no features**
  - Input: Channel bound to a repo with empty `.trees/`
  - Expected: Message saying "No features found. Run `/coda plan <slug>` to create one."

- [ ] **Scenario: `/coda status <slug>`**
  - Input: Valid feature slug
  - Expected: Detailed Block Kit message with git info, phase progress, cost breakdown

- [ ] **Scenario: `/coda status <nonexistent>`**
  - Input: Feature slug that does not exist
  - Expected: Error message listing available features

- [ ] **Scenario: `/coda clean`**
  - Input: Repo with worktrees whose PRs are merged
  - Expected: Message listing candidates with a confirm button

- [ ] **Scenario: Clean confirm button clicked**
  - Input: User clicks confirm button from `/coda clean` message
  - Expected: Worktrees removed, message updated with removal results

- [ ] **Scenario: Command in unbound channel**
  - Input: `/coda list` in a channel with no binding
  - Expected: Error message: "No repository bound to this channel. Use `/coda bind /path/to/repo` first."

- [ ] **Scenario: `/coda help`**
  - Input: `/coda help` or `/coda` with no arguments
  - Expected: Message listing all available subcommands with usage hints

### Init Command

- [ ] **Scenario: `/coda init` on uninitialized repo**
  - Input: Channel bound to a repo without `.coda/`
  - Expected: Initial progress message posted, phases update in-place, final summary with cost/duration

- [ ] **Scenario: `/coda init` on already initialized repo**
  - Input: Channel bound to a repo with existing `.coda/`
  - Expected: Error message: "Project already initialized"

- [ ] **Scenario: Init progress updates**
  - Input: Init running with two phases
  - Expected: Message updated showing phase 1 ✅ → phase 2 ⏳ → phase 2 ✅ → final summary

### Run Command

- [ ] **Scenario: `/coda run <slug>` on planned feature**
  - Input: Feature with status `Planned` and valid `state.yml`
  - Expected: Progress message posted, phases update in-place, PR link in final summary

- [ ] **Scenario: Run progress shows phase transitions**
  - Input: Feature with 3 dev phases + review + verify
  - Expected: Each phase transitions Pending → Running → Completed in the Slack message

- [ ] **Scenario: `/coda run <slug>` while already running**
  - Input: Same slug already in `RunningTasks`
  - Expected: Error message: "Feature `<slug>` is already running"

- [ ] **Scenario: `/coda run <nonexistent>`**
  - Input: Feature slug with no `state.yml`
  - Expected: Error message with available features hint

- [ ] **Scenario: Run with review/verify sub-events**
  - Input: Review finds issues, verify has retries
  - Expected: ReviewRound and VerifyAttempt events reflected in progress message

### Plan Command

- [ ] **Scenario: `/coda plan <slug>` creates thread**
  - Input: New feature slug
  - Expected: Thread parent message posted with header and instructions, PlanSession stored in SessionManager

- [ ] **Scenario: User sends message in plan thread**
  - Input: Text message in the plan thread
  - Expected: ⏳ reaction added, `PlanSession::send()` called, reply posted in thread, reaction removed

- [ ] **Scenario: `approve` in plan thread**
  - Input: User sends "approve" in thread
  - Expected: Design spec and verification plan generated, posted as file snippets if >3000 chars, thread parent updated to show "Approved"

- [ ] **Scenario: `done` in plan thread after approval**
  - Input: User sends "done" after approve
  - Expected: `PlanSession::finalize()` called, worktree created, summary posted with artifact paths, session removed from SessionManager

- [ ] **Scenario: `done` before `approve`**
  - Input: User sends "done" without prior approve
  - Expected: Error reply: "Design has not been approved yet. Send `approve` first."

- [ ] **Scenario: `quit` in plan thread**
  - Input: User sends "quit"
  - Expected: Session disconnected, confirmation reply, session removed from SessionManager

- [ ] **Scenario: `/coda plan <existing-slug>`**
  - Input: Feature slug that already exists in `.trees/`
  - Expected: Error message indicating the feature already exists

- [ ] **Scenario: Bot ignores its own messages in thread**
  - Input: Bot posts a reply in the plan thread
  - Expected: Events handler checks `bot_id` field, skips processing

### Formatter

- [ ] **Scenario: Feature list Block Kit output**
  - Input: List of `FeatureState` with various statuses
  - Expected: Valid Block Kit JSON with status icons, aligned columns

- [ ] **Scenario: Run progress Block Kit output**
  - Input: Mix of Pending, Running, Completed phases
  - Expected: Valid Block Kit JSON with ⬚/⏳/✅/✗ status indicators

- [ ] **Scenario: Error Block Kit output**
  - Input: Error message string
  - Expected: Valid Block Kit JSON with error formatting

## Edge Cases

- [ ] `/coda bind` with trailing slash in path — should normalize
- [ ] `/coda bind` with relative path — should reject, require absolute path
- [ ] `/coda plan` with invalid slug (uppercase, spaces, special chars) — should return validation error from `validate_feature_slug()`
- [ ] Slash command with extra whitespace — `/coda   plan   add-auth` — should parse correctly
- [ ] Empty slash command text — `/coda` with no arguments — should show help
- [ ] Unknown subcommand — `/coda foo` — should show help with available commands
- [ ] Very long agent response in plan thread (>3000 chars) — should upload as file snippet, not inline
- [ ] Agent response exactly at 3000 char boundary — should inline without file upload
- [ ] Multiple concurrent `/coda run` for different features — should both run independently
- [ ] Plan session idle timeout — session left idle >30 minutes — reaper disconnects and removes it
- [ ] Thread message after session reaped — should reply "Planning session has expired. Start a new one with `/coda plan <slug>`."
- [ ] WebSocket receives `disconnect` envelope type — should cleanly reconnect without error
- [ ] Config file missing on startup — should create default config with empty bindings
- [ ] Config file has invalid YAML — should fail with clear error message on startup
- [ ] Slack Web API returns error (e.g., `channel_not_found`) — should log error, not crash server
- [ ] Slack Web API rate limited (429) — should log warning, retry with backoff

## Integration Points

- [ ] `Engine::new()` creates successfully from bound repo path
- [ ] `Engine::init()` emits `InitEvent`s through the mpsc channel correctly
- [ ] `Engine::run()` emits `RunEvent`s through the mpsc channel correctly
- [ ] `Engine::plan()` returns a valid `PlanSession`
- [ ] `PlanSession::send()` returns agent response text
- [ ] `PlanSession::approve()` returns `(design, verification)` tuple
- [ ] `PlanSession::finalize()` creates worktree and returns `PlanOutput`
- [ ] `Engine::list_features()` results serialize correctly into Block Kit format
- [ ] `Engine::feature_status()` results serialize correctly into Block Kit format
- [ ] `Engine::scan_cleanable_worktrees()` and `Engine::remove_worktrees()` work through Slack interaction flow
- [ ] `BindingStore` mutations are persisted and survive server restart
- [ ] Workspace `Cargo.toml` includes `coda-server` and builds without dependency conflicts
- [ ] `docs/slack-setup.md` is present and linked from `docs/index.md`

## Performance

- [ ] `chat.update` debouncing: during a `/coda run`, Slack API is called at most once per 3 seconds for high-frequency events (text deltas, tool activity)
- [ ] Phase-level events (`PhaseCompleted`, `PhaseFailed`) trigger immediate `chat.update` regardless of debounce timer
- [ ] WebSocket reconnection uses exponential backoff (1s → 2s → 4s → ... capped at 30s), not tight-loop retries
- [ ] `SessionManager::reap_idle()` completes quickly even with many active sessions (DashMap iteration is non-blocking)
- [ ] `Engine` instances are created on-demand per command and not held in memory between commands

## Security

- [ ] `app_token` and `bot_token` are never logged (even at debug/trace level)
- [ ] Config file at `~/.coda-server/config.yml` is created with `chmod 600` permissions (documented in setup guide)
- [ ] Bot ignores messages from other bots (check `bot_id` field) to prevent message loops
- [ ] `/coda bind` validates that the path exists and is a directory before accepting
- [ ] No `unwrap()` or `expect()` in production code paths
- [ ] Slack Web API calls use `rustls` (not native-tls/OpenSSL) via reqwest's `rustls-tls` feature
- [ ] WebSocket connection uses `rustls-tls-webpki-roots` (not native TLS) via tokio-tungstenite