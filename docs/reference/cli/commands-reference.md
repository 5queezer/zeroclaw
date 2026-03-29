# Hrafn Commands Reference

This reference is derived from the current CLI surface (`hrafn --help`).

Last verified: **March 26, 2026**.

## Top-Level Commands

| Command | Purpose |
|---|---|
| `onboard` | Initialize workspace/config quickly or interactively |
| `agent` | Run interactive chat or single-message mode |
| `gateway` | Start webhook and WhatsApp HTTP gateway |
| `acp` | Start ACP (Agent Control Protocol) server over stdio |
| `daemon` | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service` | Manage user-level OS service lifecycle |
| `doctor` | Run diagnostics and freshness checks |
| `status` | Print current configuration and system summary |
| `estop` | Engage/resume emergency stop levels and inspect estop state |
| `cron` | Manage scheduled tasks |
| `models` | Refresh provider model catalogs |
| `providers` | List provider IDs, aliases, and active provider |
| `channel` | Manage channels and channel health checks |
| `integrations` | Inspect integration details |
| `skills` | List/install/remove skills |
| `migrate` | Import from external runtimes (currently OpenClaw) |
| `config` | Export machine-readable config schema |
| `completions` | Generate shell completion scripts to stdout |
| `hardware` | Discover and introspect USB hardware |
| `peripheral` | Configure and flash peripherals |

## Command Groups

### `onboard`

- `hrafn onboard`
- `hrafn onboard --channels-only`
- `hrafn onboard --force`
- `hrafn onboard --reinit`
- `hrafn onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `hrafn onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `hrafn onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` safety behavior:

- If `config.toml` already exists, onboarding offers two modes:
  - Full onboarding (overwrite `config.toml`)
  - Provider-only update (update provider/model/API key while preserving existing channels, tunnel, memory, hooks, and other settings)
- In non-interactive environments, existing `config.toml` causes a safe refusal unless `--force` is passed.
- Use `hrafn onboard --channels-only` when you only need to rotate channel tokens/allowlists.
- Use `hrafn onboard --reinit` to start fresh. This backs up your existing config directory with a timestamp suffix and creates a new configuration from scratch.

### `agent`

- `hrafn agent`
- `hrafn agent -m "Hello"`
- `hrafn agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `hrafn agent --peripheral <board:path>`

Tip:

- In interactive chat, you can ask for route changes in natural language (for example “conversation uses kimi, coding uses gpt-5.3-codex”); the assistant can persist this via tool `model_routing_config`.

### `acp`

- `hrafn acp`
- `hrafn acp --max-sessions <N>`
- `hrafn acp --session-timeout <SECONDS>`

Start the ACP (Agent Control Protocol) server for IDE and tool integration.

- Uses JSON-RPC 2.0 over stdin/stdout
- Supports methods: `initialize`, `session/new`, `session/prompt`, `session/stop`
- Streams agent reasoning, tool calls, and content in real-time as notifications
- Default max sessions: 10
- Default session timeout: 3600 seconds (1 hour)

### `gateway` / `daemon`

- `hrafn gateway [--host <HOST>] [--port <PORT>]`
- `hrafn daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `hrafn estop` (engage `kill-all`)
- `hrafn estop --level network-kill`
- `hrafn estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `hrafn estop --level tool-freeze --tool shell [--tool browser]`
- `hrafn estop status`
- `hrafn estop resume`
- `hrafn estop resume --network`
- `hrafn estop resume --domain "*.chase.com"`
- `hrafn estop resume --tool shell`
- `hrafn estop resume --otp <123456>`

Notes:

- `estop` commands require `[security.estop].enabled = true`.
- When `[security.estop].require_otp_to_resume = true`, `resume` requires OTP validation.
- OTP prompt appears automatically if `--otp` is omitted.

### `service`

- `hrafn service install`
- `hrafn service start`
- `hrafn service stop`
- `hrafn service restart`
- `hrafn service status`
- `hrafn service uninstall`

### `cron`

- `hrafn cron list`
- `hrafn cron add <expr> [--tz <IANA_TZ>] <command>`
- `hrafn cron add-at <rfc3339_timestamp> <command>`
- `hrafn cron add-every <every_ms> <command>`
- `hrafn cron once <delay> <command>`
- `hrafn cron remove <id>`
- `hrafn cron pause <id>`
- `hrafn cron resume <id>`

Notes:

- Mutating schedule/cron actions require `cron.enabled = true`.
- Shell command payloads for schedule creation (`create` / `add` / `once`) are validated by security command policy before job persistence.

### `models`

- `hrafn models refresh`
- `hrafn models refresh --provider <ID>`
- `hrafn models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, and `nvidia`.

### `doctor`

- `hrafn doctor`
- `hrafn doctor models [--provider <ID>] [--use-cache]`
- `hrafn doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `hrafn doctor traces --id <TRACE_ID>`

`doctor traces` reads runtime tool/model diagnostics from `observability.runtime_trace_path`.

### `channel`

- `hrafn channel list`
- `hrafn channel start`
- `hrafn channel doctor`
- `hrafn channel bind-telegram <IDENTITY>`
- `hrafn channel add <type> <json>`
- `hrafn channel remove <name>`

Runtime in-chat commands (Telegram/Discord while channel server is running):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`
- `/new`

Channel runtime also watches `config.toml` and hot-applies updates to:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (for the default provider)
- `reliability.*` provider retry settings

`add/remove` currently route you back to managed setup/manual config paths (not full declarative mutators yet).

### `integrations`

- `hrafn integrations info <name>`

### `skills`

- `hrafn skills list`
- `hrafn skills audit <source_or_name>`
- `hrafn skills install <source>`
- `hrafn skills remove <name>`

`<source>` accepts git remotes (`https://...`, `http://...`, `ssh://...`, and `git@host:owner/repo.git`) or a local filesystem path.

`skills install` always runs a built-in static security audit before the skill is accepted. The audit blocks:
- symlinks inside the skill package
- script-like files (`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- high-risk command snippets (for example pipe-to-shell payloads)
- markdown links that escape the skill root, point to remote markdown, or target script files

Use `skills audit` to manually validate a candidate skill directory (or an installed skill by name) before sharing it.

Skill manifests (`SKILL.toml`) support `prompts` and `[[tools]]`; both are injected into the agent system prompt at runtime, so the model can follow skill instructions without manually reading skill files.

### `migrate`

- `hrafn migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `hrafn config schema`

`config schema` prints a JSON Schema (draft 2020-12) for the full `config.toml` contract to stdout.

### `completions`

- `hrafn completions bash`
- `hrafn completions fish`
- `hrafn completions zsh`
- `hrafn completions powershell`
- `hrafn completions elvish`

`completions` is stdout-only by design so scripts can be sourced directly without log/warning contamination.

### `hardware`

- `hrafn hardware discover`
- `hrafn hardware introspect <path>`
- `hrafn hardware info [--chip <chip_name>]`

### `peripheral`

- `hrafn peripheral list`
- `hrafn peripheral add <board> <path>`
- `hrafn peripheral flash [--port <serial_port>]`
- `hrafn peripheral setup-uno-q [--host <ip_or_host>]`
- `hrafn peripheral flash-nucleo`

## Validation Tip

To verify docs against your current binary quickly:

```bash
hrafn --help
hrafn <command> --help
```
