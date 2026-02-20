# Feature: support-slack

## Overview

Add Slack integration to CODA by creating a new `apps/coda-server` crate — a Socket Mode client that connects to Slack via outbound WebSocket, receives slash commands and message events, delegates to `coda-core` (Engine, PlanSession, Runner), and posts results back via Slack's Web API.

This enables all core CODA workflows (`init`, `plan`, `run`, `list`, `status`, `clean`) to be triggered from Slack channels. Users bind different channels to different repositories via `/coda bind`. The `plan` command runs as a multi-turn conversation inside a Slack thread. The `init` and `run` commands post live-updating progress messages. Designed for personal use on a local machine — no public URL, TLS certificate, or cloud deployment required.

## High-Level Design

```
┌───────────────────────────────────────────────────┐
│                  Slack Workspace                   │
│   /coda bind · /coda init · /coda plan · /coda run │
│   Thread replies for plan conversations            │
└────────────────────┬──────────────────────────────┘
                     │
┌────────────────────▼──────────────────────────────┐
│                  Slack Cloud                        │
│   Routes commands/events over WebSocket            │
└────────────────────┬──────────────────────────────┘
                     │ wss:// (outbound from server)
┌────────────────────▼──────────────────────────────┐
│              coda-server (new app)                  │
│                                                    │
│  ┌─────────────────────────────────────────────┐   │
│  │  SocketClient (tokio-tungstenite)           │   │
│  │  - Connect with xapp- token                 │   │
│  │  - Receive envelopes (commands/events)      │   │
│  │  - Acknowledge within 3s                    │   │
│  │  - Auto-reconnect on disconnect             │   │
│  └──────────────┬──────────────────────────────┘   │
│                 │                                   │
│  ┌──────────────▼──────────────────────────────┐   │
│  │  Dispatcher                                 │   │
│  │  - slash_commands → CommandHandler          │   │
│  │  - events_api     → EventHandler            │   │
│  │  - interactive    → InteractionHandler      │   │
│  └──────────────┬──────────────────────────────┘   │
│                 │                                   │
│  ┌──────────────▼──────────────────────────────┐   │
│  │  AppState (Arc)                             │   │
│  │  - BindingStore: channel_id → repo_path     │   │
│  │  - SessionManager: (ch, ts) → PlanSession   │   │
│  │  - SlackClient: bot_token + reqwest         │   │
│  └─────────────────────────────────────────────┘   │
└────────────────────┬───────────────────────────────┘
                     │
       ┌─────────────┼─────────────┐
  ┌────▼────┐  ┌────▼────┐  ┌────▼─────┐
  │coda-core│  │ coda-pm │  │claude-   │
  │ Engine  │  │PromptMgr│  │agent-sdk │
  └─────────┘  └─────────┘  └──────────┘
```

### Socket Mode Protocol

```
Server                          Slack
  │                               │
  │──── WSS connect (xapp-) ────►│
  │◄──── hello ──────────────────│
  │                               │
  │◄── envelope {                 │
  │      envelope_id: "abc",      │
  │      type: "slash_commands",  │
  │      payload: { command,      │
  │        text, channel_id, ... }│
  │    } ────────────────────────│
  │                               │
  │──── { envelope_id: "abc" } ──►│  (ack within 3s)
  │                               │
  │──── chat.postMessage ────────►│  (Web API, async)
  │◄──── ok ─────────────────────│
  │                               │
  │◄──── ping ───────────────────│
  │──── pong ────────────────────►│
```

Every envelope must be acknowledged within 3 seconds by sending back its `envelope_id`. Actual business logic runs asynchronously after the ack.

### Command Flow

1. Slack delivers envelope via WebSocket
2. Server acks immediately (envelope_id)
3. Dispatcher routes by envelope type: `slash_commands` / `events_api` / `interactive`
4. Command handler resolves channel → repo binding from `BindingStore`
5. Creates `Engine` for the bound repo path
6. Calls the appropriate `coda-core` method (init/plan/run/list/status/clean)
7. Translates events/results into Slack messages via `SlackClient`

### Plan Session Lifecycle

1. `/coda plan <slug>` → create thread parent message → initialize `PlanSession` → store in `SessionManager`
2. Thread messages arrive via Events API → lookup `SessionManager` by `(channel_id, thread_ts)`
3. Forward text to `PlanSession::send()` → post reply in thread
4. `approve` keyword → `PlanSession::approve()` → post design + verification (as file snippets if long)
5. `done` keyword → `PlanSession::finalize()` → post summary with artifact paths
6. `quit` keyword → `PlanSession::disconnect()` → post confirmation
7. Background reaper disconnects sessions idle >30 minutes

### Progress Update Strategy

For `init` and `run`, events arrive at high frequency. Slack's `chat.update` rate limit is ~1 call/sec/channel.

- Phase-level transitions (`PhaseStarting`, `PhaseCompleted`, `PhaseFailed`) trigger an immediate `chat.update`
- High-frequency events (`AgentTextDelta`, `TurnCompleted`, `ToolActivity`) are batched; the message is updated at most every 3 seconds
- The initial message is posted with `chat.postMessage`; all subsequent updates rewrite the same message via `chat.update` using the returned `ts`

### Long Content Handling

Slack Block Kit limits a single section to 3000 characters. Design specs and verification plans typically exceed this.

- Content ≤3000 chars → inline in a Block Kit section
- Content >3000 chars → upload via `files.upload` as a text snippet attached to the thread
- A short summary message is posted alongside the file

## Interface Design

### Slash Command Parsing

```rust
/// Parsed subcommand from `/coda <text>`.
pub enum CodaCommand {
    Bind { repo_path: String },
    Unbind,
    Init,
    Plan { feature_slug: String },
    Run { feature_slug: String },
    List,
    Status { feature_slug: String },
    Clean,
    Help,
}
```

### SlackClient (thin Web API wrapper)

```rust
/// Thin async client for Slack Web API methods used by coda-server.
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
}

impl SlackClient {
    pub async fn post_message(&self, channel: &str, blocks: Vec<serde_json::Value>) -> Result<PostMessageResponse>;
    pub async fn update_message(&self, channel: &str, ts: &str, blocks: Vec<serde_json::Value>) -> Result<()>;
    pub async fn post_thread_reply(&self, channel: &str, thread_ts: &str, text: &str) -> Result<String>;
    pub async fn upload_file(&self, channel: &str, thread_ts: Option<&str>, content: &str, title: &str, filetype: &str) -> Result<()>;
    pub async fn add_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<()>;
    pub async fn remove_reaction(&self, channel: &str, ts: &str, name: &str) -> Result<()>;
    pub async fn connections_open(&self, app_token: &str) -> Result<String>; // returns wss:// URL
}

pub struct PostMessageResponse {
    pub ts: String,
    pub channel: String,
}
```

### SocketClient (Socket Mode connection)

```rust
/// Manages the WebSocket connection to Slack Socket Mode.
pub struct SocketClient {
    app_token: String,
    slack: SlackClient,
}

impl SocketClient {
    /// Connects and runs the event loop until shutdown.
    /// Calls `handler` for each received envelope.
    /// Auto-reconnects on disconnect with exponential backoff (1s → 30s cap).
    pub async fn run<F, Fut>(&self, handler: F, shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()>
    where
        F: Fn(Envelope) -> Fut + Send + Sync,
        Fut: Future<Output = ()> + Send;
}

/// A Socket Mode envelope received from Slack.
pub struct Envelope {
    pub envelope_id: String,
    pub envelope_type: EnvelopeType,
    pub payload: serde_json::Value,
}

pub enum EnvelopeType {
    SlashCommands,
    EventsApi,
    Interactive,
    Disconnect,
}
```

### AppState

```rust
/// Shared application state, passed as Arc<AppState> to all handlers.
pub struct AppState {
    pub slack: SlackClient,
    pub bindings: BindingStore,
    pub sessions: SessionManager,
    pub running_tasks: RunningTasks,
}
```

### BindingStore

```rust
/// Manages channel_id → repo_path mappings.
/// Persisted to ~/.coda-server/config.yml on every mutation.
pub struct BindingStore {
    config_path: PathBuf,
    bindings: DashMap<String, PathBuf>,
}

impl BindingStore {
    pub fn get(&self, channel_id: &str) -> Option<PathBuf>;
    pub fn set(&self, channel_id: &str, repo_path: PathBuf) -> Result<()>;
    pub fn remove(&self, channel_id: &str) -> Result<bool>;
}
```

### SessionManager

```rust
/// Tracks active PlanSessions keyed by (channel_id, thread_ts).
pub struct SessionManager {
    sessions: DashMap<(String, String), ActiveSession>,
}

struct ActiveSession {
    session: PlanSession,
    last_activity: Instant,
}

impl SessionManager {
    pub fn insert(&self, channel: String, thread_ts: String, session: PlanSession);
    pub fn get_mut(&self, channel: &str, thread_ts: &str) -> Option<RefMut<(String, String), ActiveSession>>;
    pub fn remove(&self, channel: &str, thread_ts: &str);
    /// Disconnects sessions idle longer than `max_idle`. Returns count of reaped sessions.
    pub async fn reap_idle(&self, max_idle: Duration) -> usize;
}
```

### RunningTasks

```rust
/// Tracks in-flight init/run tasks to prevent duplicates.
pub struct RunningTasks {
    tasks: DashMap<String, tokio::task::JoinHandle<()>>,  // key: "init:{channel}" or "run:{slug}"
}

impl RunningTasks {
    pub fn is_running(&self, key: &str) -> bool;
    pub fn insert(&self, key: String, handle: tokio::task::JoinHandle<()>);
    pub fn remove(&self, key: &str);
}
```

## Directory Structure

```
apps/coda-server/                 # NEW — entire crate
├── Cargo.toml
└── src/
    ├── main.rs                   # Entrypoint: load config → connect socket → event loop
    ├── config.rs                 # ServerConfig: slack tokens, bindings, idle timeout
    ├── state.rs                  # AppState: bindings, sessions, slack client, running tasks
    ├── slack_client.rs           # Slack Web API wrapper (reqwest)
    ├── socket.rs                 # Socket Mode: connect, receive, ack, reconnect
    ├── dispatch.rs               # Envelope type → handler routing
    ├── handlers/
    │   ├── mod.rs
    │   ├── commands.rs           # Slash command parsing and dispatch
    │   ├── events.rs             # Events API (plan thread message routing)
    │   └── interactions.rs       # Button click handling (clean confirm)
    ├── commands/
    │   ├── mod.rs
    │   ├── bind.rs               # /coda bind, /coda unbind
    │   ├── init.rs               # InitEvent → debounced Slack message updates
    │   ├── plan.rs               # PlanSession lifecycle + thread routing
    │   ├── run.rs                # RunEvent → debounced Slack message updates
    │   └── query.rs              # /coda list, status, clean
    ├── session.rs                # DashMap-based PlanSession manager + reaper
    ├── formatter.rs              # Block Kit message builders
    └── error.rs                  # ServerError types (thiserror)

docs/slack-setup.md               # NEW — Slack App setup guide
Cargo.toml                        # MODIFIED — add coda-server to workspace members
```

## Core Data Structures

### Server Configuration

```rust
/// Top-level server configuration loaded from ~/.coda-server/config.yml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub slack: SlackConfig,
    #[serde(default)]
    pub bindings: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    pub app_token: String,
    pub bot_token: String,
}
```

### Slack API Types

```rust
/// Slash command payload from Slack.
#[derive(Debug, Deserialize)]
pub struct SlashCommandPayload {
    pub command: String,
    pub text: String,
    pub channel_id: String,
    pub channel_name: String,
    pub user_id: String,
    pub user_name: String,
    pub response_url: String,
    pub trigger_id: String,
}

/// Message event from Events API.
#[derive(Debug, Deserialize)]
pub struct MessageEvent {
    pub channel: String,
    pub user: Option<String>,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
}

/// Block Kit action from interactive messages.
#[derive(Debug, Deserialize)]
pub struct InteractionPayload {
    pub actions: Vec<Action>,
    pub channel: ChannelInfo,
    pub user: UserInfo,
    pub message: MessageInfo,
}
```

### Formatter Output

```rust
/// Builds Block Kit JSON arrays for different message types.
pub struct Formatter;

impl Formatter {
    /// Progress message for init (updated in-place).
    pub fn init_progress(phases: &[InitPhaseDisplay]) -> Vec<serde_json::Value>;

    /// Progress message for run (updated in-place).
    pub fn run_progress(feature_slug: &str, phases: &[RunPhaseDisplay]) -> Vec<serde_json::Value>;

    /// Feature list table.
    pub fn feature_list(features: &[FeatureState]) -> Vec<serde_json::Value>;

    /// Detailed feature status.
    pub fn feature_status(state: &FeatureState) -> Vec<serde_json::Value>;

    /// Plan thread parent message.
    pub fn plan_thread_header(feature_slug: &str, phase: &str) -> Vec<serde_json::Value>;

    /// Clean candidates with confirm button.
    pub fn clean_candidates(candidates: &[CleanedWorktree]) -> Vec<serde_json::Value>;

    /// Error message.
    pub fn error(message: &str) -> Vec<serde_json::Value>;
}

pub struct InitPhaseDisplay {
    pub name: String,
    pub status: PhaseDisplayStatus,
    pub duration: Option<Duration>,
    pub cost_usd: Option<f64>,
}

pub struct RunPhaseDisplay {
    pub name: String,
    pub status: PhaseDisplayStatus,
    pub duration: Option<Duration>,
    pub turns: Option<u32>,
    pub cost_usd: Option<f64>,
}

pub enum PhaseDisplayStatus {
    Pending,
    Running,
    Completed,
    Failed,
}
```

## Development Phases

### Phase 1: Server Foundation & Socket Connection

- **Goal**: Bootable server that connects to Slack Socket Mode, receives slash commands, and manages channel-repo bindings
- **Tasks**:
  - Add `apps/coda-server` to workspace `Cargo.toml` members
  - `config.rs` — load `~/.coda-server/config.yml` with `ServerConfig`
  - `error.rs` — `ServerError` enum with `thiserror`
  - `slack_client.rs` — `SlackClient` with `post_message`, `update_message`, `post_thread_reply`, `upload_file`, `add_reaction`, `remove_reaction`, `connections_open`
  - `socket.rs` — `apps.connections.open` → WSS connect → receive/ack loop → ping/pong → exponential backoff reconnect (1s → 2s → 4s → ... → 30s cap)
  - `dispatch.rs` — parse envelope type, route to handler
  - `state.rs` — `AppState` with `BindingStore` and `SlackClient`
  - `handlers/commands.rs` — parse `/coda <text>` into `CodaCommand` enum, dispatch
  - `commands/bind.rs` — `bind` persists to config.yml, `unbind` removes entry
  - `commands/query.rs` — stub `/coda help` that lists available commands
  - `main.rs` — load config, build `AppState`, start socket, graceful shutdown on SIGINT/SIGTERM
  - `docs/slack-setup.md` — Slack App creation and configuration guide
- **Commit message**: `feat(server): add coda-server with Socket Mode connection and channel binding`

### Phase 2: Synchronous Commands

- **Goal**: `list`, `status`, `clean` working end-to-end through Slack
- **Tasks**:
  - `formatter.rs` — Block Kit builders for feature list, status detail, clean candidates, error messages
  - `commands/query.rs` — resolve binding → create `Engine` → call `list_features()` / `feature_status()` / `scan_cleanable_worktrees()` → format → post
  - `handlers/interactions.rs` — handle clean confirm button click → call `Engine::remove_worktrees()` → update message
  - Error formatting: no binding for channel, feature not found, `.trees/` missing
- **Commit message**: `feat(server): add list, status, and clean commands with Block Kit formatting`

### Phase 3: Init & Run with Live Progress

- **Goal**: Long-running `init` and `run` commands with real-time Slack message updates
- **Tasks**:
  - `state.rs` — add `RunningTasks` to `AppState` for tracking in-flight operations
  - `commands/init.rs` — spawn tokio task, subscribe to `InitEvent` via mpsc channel, debounce `chat.update` (every 3s, immediate on phase transitions), post final summary
  - `commands/run.rs` — spawn tokio task, subscribe to `RunEvent` via mpsc channel, debounce `chat.update`, handle `ReviewRound`/`VerifyAttempt` sub-events, post final summary with PR link
  - Duplicate prevention: reject `/coda run <slug>` if already running for that slug
  - `formatter.rs` — add `init_progress()` and `run_progress()` builders
- **Commit message**: `feat(server): add init and run commands with live progress updates`

### Phase 4: Plan Interactive Thread

- **Goal**: Multi-turn planning conversation in Slack threads
- **Tasks**:
  - `session.rs` — `SessionManager` with `DashMap<(String, String), ActiveSession>`, `insert`, `get_mut`, `remove`, `reap_idle`
  - `state.rs` — add `SessionManager` to `AppState`
  - `commands/plan.rs` — create thread parent via `post_message`, initialize `PlanSession`, store in `SessionManager`
  - `handlers/events.rs` — handle `message` events: skip bot messages, match `thread_ts` to active session, route text to `PlanSession::send()`, post reply in thread
  - Plan commands in thread: `approve` → `PlanSession::approve()` → post design + verification (file snippet if >3000 chars), `done` → `PlanSession::finalize()` → post summary, `quit` → disconnect
  - ⏳ reaction as thinking indicator: `add_reaction` before `PlanSession::send()`, `remove_reaction` after
  - Update thread parent message with phase indicator (Discussing → Approved → Finalized)
  - Background reaper task: `tokio::spawn` loop every 5 min, call `SessionManager::reap_idle(Duration::from_secs(1800))`
  - `formatter.rs` — add `plan_thread_header()` builder
- **Commit message**: `feat(server): add interactive plan sessions in Slack threads`

## Slack Setup Guide

Delivered as `docs/slack-setup.md` in Phase 1.

### Step 1: Create Slack App

1. Go to [api.slack.com/apps](https://api.slack.com/apps) → **Create New App** → **From scratch**
2. App Name: `CODA`, select your Workspace → **Create App**

### Step 2: Enable Socket Mode

1. Left menu → **Socket Mode** → toggle **Enable Socket Mode**
2. Generate App-Level Token: name `coda-socket`, scope `connections:write`
3. Copy the `xapp-...` token

### Step 3: Create Slash Command

1. Left menu → **Slash Commands** → **Create New Command**
2. Command: `/coda`, Description: `CODA - AI Development Agent`, Usage Hint: `[bind|init|plan|run|list|status|clean] [args]`

### Step 4: Subscribe to Events

1. Left menu → **Event Subscriptions** → toggle **Enable Events**
2. Under **Subscribe to bot events** add: `message.channels`, `message.groups`, `message.im` (optional)

### Step 5: Configure Bot Permissions

Under **OAuth & Permissions** → **Bot Token Scopes**, ensure:

| Scope | Purpose |
|-------|---------|
| `commands` | Receive slash commands |
| `chat:write` | Send and update messages |
| `files:write` | Upload long content as snippets |
| `reactions:write` | ⏳ thinking indicator |
| `channels:history` | Read public channel messages (plan threads) |
| `groups:history` | Read private channel messages (plan threads) |
| `im:history` | Read DM messages (optional) |

### Step 6: Install to Workspace

1. Left menu → **Install App** → **Install to Workspace** → Authorize
2. Copy the **Bot User OAuth Token** (`xoxb-...`)

### Step 7: Configure coda-server

```bash
mkdir -p ~/.coda-server
cat > ~/.coda-server/config.yml << 'EOF'
slack:
  app_token: "xapp-1-..."
  bot_token: "xoxb-..."
bindings: {}
EOF
chmod 600 ~/.coda-server/config.yml
```

### Step 8: Start & Verify

```bash
cargo run -p coda-server
# INFO coda_server: Loading config from ~/.coda-server/config.yml
# INFO coda_server::socket: Connecting to Slack Socket Mode...
# INFO coda_server::socket: Connected. Waiting for events...
```

Test in Slack:
```
/coda bind /path/to/your/repo
/coda list
/coda plan add-auth
```

### Step 9: Invite Bot to Channel

The bot must be in the channel to receive messages: type `/invite @CODA` in the target channel.

### Troubleshooting

| Problem | Solution |
|---------|----------|
| `/coda` no response | Check server is running; logs should show `Connected` |
| `not_in_channel` error | `/invite @CODA` in the channel |
| Bot ignores plan thread replies | Check Event Subscriptions has `message.channels` enabled |
| `invalid_auth` error | Confirm `bot_token` starts with `xoxb-`, `app_token` starts with `xapp-` |
| Frequent disconnects | Check network; server logs should show automatic reconnection |

## Dependencies

New dependencies for `apps/coda-server`:

| Crate | Version | Purpose |
|-------|---------|---------|
| `tokio-tungstenite` | `0.24` | WebSocket client for Socket Mode (`features = ["rustls-tls-webpki-roots"]`) |
| `reqwest` | `0.12` | Slack Web API HTTP client (`features = ["json", "rustls-tls"]`) |
| `dashmap` | `6.1` | Concurrent session and task storage |

Workspace dependencies already available: `tokio`, `serde`, `serde_json`, `serde_yaml`, `tracing`, `tracing-subscriber`, `anyhow`, `thiserror`, `futures`, `chrono`, `coda-core`, `coda-pm`.

## Risk & Trade-offs

| Risk | Mitigation |
|------|-----------|
| **WebSocket disconnect** | Exponential backoff reconnect (1s → 30s cap); Slack re-delivers un-acked envelopes |
| **`chat.update` rate limit** (~1/sec/channel) | Debounce: batch high-frequency events, update every 3s; phase transitions trigger immediate update |
| **PlanSession memory leak** (abandoned sessions) | Background reaper disconnects sessions idle >30 min every 5 min |
| **Server restart loses active plan sessions** | Acceptable for personal use; `init`/`run` have `state.yml` crash recovery and can be resumed |
| **Long content exceeds Slack limits** (3000 chars/section) | Content >3000 chars uploaded as file snippets via `files.upload` |
| **Requires local git/gh/Claude CLI** | Server must run on a machine with these tools installed and configured |
| **Socket Mode not suitable for large scale** | By design — personal use only; team scale would require HTTP webhook mode |
| **No auth beyond Slack app tokens** | Acceptable for personal use; anyone in the Slack workspace with access to the channel can invoke commands |