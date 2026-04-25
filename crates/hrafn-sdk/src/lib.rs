#![cfg_attr(not(feature = "std"), no_std)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod prelude;
pub mod protocol;
pub mod tool;

pub use protocol::{
    Capability, ExtensionKind, HandshakeRequest, HandshakeResponse, Permission, PluginManifest,
    SDK_PROTOCOL_VERSION,
};
pub use tool::{ToolResult, ToolSpec};
