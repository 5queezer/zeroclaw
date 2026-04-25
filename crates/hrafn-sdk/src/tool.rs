use crate::prelude::String;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Result of a tool execution.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Description of a tool for LLM/function-call registration.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    #[cfg(feature = "serde")]
    pub parameters: serde_json::Value,
}
