//! Deprecated stdio-RPC module.
//!
//! This module previously provided a JSON-RPC 2.0 session server over stdio
//! for IDE integration. It has been superseded by the ACP (Agent Communication
//! Protocol) HTTP API available on the gateway.
//!
//! The `hrafn stdio-rpc` command now prints a migration guide and exits.

/// Print deprecation message and migration guide.
pub fn print_deprecation_notice() {
    eprintln!(
        "\n\
         WARNING: 'stdio-rpc' is deprecated and will be removed in a future release.\n\
         \n\
         Use the ACP HTTP API instead:\n\
         \n\
         1. Start the gateway:  hrafn gateway\n\
         2. Connect to:         http://localhost:3000\n\
         \n\
         Migration guide:\n\
         \n\
           stdio-rpc              ACP equivalent\n\
           ─────────              ──────────────\n\
           initialize         →   GET  /ping + GET /agents\n\
           session/new        →   POST /runs (creates session)\n\
           session/prompt     →   POST /runs (with session_id)\n\
           session/stop       →   POST /runs/{{id}}/cancel\n\
           session/event      →   POST /runs (mode: stream, SSE)\n\
         \n\
         Documentation: docs/superpowers/specs/2026-04-10-acp-implementation-design.md\n"
    );
}
