//! Hermes-parity features: autonomous skill creation, self-improvement, and pipeline execution.
//!
//! This module implements three capabilities inspired by the Hermes Agent (NousResearch):
//! 1. **Autonomous skill creation** — after each agent turn, classify whether the workflow
//!    should be persisted as a reusable skill document.
//! 2. **Skill self-improvement** — after using an existing skill, diff execution against the
//!    skill doc and optionally improve it with atomic writes and audit trails.
//! 3. **Programmatic pipeline tool calling** — execute a JSON array of tool steps sequentially
//!    or in parallel with template interpolation and security controls.

pub mod autonomous;
pub mod improve;
pub mod pipeline;
