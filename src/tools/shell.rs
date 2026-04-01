use super::traits::{Tool, ToolResult};
use crate::config::ProcessLimitsConfig;
use crate::runtime::RuntimeAdapter;
use crate::security::SecurityPolicy;
use crate::security::traits::Sandbox;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Default maximum shell command execution time before kill.
const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
#[cfg(not(target_os = "windows"))]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Environment variables safe to pass to shell commands on Windows.
/// Includes Windows-specific variables needed for cmd.exe and program resolution.
#[cfg(target_os = "windows")]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

/// Shell persistence commands that create background-persistent processes.
/// Blocked when `process_limits.max_respawns == 0`.
const PERSISTENCE_COMMANDS: &[&str] = &["nohup", "setsid", "screen", "tmux", "disown"];

/// Shell command execution tool with sandboxing
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    sandbox: Arc<dyn Sandbox>,
    timeout_secs: u64,
    process_limits: ProcessLimitsConfig,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self {
            security,
            runtime,
            sandbox: Arc::new(crate::security::NoopSandbox),
            timeout_secs: DEFAULT_SHELL_TIMEOUT_SECS,
            process_limits: ProcessLimitsConfig::default(),
        }
    }

    pub fn new_with_sandbox(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            security,
            runtime,
            sandbox,
            timeout_secs: DEFAULT_SHELL_TIMEOUT_SECS,
            process_limits: ProcessLimitsConfig::default(),
        }
    }

    /// Override the command execution timeout (in seconds).
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Attach process limits for TTL clamping and persistence blocking.
    pub fn with_process_limits(mut self, limits: ProcessLimitsConfig) -> Self {
        self.process_limits = limits;
        self
    }

    /// Check whether a command uses a persistence wrapper (nohup, setsid, etc.)
    /// that would create a background-persistent process.
    fn uses_persistence_command(command: &str) -> bool {
        // Only check command positions: the first token and any token immediately
        // after a shell operator (&&, ||, ;, |). This avoids false positives from
        // persistence keywords appearing as arguments (e.g. `echo "connect to tmux"`)
        // and catches bypass attempts via quoted wrappers (e.g. `"nohup" cmd`).
        let mut check_next = true;
        for word in command.split_whitespace() {
            if check_next {
                let clean = word.trim_matches(|c: char| c == '"' || c == '\'' || c == '\\');
                let base = clean.rsplit('/').next().unwrap_or(clean);
                if PERSISTENCE_COMMANDS.contains(&base) {
                    return true;
                }
            }
            check_next = matches!(word, "&&" | "||" | ";" | "|");
        }
        false
    }
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk commands in supervised mode",
                    "default": false
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;
        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match self.security.validate_command_execution(command, approved) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        // Block persistence commands when max_respawns == 0 (no background
        // persistence without explicit opt-in).
        if self.process_limits.max_respawns == 0 && Self::uses_persistence_command(command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Command refused: persistence commands (nohup, setsid, screen, tmux, disown) \
                     are blocked by process_limits.max_respawns=0. \
                     Background-persistent processes require explicit owner opt-in."
                        .to_string(),
                ),
            });
        }

        // Execute with timeout to prevent hanging commands.
        // Clear the environment to prevent leaking API keys and other secrets
        // (CWE-200), then re-add only safe, functional variables.
        let mut cmd = match self
            .runtime
            .build_shell_command(command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };

        // Apply sandbox wrapping before execution.
        // The Sandbox trait operates on std::process::Command, so use as_std_mut()
        // to get a mutable reference to the underlying command.
        self.sandbox
            .wrap_command(cmd.as_std_mut())
            .map_err(|e| anyhow::anyhow!("Sandbox error: {}", e))?;

        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        // Clamp timeout to the global TTL ceiling from process_limits.
        let timeout_secs = self
            .timeout_secs
            .min(self.process_limits.max_shell_ttl_secs);
        let result = tokio::time::timeout(Duration::from_secs(timeout_secs), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate output to prevent OOM
                if stdout.len() > MAX_OUTPUT_BYTES {
                    let mut b = MAX_OUTPUT_BYTES.min(stdout.len());
                    while b > 0 && !stdout.is_char_boundary(b) {
                        b -= 1;
                    }
                    stdout.truncate(b);
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    let mut b = MAX_OUTPUT_BYTES.min(stderr.len());
                    while b > 0 && !stderr.is_char_boundary(b) {
                        b -= 1;
                    }
                    stderr.truncate(b);
                    stderr.push_str("\n... [stderr truncated at 1MB]");
                }

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Command timed out after {timeout_secs}s and was killed"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::wrappers::{PathGuardedTool, RateLimitedTool};

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    /// Returns the fully-wrapped shell tool as it is composed in production:
    /// RateLimited(PathGuarded(ShellTool)).  Tests that verify path-blocking or
    /// rate-limiting behaviour must use this helper so they exercise the wrappers.
    fn wrapped_shell(security: Arc<SecurityPolicy>) -> RateLimitedTool<PathGuardedTool<ShellTool>> {
        RateLimitedTool::new(
            PathGuardedTool::new(
                ShellTool::new(security.clone(), test_runtime()),
                security.clone(),
            ),
            security,
        )
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required field should be an array")
                .contains(&json!("command"))
        );
        assert!(schema["properties"]["approved"].is_object());
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "rm -rf /"}))
            .await
            .expect("disallowed command execution should return a result");
        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("not allowed") || error.contains("high-risk"));
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool
            .execute(json!({"command": "ls"}))
            .await
            .expect("readonly command execution should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .expect("error field should be present for blocked command")
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .expect("command with nonexistent path should return a result");
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_blocks_absolute_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "cat /etc/passwd"}))
            .await
            .expect("absolute path argument should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_option_assignment_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "grep --file=/etc/passwd root ./src"}))
            .await
            .expect("option-assigned forbidden path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_short_option_attached_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "grep -f/etc/passwd root ./src"}))
            .await
            .expect("short option attached forbidden path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_tilde_user_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "cat ~root/.ssh/id_rsa"}))
            .await
            .expect("tilde-user path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_input_redirection_path_bypass() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat </etc/passwd"}))
            .await
            .expect("input redirection bypass should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into(), "echo".into()],
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                // SAFETY: test-only, single-threaded test runner.
                Some(val) => unsafe { std::env::set_var(self.key, val) },
                // SAFETY: test-only, single-threaded test runner.
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("HRAFN_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "HRAFN_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home_for_env_command() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command should succeed");
        assert!(result.success);
        assert!(
            result.output.contains("HOME="),
            "HOME should be available in shell environment"
        );
        assert!(
            result.output.contains("PATH="),
            "PATH should be available in shell environment"
        );
    }

    #[tokio::test]
    async fn shell_blocks_plain_variable_expansion() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .expect("plain variable expansion should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("HRAFN_TEST_PASSTHROUGH", "db://unit-test");
        let tool = ShellTool::new(
            test_security_with_env_passthrough(&["HRAFN_TEST_PASSTHROUGH"]),
            test_runtime(),
        );

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(
            result
                .output
                .contains("HRAFN_TEST_PASSTHROUGH=db://unit-test")
        );
    }

    #[test]
    fn invalid_shell_env_passthrough_names_are_filtered() {
        let security = SecurityPolicy {
            shell_env_passthrough: vec![
                "VALID_NAME".into(),
                "BAD-NAME".into(),
                "1NOPE".into(),
                "ALSO_VALID".into(),
            ],
            ..SecurityPolicy::default()
        };
        let vars = collect_allowed_shell_env_vars(&security);
        assert!(vars.contains(&"VALID_NAME".to_string()));
        assert!(vars.contains(&"ALSO_VALID".to_string()));
        assert!(!vars.contains(&"BAD-NAME".to_string()));
        assert!(!vars.contains(&"1NOPE".to_string()));
    }

    #[tokio::test]
    async fn shell_requires_approval_for_medium_risk_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["touch".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security.clone(), test_runtime());
        let denied = tool
            .execute(json!({"command": "touch hrafn_shell_approval_test"}))
            .await
            .expect("unapproved command should return a result");
        assert!(!denied.success);
        assert!(
            denied
                .error
                .as_deref()
                .unwrap_or("")
                .contains("explicit approval")
        );

        let allowed = tool
            .execute(json!({
                "command": "touch hrafn_shell_approval_test",
                "approved": true
            }))
            .await
            .expect("approved command execution should succeed");
        assert!(allowed.success);

        let _ =
            tokio::fs::remove_file(std::env::temp_dir().join("hrafn_shell_approval_test")).await;
    }

    // ── shell timeout enforcement tests ─────────────────

    #[test]
    fn shell_timeout_default_is_reasonable() {
        assert_eq!(
            DEFAULT_SHELL_TIMEOUT_SECS, 60,
            "default shell timeout must be 60 seconds"
        );
    }

    #[test]
    fn shell_timeout_can_be_overridden() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_timeout_secs(120);
        assert_eq!(tool.timeout_secs, 120);
    }

    #[test]
    fn shell_output_limit_is_1mb() {
        assert_eq!(
            MAX_OUTPUT_BYTES, 1_048_576,
            "max output must be 1 MB to prevent OOM"
        );
    }

    // ── Non-UTF8 binary output tests ────────────────────

    #[test]
    fn shell_safe_env_vars_excludes_secrets() {
        for var in SAFE_ENV_VARS {
            let lower = var.to_lowercase();
            assert!(
                !lower.contains("key") && !lower.contains("secret") && !lower.contains("token"),
                "SAFE_ENV_VARS must not include sensitive variable: {var}"
            );
        }
    }

    #[test]
    fn shell_safe_env_vars_includes_essentials() {
        assert!(
            SAFE_ENV_VARS.contains(&"PATH"),
            "PATH must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"HOME") || SAFE_ENV_VARS.contains(&"USERPROFILE"),
            "HOME or USERPROFILE must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"TERM"),
            "TERM must be in safe env vars"
        );
    }

    #[tokio::test]
    async fn shell_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);
        let result = tool
            .execute(json!({"command": "echo test"}))
            .await
            .expect("rate-limited command should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn shell_handles_nonexistent_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime());
        let result = tool
            .execute(json!({"command": "echo error_msg >&2"}))
            .await
            .unwrap();
        assert!(result.error.as_deref().unwrap_or("").contains("error_msg"));
    }

    #[tokio::test]
    async fn shell_record_action_budget_exhaustion() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 1,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);

        let r1 = tool
            .execute(json!({"command": "echo first"}))
            .await
            .unwrap();
        assert!(r1.success);

        let r2 = tool
            .execute(json!({"command": "echo second"}))
            .await
            .unwrap();
        assert!(!r2.success);
        assert!(
            r2.error.as_deref().unwrap_or("").contains("Rate limit")
                || r2.error.as_deref().unwrap_or("").contains("budget")
        );
    }

    // ── Sandbox integration tests ────────────────────────

    #[test]
    fn shell_tool_can_be_constructed_with_sandbox() {
        use crate::security::NoopSandbox;

        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let tool = ShellTool::new_with_sandbox(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            sandbox,
        );
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn noop_sandbox_does_not_modify_command() {
        use crate::security::NoopSandbox;

        let sandbox = NoopSandbox;
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello");

        let program_before = cmd.get_program().to_os_string();
        let args_before: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();

        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command should succeed");

        assert_eq!(cmd.get_program(), program_before);
        assert_eq!(
            cmd.get_args().map(|a| a.to_os_string()).collect::<Vec<_>>(),
            args_before
        );
    }

    #[tokio::test]
    async fn shell_executes_with_sandbox() {
        use crate::security::NoopSandbox;

        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let tool = ShellTool::new_with_sandbox(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            sandbox,
        );
        let result = tool
            .execute(json!({"command": "echo sandbox_test"}))
            .await
            .expect("command with sandbox should succeed");
        assert!(result.success);
        assert!(result.output.contains("sandbox_test"));
    }

    // ── Process limits tests ────────────────────────────────

    #[test]
    fn uses_persistence_command_detects_nohup() {
        assert!(ShellTool::uses_persistence_command("nohup ./server.sh"));
        assert!(ShellTool::uses_persistence_command("/usr/bin/nohup ./run"));
    }

    #[test]
    fn uses_persistence_command_detects_setsid() {
        assert!(ShellTool::uses_persistence_command("setsid ./daemon"));
    }

    #[test]
    fn uses_persistence_command_detects_screen_tmux() {
        assert!(ShellTool::uses_persistence_command("screen -d -m ./run"));
        assert!(ShellTool::uses_persistence_command("tmux new-session -d"));
    }

    #[test]
    fn uses_persistence_command_detects_disown() {
        assert!(ShellTool::uses_persistence_command("disown %1"));
    }

    #[test]
    fn uses_persistence_command_detects_quoted_wrapper() {
        assert!(ShellTool::uses_persistence_command("\"nohup\" ./server.sh"));
        assert!(ShellTool::uses_persistence_command("'tmux' new-session -d"));
    }

    #[test]
    fn uses_persistence_command_detects_after_operator() {
        assert!(ShellTool::uses_persistence_command(
            "echo hi && nohup ./server.sh"
        ));
        assert!(ShellTool::uses_persistence_command(
            "echo hi || screen -d -m ./run"
        ));
        assert!(ShellTool::uses_persistence_command(
            "echo hi ; tmux new-session -d"
        ));
        assert!(ShellTool::uses_persistence_command(
            "cat file | nohup tee out"
        ));
    }

    #[test]
    fn uses_persistence_command_ignores_normal_commands() {
        assert!(!ShellTool::uses_persistence_command("echo hello"));
        assert!(!ShellTool::uses_persistence_command("ls -la"));
        assert!(!ShellTool::uses_persistence_command("cargo build"));
    }

    #[test]
    fn uses_persistence_command_ignores_arguments() {
        assert!(!ShellTool::uses_persistence_command(
            "echo \"connect to tmux\""
        ));
        assert!(!ShellTool::uses_persistence_command("echo screen"));
        assert!(!ShellTool::uses_persistence_command(
            "grep nohup logfile.txt"
        ));
        assert!(!ShellTool::uses_persistence_command(
            "ls -la /usr/bin/nohup"
        ));
    }

    #[tokio::test]
    async fn shell_blocks_persistence_command_by_default() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["nohup".into(), "echo".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        // Default process_limits has max_respawns=0
        let result = tool
            .execute(json!({"command": "nohup echo test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("persistence commands")
        );
    }

    #[tokio::test]
    async fn shell_allows_persistence_when_respawns_configured() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            allowed_commands: vec!["nohup".into(), "echo".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let limits = ProcessLimitsConfig {
            max_respawns: 1,
            ..ProcessLimitsConfig::default()
        };
        let tool = ShellTool::new(security, test_runtime()).with_process_limits(limits);
        let result = tool
            .execute(json!({"command": "nohup echo test"}))
            .await
            .unwrap();
        // Should not be blocked by persistence check (may still be blocked by
        // other security policy, but the persistence gate should pass)
        assert!(
            result.success
                || !result
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .contains("persistence commands")
        );
    }

    #[test]
    fn shell_ttl_clamps_timeout() {
        let limits = ProcessLimitsConfig {
            max_shell_ttl_secs: 30,
            ..ProcessLimitsConfig::default()
        };
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_timeout_secs(120)
            .with_process_limits(limits);
        // The effective timeout should be min(120, 30) = 30
        assert_eq!(
            tool.timeout_secs
                .min(tool.process_limits.max_shell_ttl_secs),
            30
        );
    }
}
