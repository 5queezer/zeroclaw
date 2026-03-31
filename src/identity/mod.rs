//! Identity system: AIEOS/OpenClaw agent identity + persistent caller registry.

mod aieos;
pub mod registry;

// Re-export AIEOS public API so existing `crate::identity::` paths keep working.
pub use aieos::{aieos_to_system_prompt, is_aieos_configured, load_aieos_identity};
