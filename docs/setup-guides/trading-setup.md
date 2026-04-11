# Trading Signal Pipeline Setup

Hrafn can analyze trading signals from external sources by combining
Telegram channel monitoring with an MCP-compatible chart server for technical
analysis.

## Prerequisites

- An MCP-compatible chart/trading server running in trade mode
- A subscription to a signal provider Telegram channel
- Telegram Bot API credentials (create a bot via
  [@BotFather](https://t.me/BotFather))
- Telegram API credentials (`api_id`, `api_hash`) from
  [my.telegram.org](https://my.telegram.org) if you plan to monitor channels
  with a user client

## Hrafn Configuration

### Chart MCP Server

Add your chart MCP server to `hrafn.toml` so the agent can call chart
generation and indicator tools:

```toml
[mcp]
enabled = true

[[mcp.servers]]
name = "charts"
transport = "http"
url = "https://your-chart-server.example.com"
```

For local development, use stdio transport instead:

```toml
[[mcp.servers]]
name = "charts"
transport = "stdio"
command = "./your-chart-server"
args = ["--mcp"]
```

### Telegram Bot Channel

Configure the Telegram bot that receives forwarded signals or user queries:

```toml
[channels_config.telegram]
bot_token = "123456:ABC-DEF..."
allowed_users = ["your_telegram_username"]
```

### Telegram User Channel (optional)

If you want passive monitoring of third-party channels, add:

```toml
[channels_config.telegram_user]
api_id = 123456
api_hash = "your_api_hash"
phone = "+1 555 123 4567"
session_file = "~/.hrafn/telegram_user.session"
reply_via_bot = "@your_hrafn_bot"

[[channels_config.telegram_user.watch]]
channel = "signal_provider"
handler = "trading_signal"
```

### Trading Signal Analysis Prompt

Hrafn loads personality/instruction files from the workspace directory. Create a
`SOUL.md` (or edit the existing one) in your Hrafn workspace
(`~/.hrafn/SOUL.md` by default) to include trading analysis instructions:

```markdown
# SOUL.md

You are a trading analyst assistant. When you receive a signal:

## Analysis Rules
- Use `charts__get_indicators` to fetch your own technical analysis
- Cross-check signals against your own indicator readings before acting
- Always generate your own chart and send it to the user
- Decide: place_order, set_alert, or skip — always with justification

## Response Format
1. Summarise the incoming signal (pair, direction, timeframe)
2. Run your own indicator checks
3. Generate a chart via `charts__generate_chart`
4. State your decision with reasoning
```

Alternatively, if you use delegate agents, you can define a dedicated trading
analyst agent with its own system prompt:

```toml
[agents.trading_analyst]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
agentic = true
allowed_tools = [
    "charts__generate_chart",
    "charts__list_indicators",
    "charts__get_indicators",
]
system_prompt = """
You are a trading analyst. You receive signals from a signal provider.
Use charts__get_indicators to run your own analysis.
Rules:
- Cross-check every signal with your own indicator readings
- Always generate your own chart and send it to the user
- Decision: place_order, set_alert, or skip with justification
"""
```

## Testing

1. Start your chart server in trade mode
2. Start Hrafn: `hrafn`
3. Send a test trading signal image to your Hrafn Telegram bot
4. Verify the agent calls MCP tools (`charts__generate_chart`,
   `charts__get_indicators`) and responds with analysis

## Troubleshooting

- **MCP tools not appearing**: Ensure `[mcp] enabled = true` and the chart
  server is reachable at the configured URL.
- **Agent not using tools**: Check that `deferred_loading` is working — the
  agent must call `tool_search` first if deferred loading is enabled (the
  default). Set `deferred_loading = false` to eagerly load all tool schemas,
  or add frequently-needed tools to `eager_tools` on the server config so
  they're available without a `tool_search` roundtrip.
- **Timeout errors**: Increase `tool_timeout_secs` on the MCP server config:
  ```toml
  [[mcp.servers]]
  name = "charts"
  transport = "http"
  url = "https://your-chart-server.example.com"
  tool_timeout_secs = 60
  ```
