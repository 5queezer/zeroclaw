# Hrafn -- Product Requirements Document

**Status:** Draft v0.2
**Author:** Christian Pojoni (@5queezer)
**Date:** 2026-03-29

---

## One-liner

Lightweight autonomous AI agent runtime. Hrafn thinks, MuninnDB remembers.

## Origin

Hrafn is a fork of ZeroClaw (Apache-2.0). We thank the ZeroClaw contributors for the foundation.

---

## Problem

ZeroClaw has strong technical fundamentals (Rust, trait-based, small binary) but suffers from:

1. **Monolithic binary.** All features ship in one binary. Channels, tools, and providers the user doesn't need still compile in. Bloat grows with every PR.
2. **No plugin architecture.** Runtime extensibility requires forking and recompiling.
3. **Closed governance.** Community PRs get silently closed without review or explanation. No public roadmap. No RFC process. Contributors burn out and leave.
4. **Feature-of-the-week development.** Desktop apps, 30+ language localizations, and niche channel integrations (DingTalk, Lark, QQ) ship before core interoperability (A2A) is solved.

The OpenClaw ecosystem has the plugin breadth but runs on Node.js (~400MB overhead). No project currently combines Rust's efficiency with OpenClaw's extensibility and healthy community governance.

---

## Vision

A modular, community-driven AI agent runtime where:

- The core binary stays small (target: <5MB default build)
- Features are opt-in via compile-time flags or runtime MCP plugins
- Every contribution gets a response within 48 hours
- Quality is enforced through trust, not gatekeeping

---

## Non-goals

- Replacing OpenClaw (different philosophy, different audience)
- Building a commercial product or SaaS
- Supporting every messaging platform from day one
- WASM plugin runtime (Stufe 4 -- only if community demands it)

---

## Architecture

### Core principles

1. **Minimal core.** The default `cargo build` produces a lean binary with only essential functionality.
2. **Compile-time modularity.** Channels, tools, providers, and memory backends are Cargo features.
3. **Runtime extensibility via MCP.** Any MCP server is a plugin. No custom protocol.
4. **Trait boundaries = plugin API.** Internal traits are designed as if they were public crate interfaces from day one.

### Current structure (Phase 1: features in monorepo)

All modules live under `src/` with `#[cfg(feature)]` gating:

```
src/agent/        # Agent loop, orchestration
src/config/       # TOML schema, workspace management
src/channels/     # Channel implementations (gated per-channel)
src/tools/        # Tool implementations (gated per-tool)
src/providers/    # LLM provider integrations
src/memory/       # Memory backends (gated per-backend)
```

### Planned structure (Phase 2: Cargo workspace, >20 modules)

```
hrafn-core          # Agent loop, config, trait definitions
hrafn-channel-*     # Channel implementations
hrafn-tool-*        # Tool implementations
hrafn-provider-*    # LLM provider integrations
hrafn-memory-*      # Memory backends
```

### Memory architecture

The `Memory` trait is the abstraction boundary. The agent loop speaks only to the trait, never to a specific backend. MuninnDB is the recommended implementation, available as an opt-in Cargo feature (`memory-muninndb`). The default backend is SQLite (inherited from ZeroClaw).

MuninnDB is designed as a standalone crate -- it can be used independently of Hrafn. Hrafn is a consumer, not the owner. This keeps both projects independently useful and avoids tight coupling.

```
Agent Loop → Memory Trait → SQLite (default)
                          → MuninnDB (opt-in, Ebbinghaus + Hebbian + Dream Engine)
                          → Custom (implement the trait)
```

### Scaling path

| Phase | Structure | Trigger |
|-------|-----------|---------|
| 1 | Cargo features in monorepo | Now |
| 2 | Cargo workspace (separate crates) | >20 modules |
| 3 | `hrafn-core` on crates.io, plugins in own repos | Active community |
| 4 | WASM plugin runtime (optional) | Non-Rust contributors |

### OC Bridge (transitional)

A lightweight Node.js process that loads an OpenClaw plugin and exposes its tools as an MCP server. Purpose: validate demand before investing in a native Rust port.

**Plugin lifecycle funnel:**

```
OC Plugin
  → MCP Adapter (Node.js wrapper, ~100 LOC)
    → Community testing via Bridge
      → Validation (usage data, feedback)
        → Port Queue (prioritized by demand)
          → Native Rust implementation
            → Review & merge
```

This decouples validation from implementation. No plugin gets ported until the community has validated it through actual usage.

---

## Differentiators

### vs. ZeroClaw

| | ZeroClaw | Hrafn |
|---|---------|-------|
| Binary | Monolithic, all features | Modular, opt-in features |
| Plugins | None (recompile) | MCP + OC Bridge |
| Governance | Silent PR closes | 48h response guarantee |
| Roadmap | None public | Public, community-driven |
| Memory | SQLite only | MuninnDB (Ebbinghaus + Hebbian) |
| Interop | No A2A | Native A2A protocol support |

### vs. OpenClaw

| | OpenClaw | Hrafn |
|---|---------|-------|
| Runtime | Node.js (~400MB) | Rust (<5MB) |
| Target HW | Server, desktop | Edge, Pi, embedded |
| Plugins | npm/TS in-process | MCP (any language) |
| Plugin compat | Native | Via OC Bridge (transitional) |

---

## Community & Governance

### Principles

1. **Every PR gets a response within 48 hours.** Accept, request changes, or explain why not.
2. **No silent closes.** If a PR is rejected, there is a written explanation.
3. **RFCs before big features.** Community input before implementation, not after.
4. **Public roadmap.** GitHub Projects board, updated weekly.
5. **Weekly community calls.** Open Zoom/Meet for contributors and users.

### Contribution ladder

| Level | Activity | Trust |
|-------|----------|-------|
| User | Use Hrafn, report issues | -- |
| Tester | Test OC plugins via Bridge, give feedback | Low |
| Adapter | Write MCP adapters for OC plugins | Medium |
| Porter | Take a plugin from the port queue, implement in Rust | High |
| Reviewer | Review native ports, enforce quality standards | Core |
| Maintainer | Trait design, core features, release management | Core |

### Quality standards

- All PRs must pass `cargo fmt`, `cargo clippy -D warnings`, and existing tests
- New features require tests (unit + integration)
- Security-relevant code requires review from a second maintainer
- No AI-generated code without human review and understanding

---

## Bundled features (Phase 1)

### Default (always compiled)

- Agent loop (process_message pipeline)
- Config system (TOML)
- CLI (status, doctor, onboard)
- MCP client (runtime plugin loading)

### Opt-in Cargo features

Current features in `Cargo.toml`:

- `channel-nostr`
- `channel-matrix`
- `channel-lark` (aliased as `channel-feishu`)
- `whatsapp-web` (native WhatsApp Web client)
- `hardware` (USB enumeration + serial)
- `peripheral-rpi` (Raspberry Pi GPIO via rppal)
- `browser-native` (Fantoccini/WebDriver)
- `observability-otel` (OpenTelemetry traces + metrics)
- `sandbox-landlock` / `sandbox-bubblewrap`
- `probe` (probe-rs for STM32/Nucleo)
- `rag-pdf` (PDF ingestion for datasheet RAG)
- `voice-wake` (microphone wake-word detection)
- `plugins-wasm` (extism-based WASM plugin runtime)
- `skill-creation` (default)
- `observability-prometheus` (default)

Planned features (tracked in #90):

- `channel-telegram`, `channel-discord`, `channel-whatsapp`, `channel-signal`
- `tool-shell`, `tool-a2a`
- `gateway` (decoupled from `desktop`)
- `memory-muninndb`

---

## Key integrations

### MuninnDB

Standalone cognitive memory crate. Ebbinghaus-curve decay for forgetting, Hebbian learning for association strengthening. Dream Engine consolidation (LLM-powered, runs via Ollama to keep data local). Consumed by Hrafn via the `Memory` trait behind the `memory-muninndb` feature gate.

### A2A Protocol

Native Agent-to-Agent communication. Outbound client tool + inbound JSON-RPC server. Auto-generated Agent Card. Cherry-picked from PR #4166 (40 tests, SSRF hardening, E2E validated on Pi Zero 2 W).

---

## Security

### Initial audit (2026-03-29)

A security audit of the inherited ZeroClaw codebase identified 7 issue groups, tracked as GitHub Issues with `PR: Security` label:

| Priority | Issue | Scope |
|----------|-------|-------|
| P0 | Panic on user input (iMessage bounds, provider double-call, WAV overflow) | channels, providers |
| P0 | Silent mutex poisoning (auth, whatsapp voice) | security, channels |
| P0 | Dependency CVE (rustls-webpki) | dependencies |
| P1 | Integer overflow / lossy casts (voice_wake, whatsapp_web, GLM) | channels, providers |
| P1 | Gateway error sanitization + open redirect | gateway |
| P1 | Gateway CORS + forwarded header validation | gateway |
| P2 | Security module hardening (OTP TOCTOU, icacls, audit log) | security |

### Policy

- No `unwrap()` in production code. Use `?`, `anyhow`, or `thiserror`.
- Security-relevant PRs require a second reviewer (`PR: Security` label).
- No self-merging on security code.
- `cargo audit` runs in CI; known CVEs block merge.

---

## Milestones

### M0: Fork & Rename (Week 1)

- [x] Rename repo to `hrafn`
- [x] Logo (geometric raven, line-art)
- [x] Update README with own identity, origin section
- [x] CONTRIBUTING.md with governance promises
- [x] Remove non-English README translations
- [x] Update repo About section (description, remove zeroclawlabs.ai link)
- [x] Security audit of inherited codebase
- [x] Initial feature-gate structure (`#[cfg(feature = "...")]`)
- [x] Cherry-pick A2A commits (PR #4166)
- [x] Cherry-pick config hot-reload (PR #3571)
- [x] Fix P0 security issues (panics, mutex poisoning, rustls-webpki CVE)

### M1: Modular Core (Week 2-3)

- [ ] Feature-gate all channels, tools, providers
- [ ] Verify minimal build compiles and runs
- [ ] CI matrix for feature combinations
- [ ] `hrafn doctor` validates config vs. enabled features
- [ ] Fix P1 security issues (integer overflows, gateway hardening)
- [x] `cargo audit` in CI pipeline

### M2: MuninnDB Integration (Week 3-4)

- [ ] `memory-muninndb` feature gate
- [ ] Dream Engine consolidation (Ollama-backed)
- [ ] Migration from default SQLite memory

### M3: OC Bridge (Week 4-5)

- [ ] Generic OC-Plugin-to-MCP adapter (Node.js)
- [ ] First plugin candidate (voice-call or similar)
- [ ] Documentation: "How to test an OC plugin with Hrafn"

### M4: Community Launch (Week 5-6)

- [ ] Public roadmap (GitHub Projects)
- [ ] First community call
- [ ] Blog post: "Why Hrafn exists" (vasudev.xyz)
- [ ] Cross-reference solved ZeroClaw issues
- [ ] Fix P2 security issues (OTP TOCTOU, audit log)

---

## Risks

| Risk | Mitigation |
|------|------------|
| Solo maintainer burnout | Contribution ladder + async governance. Don't merge everything yourself. |
| ZeroClaw trademark claim | Own name, own identity, Apache-2.0 compliant attribution |
| OC Bridge becomes permanent | Port queue with demand metrics. Bridge plugins that see >2 weeks active use get queued. |
| Scope creep | This PRD. RFCs for anything not on the roadmap. |
| Splitting focus with job search | Hrafn is a portfolio piece, not a second job. Timebox to evenings/weekends. |
| Inherited security debt | Audit completed, 7 issues tracked, P0 fixes in M0. |

---

## Resolved decisions

1. **Name:** Hrafn (Old Norse for 'raven'). Available on Crates.io and GitHub.
2. **Logo:** Geometric line-art raven in blue.
3. **Domain:** GitHub-only until M4. Evaluate hrafn.dev if traction warrants it.
4. **License:** Apache-2.0 (inherited, no change).
5. **Localizations:** English-only README. Community translations welcome as `docs/README.<lang>.md`.
6. **Memory:** MuninnDB as opt-in feature behind `Memory` trait boundary. SQLite as default.

## Open questions

1. First community call platform: Zoom, Google Meet, Discord?
2. Dual-license (Apache-2.0 + MIT) for broader compatibility?

