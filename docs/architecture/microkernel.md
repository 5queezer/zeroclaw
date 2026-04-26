# Hrafn Microkernel Migration

Goal: reduce clean/incremental build time and binary size by making the default architecture a small kernel plus optional integrations.

## Current seed

This repository now has a minimal SDK boundary and kernel seed:

- `crates/hrafn-sdk`: dependency-light shared plugin/kernel protocol types.
- `crates/hrafn-kernel`: tiny standalone kernel seed binary gated away from the full `hrafn` package.
- `full`: explicit feature profile preserving the historical all-in-one default set.
- `dev/measure-microkernel.sh`: build/dependency-size measurement helper.

The first invariant is that the SDK must stay small. It must not depend on integration crates such as `reqwest`, `axum`, `ratatui`, `rusqlite`, `matrix-sdk`, or `wa-rs`.

## Intended shape

The kernel owns:

- config loading
- session routing
- security policy
- capability grants
- audit records
- event bus
- plugin lifecycle

Plugins own:

- providers
- channels
- tools
- memory backends
- gateways/frontends
- hardware integrations

## Near-term migration order

1. Keep `default = ["full"]` until compatibility is intentionally changed.
2. Use `crates/hrafn-kernel` to prove that a tiny non-desktop binary can compile without the monolith.
3. Move stable trait/request/response types from `src/*/traits.rs` into `crates/hrafn-sdk`.
4. Extract one provider crate, preferably OpenRouter, and register it as an in-process plugin.
5. Extract one high-risk tool crate, preferably shell, and attach explicit capability metadata.
6. Extract `gateway` and `tui` into separate crates/binaries so server and CLI builds do not pull those dependencies unless requested.
7. Add JSON-RPC-over-stdio plugin support for heavy or independently released integrations.

## Measurement

Run:

```bash
./dev/measure-microkernel.sh kernel
./dev/measure-microkernel.sh full
```

Outputs:

- `target/hrafn-kernel-features.txt`
- `target/hrafn-full-features.txt`
- optional `target/hrafn-*-bloat.txt` if `cargo-bloat` is installed

## Build commands

Minimal kernel seed:

```bash
cargo build -p hrafn-kernel
```

Historical full distribution:

```bash
cargo build --features full --bin hrafn
```
