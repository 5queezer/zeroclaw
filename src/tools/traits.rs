use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Description of a tool for the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

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
    /// capped at 120 chars. Falls back to the full description truncated at
    /// 120 chars with "…". Truncation is always on a UTF-8 character boundary.
    fn stub_description(&self) -> String {
        let d = self.description();
        let raw = if let Some(pos) = d.find(". ") {
            &d[..=pos]
        } else {
            d
        };
        if raw.len() <= 120 {
            raw.to_string()
        } else {
            let mut end = 119;
            while end > 0 && !raw.is_char_boundary(end) {
                end -= 1;
            }
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
        // Description is all multi-byte characters (3 bytes each in UTF-8).
        // Any byte-level slice at position 119 would land mid-character → panic
        // without the char-boundary fix.
        struct UnicodeTool;
        #[async_trait]
        impl Tool for UnicodeTool {
            fn name(&self) -> &str {
                "t"
            }
            fn description(&self) -> &str {
                // "€" is 3 bytes; 45 repetitions = 135 bytes, > 120
                "€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€€"
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
        let stub = UnicodeTool.stub_description();
        // Must not panic, must end with ellipsis, must be valid UTF-8.
        assert!(stub.ends_with('…'));
        assert!(std::str::from_utf8(stub.as_bytes()).is_ok());
    }
}
