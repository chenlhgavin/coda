# Slack App Setup Guide for coda-server

This guide walks through creating and configuring a Slack App for use with `coda-server`.

## Step 1: Create Slack App

1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click **Create New App** > **From scratch**
2. App Name: `CODA`, select your Workspace, then click **Create App**

## Step 2: Enable Socket Mode

1. Left menu: **Socket Mode** > toggle **Enable Socket Mode**
2. Generate App-Level Token: name it `coda-socket`, scope `connections:write`
3. Copy the `xapp-...` token

## Step 3: Create Slash Command

1. Left menu: **Slash Commands** > **Create New Command**
2. Command: `/coda`, Description: `CODA - AI Development Agent`, Usage Hint: `[bind|init|plan|run|list|status|clean] [args]`

## Step 4: Subscribe to Events

1. Left menu: **Event Subscriptions** > toggle **Enable Events**
2. Under **Subscribe to bot events** add: `message.channels`, `message.groups`, `message.im` (optional)

## Step 5: Configure Bot Permissions

Under **OAuth & Permissions** > **Bot Token Scopes**, ensure these scopes are added:

| Scope | Purpose |
|-------|---------|
| `commands` | Receive slash commands |
| `chat:write` | Send and update messages |
| `reactions:write` | Thinking indicator |
| `channels:history` | Read public channel messages (plan threads) |
| `groups:history` | Read private channel messages (plan threads) |
| `im:history` | Read DM messages (optional) |

## Step 6: Install to Workspace

1. Left menu: **Install App** > **Install to Workspace** > Authorize
2. Copy the **Bot User OAuth Token** (`xoxb-...`)

## Step 7: Configure coda-server

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

## Step 8: Start and Verify

```bash
cargo run -p coda-server
# INFO coda_server: Configuration loaded successfully
# INFO coda_server: Starting Socket Mode connection...
# INFO coda_server::socket: WebSocket connected to Slack Socket Mode
```

Test in Slack:
```
/coda help
/coda bind /path/to/your/repo
/coda list
```

## Step 9: Invite Bot to Channel

The bot must be in the channel to receive messages. Type `/invite @CODA` in the target channel.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `/coda` — no response | Check server is running; logs should show `WebSocket connected` |
| `not_in_channel` error | `/invite @CODA` in the channel |
| Bot ignores plan thread replies | Check Event Subscriptions has `message.channels` enabled |
| `invalid_auth` error | Confirm `bot_token` starts with `xoxb-`, `app_token` starts with `xapp-` |
| Frequent disconnects | Check network; server logs should show automatic reconnection |
