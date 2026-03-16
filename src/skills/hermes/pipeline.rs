//! Programmatic pipeline tool calling.
//!
//! Executes a JSON array of tool steps sequentially (or in parallel where no
//! data dependency exists). Implements security controls:
//! - S3.1: Tool allowlist enforcement
//! - S3.2: Template injection prevention (single-pass interpolation)
//! - S3.3: SSRF prevention (block private/loopback IPs)
//! - S3.4: Step cap enforcement

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use thiserror::Error;

/// Maximum default steps per pipeline.
pub const DEFAULT_MAX_STEPS: usize = 20;

/// Default maximum token budget for all pipeline args combined.
pub const DEFAULT_MAX_TOKEN_BUDGET: usize = 100_000;

/// Errors specific to pipeline execution.
#[derive(Debug, Error)]
pub enum PipelineError {
    /// A step references a tool not in the allowlist.
    #[error("unknown or disallowed tool: {0}")]
    UnknownTool(String),

    /// Pipeline exceeds the maximum number of steps.
    #[error("pipeline has {0} steps, exceeding maximum of {1}")]
    TooManySteps(usize, usize),

    /// Pipeline args exceed the token budget.
    #[error("pipeline token budget exceeded: {0} > {1}")]
    TokenBudgetExceeded(usize, usize),

    /// Template interpolation detected injection attempt.
    #[error("template injection detected in step {0}: substituted value contains template syntax")]
    TemplateInjection(usize),

    /// SSRF attempt — target is a blocked network address.
    #[error("SSRF blocked: {0} resolves to a private/loopback address")]
    SsrfBlocked(String),
}

/// A single step in a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    /// Tool name to execute.
    pub tool: String,
    /// Arguments to pass to the tool.
    pub args: HashMap<String, serde_json::Value>,
}

/// Pipeline definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDefinition {
    /// Ordered list of steps.
    pub steps: Vec<PipelineStep>,
    /// Whether to execute independent steps in parallel.
    #[serde(default)]
    pub parallel: bool,
}

/// Result of a single pipeline step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Step index.
    pub index: usize,
    /// Tool name.
    pub tool: String,
    /// Whether execution succeeded.
    pub success: bool,
    /// Output from the tool.
    pub output: String,
    /// Error message if failed.
    pub error: Option<String>,
}

/// Aggregated result of a full pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    /// Results for each step.
    pub steps: Vec<StepResult>,
    /// Whether all steps succeeded.
    pub all_succeeded: bool,
}

/// Configuration for pipeline execution.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Set of tools permitted in pipelines.
    pub allowed_tools: HashSet<String>,
    /// Maximum number of steps.
    pub max_steps: usize,
    /// Maximum total estimated token cost of all args.
    pub max_token_budget: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        let mut allowed = HashSet::new();
        allowed.insert("web_search".to_string());
        allowed.insert("summarise".to_string());
        allowed.insert("read_file".to_string());
        allowed.insert("write_file".to_string());
        Self {
            allowed_tools: allowed,
            max_steps: DEFAULT_MAX_STEPS,
            max_token_budget: DEFAULT_MAX_TOKEN_BUDGET,
        }
    }
}

/// Validate a pipeline definition against security constraints.
///
/// # Security
/// - S3.1: All tools must be in the allowlist.
/// - S3.4: Step count and token budget enforcement.
pub fn validate_pipeline(
    pipeline: &PipelineDefinition,
    config: &PipelineConfig,
) -> std::result::Result<(), PipelineError> {
    // S3.4: Step cap
    if pipeline.steps.len() > config.max_steps {
        return Err(PipelineError::TooManySteps(
            pipeline.steps.len(),
            config.max_steps,
        ));
    }

    // S3.1: Tool allowlist
    for step in &pipeline.steps {
        if !config.allowed_tools.contains(&step.tool) {
            return Err(PipelineError::UnknownTool(step.tool.clone()));
        }
    }

    // S3.4: Token budget (estimate: sum of serialized arg lengths / 4)
    let total_tokens: usize = pipeline
        .steps
        .iter()
        .map(|step| {
            step.args
                .values()
                .map(|v| serde_json::to_string(v).unwrap_or_default().len() / 4)
                .sum::<usize>()
        })
        .sum();
    if total_tokens > config.max_token_budget {
        return Err(PipelineError::TokenBudgetExceeded(
            total_tokens,
            config.max_token_budget,
        ));
    }

    Ok(())
}

/// Interpolate `{{step[N].result}}` references in a string value.
///
/// # Security (S3.2)
/// - Single-pass interpolation only.
/// - After substitution, check that result does not contain `{{` sequences.
/// - Rejects if substituted values themselves contain template syntax.
pub fn interpolate_templates(
    value: &str,
    results: &[StepResult],
    step_index: usize,
) -> std::result::Result<String, PipelineError> {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;

    while let Some(start) = remaining.find("{{step[") {
        output.push_str(&remaining[..start]);
        let after_open = &remaining[start + 7..]; // skip "{{step["

        // Find the closing pattern "].result}}"
        let Some(bracket_pos) = after_open.find("].result}}") else {
            // Not a valid template, keep as-is
            output.push_str(&remaining[start..start + 7]);
            remaining = after_open;
            continue;
        };

        let index_str = &after_open[..bracket_pos];
        let Ok(ref_index) = index_str.parse::<usize>() else {
            // Invalid index, keep as-is
            output.push_str(&remaining[start..start + 7 + bracket_pos + 10]);
            remaining = &after_open[bracket_pos + 10..];
            continue;
        };

        // Get the referenced result
        let substitution = if ref_index < results.len() {
            &results[ref_index].output
        } else {
            ""
        };

        // S3.2: Check substitution for template syntax
        if substitution.contains("{{") {
            return Err(PipelineError::TemplateInjection(step_index));
        }

        output.push_str(substitution);
        remaining = &after_open[bracket_pos + 10..]; // skip "].result}}"
    }

    output.push_str(remaining);

    // Final safety check: no remaining template patterns in output
    if output.contains("{{step[") && output.contains("].result}}") {
        return Err(PipelineError::TemplateInjection(step_index));
    }

    Ok(output)
}

/// Interpolate templates in all string values within a step's args.
pub fn interpolate_step_args(
    args: &HashMap<String, serde_json::Value>,
    results: &[StepResult],
    step_index: usize,
) -> std::result::Result<HashMap<String, serde_json::Value>, PipelineError> {
    let mut interpolated = HashMap::new();
    for (key, value) in args {
        let new_value = interpolate_json_value(value, results, step_index)?;
        interpolated.insert(key.clone(), new_value);
    }
    Ok(interpolated)
}

fn interpolate_json_value(
    value: &serde_json::Value,
    results: &[StepResult],
    step_index: usize,
) -> std::result::Result<serde_json::Value, PipelineError> {
    match value {
        serde_json::Value::String(s) => {
            let interpolated = interpolate_templates(s, results, step_index)?;
            Ok(serde_json::Value::String(interpolated))
        }
        serde_json::Value::Array(arr) => {
            let new_arr: std::result::Result<Vec<_>, _> = arr
                .iter()
                .map(|v| interpolate_json_value(v, results, step_index))
                .collect();
            Ok(serde_json::Value::Array(new_arr?))
        }
        serde_json::Value::Object(obj) => {
            let mut new_obj = serde_json::Map::new();
            for (k, v) in obj {
                new_obj.insert(k.clone(), interpolate_json_value(v, results, step_index)?);
            }
            Ok(serde_json::Value::Object(new_obj))
        }
        // Numbers, booleans, nulls — pass through
        other => Ok(other.clone()),
    }
}

/// Check if an IP address is in a private/loopback/link-local range.
///
/// # Security (S3.3)
/// Blocks: 127.0.0.0/8, 169.254.0.0/16, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, ::1
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8 (loopback)
            if octets[0] == 127 {
                return true;
            }
            // 169.254.0.0/16 (link-local / cloud metadata)
            if octets[0] == 169 && octets[1] == 254 {
                return true;
            }
            // 10.0.0.0/8
            if octets[0] == 10 {
                return true;
            }
            // 172.16.0.0/12
            if octets[0] == 172 && (16..=31).contains(&octets[1]) {
                return true;
            }
            // 192.168.0.0/16
            if octets[0] == 192 && octets[1] == 168 {
                return true;
            }
            // 0.0.0.0
            if octets == [0, 0, 0, 0] {
                return true;
            }
            false
        }
        IpAddr::V6(v6) => {
            // ::1 (loopback)
            v6.is_loopback()
                // Mapped v4 addresses
                || v6.to_ipv4_mapped().is_some_and(|v4| is_blocked_ip(&IpAddr::V4(v4)))
        }
    }
}

/// Check if a URL target is blocked by SSRF rules.
///
/// # Security (S3.3)
/// Parses the host from the URL and checks against blocked IP ranges.
pub fn check_ssrf(url: &str) -> std::result::Result<(), PipelineError> {
    // Extract host from URL
    let host = extract_host(url);
    if host.is_empty() {
        return Ok(());
    }

    // Try parsing as IP directly
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(&ip) {
            return Err(PipelineError::SsrfBlocked(url.to_string()));
        }
    }

    // Check common localhost aliases
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower == "0.0.0.0"
        || lower.ends_with(".local")
        || lower == "metadata.google.internal"
    {
        return Err(PipelineError::SsrfBlocked(url.to_string()));
    }

    Ok(())
}

fn extract_host(url: &str) -> String {
    // Simple URL host extraction without pulling in a URL parsing crate
    let without_scheme = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else {
        url
    };

    // Remove userinfo@
    let without_userinfo = if let Some(pos) = without_scheme.find('@') {
        &without_scheme[pos + 1..]
    } else {
        without_scheme
    };

    // Take until port/path
    let host = without_userinfo
        .split([':', '/', '?', '#'])
        .next()
        .unwrap_or("");

    host.to_string()
}

/// Check all URL-like string args in a step for SSRF.
pub fn check_step_ssrf(
    args: &HashMap<String, serde_json::Value>,
) -> std::result::Result<(), PipelineError> {
    for value in args.values() {
        check_value_ssrf(value)?;
    }
    Ok(())
}

fn check_value_ssrf(value: &serde_json::Value) -> std::result::Result<(), PipelineError> {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("://") || s.starts_with("//") {
                check_ssrf(s)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                check_value_ssrf(v)?;
            }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values() {
                check_value_ssrf(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> PipelineConfig {
        PipelineConfig::default()
    }

    #[test]
    fn test_unknown_tool_is_rejected() {
        let config = default_config();
        let pipeline = PipelineDefinition {
            steps: vec![PipelineStep {
                tool: "read_secret".to_string(),
                args: HashMap::new(),
            }],
            parallel: false,
        };

        let result = validate_pipeline(&pipeline, &config);
        assert!(result.is_err());
        match result.unwrap_err() {
            PipelineError::UnknownTool(name) => assert_eq!(name, "read_secret"),
            other => panic!("expected UnknownTool, got {other:?}"),
        }
    }

    #[test]
    fn test_template_not_double_expanded() {
        // A result that contains template syntax
        let results = vec![StepResult {
            index: 0,
            tool: "web_search".to_string(),
            success: true,
            output: "{{step[1].result}} injection attempt".to_string(),
            error: None,
        }];

        let template = "Process: {{step[0].result}}";
        let result = interpolate_templates(template, &results, 1);
        assert!(result.is_err());
        match result.unwrap_err() {
            PipelineError::TemplateInjection(idx) => assert_eq!(idx, 1),
            other => panic!("expected TemplateInjection, got {other:?}"),
        }
    }

    #[test]
    fn test_template_valid_interpolation() {
        let results = vec![StepResult {
            index: 0,
            tool: "web_search".to_string(),
            success: true,
            output: "search results here".to_string(),
            error: None,
        }];

        let template = "Summarise this: {{step[0].result}}";
        let result = interpolate_templates(template, &results, 1).unwrap();
        assert_eq!(result, "Summarise this: search results here");
    }

    #[test]
    fn test_template_no_templates() {
        let template = "Just a plain string";
        let result = interpolate_templates(template, &[], 0).unwrap();
        assert_eq!(result, "Just a plain string");
    }

    #[test]
    fn test_ssrf_localhost_blocked() {
        assert!(check_ssrf("http://127.0.0.1/admin").is_err());
        assert!(check_ssrf("http://127.0.0.1:8080/api").is_err());
        assert!(check_ssrf("http://localhost/api").is_err());
        assert!(check_ssrf("https://0.0.0.0/path").is_err());
    }

    #[test]
    fn test_ssrf_metadata_endpoint_blocked() {
        // AWS/GCP metadata endpoint
        assert!(check_ssrf("http://169.254.169.254/latest/meta-data/").is_err());
        // Google metadata
        assert!(check_ssrf("http://metadata.google.internal/computeMetadata/v1/").is_err());
    }

    #[test]
    fn test_ssrf_private_ranges_blocked() {
        assert!(check_ssrf("http://10.0.0.1/api").is_err());
        assert!(check_ssrf("http://172.16.0.1/api").is_err());
        assert!(check_ssrf("http://172.31.255.255/api").is_err());
        assert!(check_ssrf("http://192.168.1.1/api").is_err());
    }

    #[test]
    fn test_ssrf_public_ip_allowed() {
        assert!(check_ssrf("https://93.184.216.34/api").is_ok());
        assert!(check_ssrf("https://example.com/api").is_ok());
    }

    #[test]
    fn test_step_cap_enforced() {
        let config = PipelineConfig {
            max_steps: 20,
            ..PipelineConfig::default()
        };

        let steps: Vec<PipelineStep> = (0..21)
            .map(|_| PipelineStep {
                tool: "web_search".to_string(),
                args: HashMap::new(),
            })
            .collect();

        let pipeline = PipelineDefinition {
            steps,
            parallel: false,
        };

        let result = validate_pipeline(&pipeline, &config);
        assert!(result.is_err());
        match result.unwrap_err() {
            PipelineError::TooManySteps(count, max) => {
                assert_eq!(count, 21);
                assert_eq!(max, 20);
            }
            other => panic!("expected TooManySteps, got {other:?}"),
        }
    }

    #[test]
    fn test_valid_pipeline_passes() {
        let config = default_config();
        let pipeline = PipelineDefinition {
            steps: vec![
                PipelineStep {
                    tool: "web_search".to_string(),
                    args: {
                        let mut m = HashMap::new();
                        m.insert(
                            "query".to_string(),
                            serde_json::Value::String("rust async".to_string()),
                        );
                        m
                    },
                },
                PipelineStep {
                    tool: "summarise".to_string(),
                    args: {
                        let mut m = HashMap::new();
                        m.insert(
                            "text".to_string(),
                            serde_json::Value::String("{{step[0].result}}".to_string()),
                        );
                        m
                    },
                },
            ],
            parallel: false,
        };

        assert!(validate_pipeline(&pipeline, &config).is_ok());
    }

    #[test]
    fn test_is_blocked_ip_ipv6_loopback() {
        let ip: IpAddr = "::1".parse().unwrap();
        assert!(is_blocked_ip(&ip));
    }

    #[test]
    fn test_is_blocked_ip_public() {
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!is_blocked_ip(&ip));
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(extract_host("http://example.com/path"), "example.com");
        assert_eq!(extract_host("https://127.0.0.1:8080/api"), "127.0.0.1");
        assert_eq!(extract_host("http://user:pass@host.com/path"), "host.com");
    }

    #[test]
    fn test_interpolate_step_args() {
        let results = vec![StepResult {
            index: 0,
            tool: "web_search".to_string(),
            success: true,
            output: "found results".to_string(),
            error: None,
        }];

        let mut args = HashMap::new();
        args.insert(
            "text".to_string(),
            serde_json::Value::String("Process: {{step[0].result}}".to_string()),
        );
        args.insert(
            "count".to_string(),
            serde_json::Value::Number(serde_json::Number::from(5)),
        );

        let interpolated = interpolate_step_args(&args, &results, 1).unwrap();
        assert_eq!(
            interpolated["text"].as_str().unwrap(),
            "Process: found results"
        );
        assert_eq!(interpolated["count"].as_i64().unwrap(), 5);
    }

    #[test]
    fn test_check_step_ssrf() {
        let mut args = HashMap::new();
        args.insert(
            "url".to_string(),
            serde_json::Value::String("http://169.254.169.254/meta".to_string()),
        );

        assert!(check_step_ssrf(&args).is_err());

        let mut safe_args = HashMap::new();
        safe_args.insert(
            "url".to_string(),
            serde_json::Value::String("https://example.com/api".to_string()),
        );

        assert!(check_step_ssrf(&safe_args).is_ok());
    }

    #[test]
    fn test_token_budget_enforced() {
        let config = PipelineConfig {
            max_token_budget: 10,
            ..PipelineConfig::default()
        };

        let pipeline = PipelineDefinition {
            steps: vec![PipelineStep {
                tool: "web_search".to_string(),
                args: {
                    let mut m = HashMap::new();
                    // Create a large value to exceed budget
                    m.insert(
                        "query".to_string(),
                        serde_json::Value::String("a".repeat(1000)),
                    );
                    m
                },
            }],
            parallel: false,
        };

        let result = validate_pipeline(&pipeline, &config);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PipelineError::TokenBudgetExceeded(_, _)
        ));
    }
}
