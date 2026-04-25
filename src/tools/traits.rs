use async_trait::async_trait;
pub use hrafn_sdk::{ToolResult, ToolSpec};
use serde::{Deserialize, Serialize};

/// Lightweight stub for tiered context injection — name + short description only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStub {
    pub name: String,
    pub stub_description: String,
}

/// Core tool trait — implement for any capability
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used in LLM function calling)
    fn name(&self) -> &str;

    /// Human-readable description
    fn description(&self) -> &str;

    /// JSON schema for parameters
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with given arguments
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;

    /// Get the full spec for LLM registration
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }

    /// Short description for tiered context injection.
    /// Used in system-prompt stubs where the full description would waste tokens.
    /// Default: first sentence of `description()` (up to the first ". "),
    /// capped at 120 characters. Falls back to the full description truncated at
    /// 120 characters with "…". Whitespace (newlines, tabs, runs of spaces) is
    /// collapsed to single spaces. Truncation is always on a character boundary.
    fn stub_description(&self) -> String {
        let d = self.description();
        // Normalize: collapse all whitespace runs to a single space and trim.
        let normalized: String = d.split_whitespace().collect::<Vec<_>>().join(" ");

        let raw: &str = if let Some(pos) = normalized.find(". ") {
            &normalized[..=pos]
        } else {
            &normalized
        };

        if raw.chars().count() <= 120 {
            raw.to_string()
        } else {
            // char_indices().nth(119) yields the byte offset of the 120th character,
            // which is always a valid UTF-8 boundary.
            let end = raw
                .char_indices()
                .nth(119)
                .map(|(i, _)| i)
                .unwrap_or(raw.len());
            format!("{}…", &raw[..end])
        }
    }

    /// Get a lightweight stub (name + short description) for tiered context injection.
    fn stub(&self) -> ToolStub {
        ToolStub {
            name: self.name().to_string(),
            stub_description: self.stub_description(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy_tool"
        }

        fn description(&self) -> &str {
            "A deterministic test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                error: None,
            })
        }
    }

    #[test]
    fn spec_uses_tool_metadata_and_schema() {
        let tool = DummyTool;
        let spec = tool.spec();

        assert_eq!(spec.name, "dummy_tool");
        assert_eq!(spec.description, "A deterministic test tool");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["properties"]["value"]["type"], "string");
    }

    #[tokio::test]
    async fn execute_returns_expected_output() {
        let tool = DummyTool;
        let result = tool
            .execute(serde_json::json!({ "value": "hello-tool" }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "hello-tool");
        assert!(result.error.is_none());
    }

    #[test]
    fn tool_result_serialization_roundtrip() {
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("boom".into()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();

        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    struct SentenceTool;

    #[async_trait]
    impl Tool for SentenceTool {
        fn name(&self) -> &str {
            "sentence_tool"
        }
        fn description(&self) -> &str {
            "First sentence. Second sentence follows here."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    struct LongTool;

    #[async_trait]
    impl Tool for LongTool {
        fn name(&self) -> &str {
            "long_tool"
        }
        fn description(&self) -> &str {
            // 130-char description with no ". " separator
            "This description is deliberately long and has no sentence break so truncation at 120 chars with an ellipsis should happen here."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    struct ShortTool;

    #[async_trait]
    impl Tool for ShortTool {
        fn name(&self) -> &str {
            "short_tool"
        }
        fn description(&self) -> &str {
            "Short desc"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    #[test]
    fn stub_description_truncates_at_first_sentence() {
        let tool = SentenceTool;
        assert_eq!(tool.stub_description(), "First sentence.");
    }

    #[test]
    fn stub_description_truncates_long_description_with_ellipsis() {
        let tool = LongTool;
        let desc = tool.description();
        assert!(
            desc.len() > 120,
            "precondition: description must be > 120 chars"
        );
        let stub = tool.stub_description();
        assert!(stub.ends_with('…'));
        // The stub should be the first 119 chars + the ellipsis character (3 bytes in UTF-8)
        assert_eq!(stub, format!("{}…", &desc[..119]));
    }

    #[test]
    fn stub_description_passes_short_description_unchanged() {
        let tool = ShortTool;
        assert_eq!(tool.stub_description(), "Short desc");
    }

    #[test]
    fn stub_returns_name_and_stub_description() {
        let tool = SentenceTool;
        let stub = tool.stub();
        assert_eq!(stub.name, "sentence_tool");
        assert_eq!(stub.stub_description, "First sentence.");
    }

    #[test]
    fn stub_description_caps_long_first_sentence() {
        // A description whose first sentence is > 120 bytes should still be capped.
        struct LongSentenceTool;
        #[async_trait]
        impl Tool for LongSentenceTool {
            fn name(&self) -> &str {
                "t"
            }
            fn description(&self) -> &str {
                // First sentence is 130+ chars, terminated by ". "
                "This first sentence is deliberately very long so that it exceeds the 120-character limit that we enforce on stub descriptions. Second sentence."
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }
        let stub = LongSentenceTool.stub_description();
        assert!(stub.ends_with('…'), "long first sentence must be truncated");
        assert!(stub.len() <= 120 + '…'.len_utf8(), "must not exceed budget");
    }

    #[test]
    fn stub_description_non_ascii_truncation_does_not_panic() {
        // 122 euro signs = 122 characters (> 120 limit), 3 bytes each in UTF-8.
        // A byte-based slice at position 119 would land mid-character → panic.
        // char_indices().nth(119) must yield a valid character boundary.
        struct UnicodeTool;
        #[async_trait]
        impl Tool for UnicodeTool {
            fn name(&self) -> &str {
                "t"
            }
            fn description(&self) -> &str {
                // 122 × "€" = 122 chars, 366 bytes
                "€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }
        let result = UnicodeTool.stub_description();
        // Must truncate (122 chars > 120), end with ellipsis, be valid UTF-8.
        // char_indices().nth(119) slices before the 120th char → 119 chars
        // remain before the ellipsis.
        assert!(result.ends_with('…'));
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        let before_ellipsis = result.trim_end_matches('…');
        assert_eq!(before_ellipsis.chars().count(), 119);
    }
}
