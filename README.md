<p align="center">
  <img src="assets/hrafn-logo.png" alt="Hrafn" width="200" />
</p>

<h1 align="center">hrafn</h1>

<p align="center">
  <em>Lightweight, modular AI agent runtime. Hrafn thinks. <a href="https://github.com/5queezer/muninndb">MuninnDB</a> remembers.</em>
</p>

<p align="center">
  <a href="https://github.com/5queezer/hrafn/actions/workflows/ci-run.yml"><img src="https://github.com/5queezer/hrafn/actions/workflows/ci-run.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://github.com/5queezer/hrafn/releases"><img src="https://img.shields.io/github/v/release/5queezer/hrafn?include_prereleases" alt="Release"></a>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> · <a href="#architecture">Architecture</a> · <a href="CONTRIBUTING.md">Contributing</a> · <a href="https://github.com/5queezer/hrafn/discussions">Discussions</a>
</p>

---

## What is Hrafn?

Hrafn is an autonomous AI agent runtime written in Rust. It connects to the messaging platforms you already use (Telegram, Discord, WhatsApp, Signal, Matrix, and more), runs on hardware as small as a Raspberry Pi, and keeps your data local.

Unlike monolithic agent frameworks, Hrafn is **modular by design**. You compile only what you need. Runtime extensibility comes through MCP -- every MCP server is a plugin.

## Why Hrafn?

**Modular, not monolithic.** Channels, tools, providers, and memory backends are Cargo features. The default build is small. You opt in to what you need.

**MCP as the plugin protocol.** No custom plugin API. Any MCP server works as a Hrafn plugin, in any language. The OpenClaw Bridge lets you test OC plugins via MCP before porting them to native Rust.

**MuninnDB.** Cognitive memory with Ebbinghaus-curve decay and Hebbian association learning. The Dream Engine consolidates memories via local LLM inference (Ollama), so your data never leaves your machine.

**A2A protocol.** Native Agent-to-Agent communication. Discover, delegate, and receive tasks from other agents over HTTP using the open A2A standard.

**Community-first governance.** Every PR gets a response within 48 hours. No silent closes. Public roadmap. Weekly community calls. See [CONTRIBUTING.md](CONTRIBUTING.md) for our promises.

## Quickstart

```bash
# Install from source
git clone https://github.com/5queezer/hrafn.git
cd hrafn
cargo build --release --locked
cargo install --path . --force --locked

# Guided setup
hrafn onboard

# Or quick start
hrafn onboard --api-key "sk-..." --provider openrouter

# Start the gateway (web dashboard + webhook server)
hrafn gateway

# Chat directly
hrafn agent -m "Hello, Hrafn!"

# Interactive mode
hrafn agent

# Full autonomous runtime
hrafn daemon

# Diagnostics
hrafn status
hrafn doctor
```

### Minimal feature build

```bash
# Full default build (all channels, tools, gateway, metrics)
cargo build --release --locked

# Selective channels: only Telegram + shell, no Matrix/Nostr/WhatsApp
cargo build --no-default-features --features "desktop,channel-telegram,tool-shell"

# Stripped-down ESP32 build (no CLI, no gateway, no optional channels)
cargo build --bin hrafn-esp32 --no-default-features --features target-esp32
```

## Architecture

Hrafn's architecture is **trait-based**. Every subsystem is a Rust trait. Swap implementations through configuration, not code changes.

```
src/
├── agent/         # Orchestration loop
├── config/        # TOML configuration
├── providers/     # LLM backends          → Provider trait
├── channels/      # Messaging platforms    → Channel trait
├── tools/         # Agent capabilities     → Tool trait
├── memory/        # Persistence            → Memory trait
├── gateway/       # HTTP/WS control plane
├── security/      # Policy, secrets, audit
├── hardware/      # Device discovery, I2C/SPI/GPIO
├── peripherals/   # Peripheral management  → Peripheral trait
├── runtime/       # Runtime adapters       → RuntimeAdapter trait
├── observability/ # Metrics, tracing
├── plugins/       # WASM plugin runtime
├── daemon/        # Background service
├── skills/        # Skill management
├── rag/           # Retrieval-augmented generation
├── hooks/         # Lifecycle hooks
├── cron/          # Scheduled tasks
├── identity/      # Identity management
├── tunnel/        # Tunnel/relay support
└── ...            # approval, auth, commands, cost, doctor, hands, health,
                   # heartbeat, integrations, nodes, onboard, routines,
                   # service, skillforge, sop, trust, verifiable_intent
```

### Compile-time modularity

Every channel, tool, and subsystem is gated behind a Cargo feature. The `desktop` feature bundles everything needed for a full CLI build; opt-in features add extra backends.

| Feature | Default | Description |
|---|---|---|
| `desktop` | Yes | Full CLI + interactive features (depends on `gateway`) |
| `gateway` | Yes | HTTP/WebSocket gateway server (axum/hyper/tower) |
| **Channels** | | |
| `channel-telegram` | Yes | Telegram bot channel |
| `channel-discord` | Yes | Discord bot channel |
| `channel-whatsapp` | Yes | WhatsApp Cloud API channel |
| `channel-signal` | Yes | Signal messenger channel |
| `channel-matrix` | No | Matrix/Element E2EE channel |
| `channel-nostr` | No | Nostr protocol channel |
| `channel-lark` | No | Lark/Feishu channel |
| `channel-feishu` | No | Alias for `channel-lark` |
| `whatsapp-web` | No | Native WhatsApp Web client (wa-rs) |
| **Tools** | | |
| `tool-shell` | Yes | Shell command execution tool |
| `tool-a2a` | Yes | Agent-to-Agent protocol tool + gateway routes |
| **Memory** | | |
| `memory-muninndb` | Yes | MuninnDB memory backend |
| **Observability** | | |
| `observability-prometheus` | Yes | Prometheus metrics |
| `observability-otel` | No | OpenTelemetry tracing |
| **Hardware** | | |
| `hardware` | No | USB device discovery + serial |
| `peripheral-rpi` | No | Raspberry Pi GPIO |
| `probe` | No | probe-rs debug probe support |
| **Sandbox** | | |
| `sandbox-landlock` | No | Linux Landlock sandboxing |
| `sandbox-bubblewrap` | No | Bubblewrap sandboxing |
| **Other** | | |
| `browser-native` | No | Fantoccini WebDriver backend |
| `voice-wake` | No | Voice wake word detection |
| `plugins-wasm` | No | WASM plugin runtime (extism) |
| `skill-creation` | Yes | Autonomous skill creation |
| `rag-pdf` | No | PDF ingestion for RAG |
| `webauthn` | No | WebAuthn/FIDO2 auth |
| `target-esp32` | No | Stripped-down ESP32-S3 build |

Use `--no-default-features` and opt in to individual features for minimal builds. Configuring a disabled module logs a warning at startup.

### Runtime extensibility

Any MCP server is a plugin. Configure in `config.toml`:

```toml
[mcp]
servers = [
  { name = "my-tool", command = "npx", args = ["-y", "my-mcp-server"] },
]

```

No recompilation needed. MCP plugins can be written in any language.

## OpenClaw Bridge

The OC Bridge lets Hrafn users run OpenClaw plugins via MCP without a native Rust port. It serves as a **validation funnel**: plugins that see sustained community usage get queued for native porting.

```
OC Plugin → MCP Adapter (Node.js) → Hrafn tests it → Community validates
  → Port Queue → Native Rust implementation → Review & merge
```

*OC Bridge is planned for a future release (M3).*

## Key Integrations

### MuninnDB

Cognitive memory backend with Ebbinghaus-curve decay (memories fade naturally) and Hebbian learning (co-activated memories strengthen each other). The Dream Engine runs periodic consolidation via local LLM inference.

```toml
[memory]
backend = "muninndb"

[memory.muninndb]
url = "http://127.0.0.1:8475"   # optional; falls back to MUNINNDB_URL env var
vault = "default"               # optional; falls back to MUNINNDB_VAULT env var
# api_key = "your-api-key"     # optional; falls back to MUNINNDB_API_KEY env var
```

### A2A Protocol

Native Agent-to-Agent communication per the open [A2A standard](https://github.com/a2aproject/A2A).

```toml
[a2a]
enabled = true
bearer_token = "your-secret"
# agent_name = "my-agent"        # defaults to config agent name
# public_url = "https://my-agent.example.com"  # auto-derived from gateway if omitted
# capabilities = ["research", "coding"]
```

Inbound tasks route through the gateway server and the existing agent pipeline (A2A uses the same gateway port; no separate bind). The agent card is auto-generated from your configuration.

## Roadmap

See the [GitHub Projects board](https://github.com/5queezer/hrafn/projects) for current status.

## Contributing

We believe open-source communities deserve transparent governance and respect for contributors' work. Read [CONTRIBUTING.md](CONTRIBUTING.md) for our promises and workflow.

The short version:
- Every PR gets a response within 48 hours.
- No silent closes. Rejections come with explanations.
- Your code stays your code. Maintainers never re-submit contributor work under their own name.

## Community

- [GitHub Discussions](https://github.com/5queezer/hrafn/discussions) -- questions, RFCs, show & tell
- Weekly community calls (schedule in Discussions)

## Origin

Hrafn originated as a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) (Apache-2.0). We thank the ZeroClaw contributors for the foundation.

## License

MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). You retain copyright of your contributions.

**`memory-muninndb` feature gate:** Enabling this feature pulls in [MuninnDB](https://github.com/5queezer/muninndb), which is licensed under BSL 1.1 (not open source) and patent pending (U.S. Provisional Application No. 63/991,402). Commercial use of MuninnDB requires a separate license. See [muninndb.com](https://muninndb.com) for details.
