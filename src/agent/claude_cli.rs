use std::time::Instant;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::scheduler::admission::ResourceRequirements;

use super::types::{AgentResult, AgentType};

pub struct ClaudeCliAgent {
    claude_bin: String,
    cwd: Option<String>,
    skip_permissions: bool,
}

impl ClaudeCliAgent {
    pub fn new() -> Self {
        Self {
            claude_bin: "claude".to_string(),
            cwd: None,
            skip_permissions: false,
        }
    }

    pub fn with_binary(bin: impl Into<String>) -> Self {
        Self {
            claude_bin: bin.into(),
            cwd: None,
            skip_permissions: false,
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_skip_permissions(mut self, skip: bool) -> Self {
        self.skip_permissions = skip;
        self
    }
}

impl Default for ClaudeCliAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentType for ClaudeCliAgent {
    async fn execute(&self, task: &str, timeout_secs: u64) -> AgentResult {
        let start = Instant::now();

        let mut cmd = Command::new(&self.claude_bin);
        cmd.arg("-p").arg("--output-format").arg("text");

        if self.skip_permissions {
            cmd.arg("--dangerously-skip-permissions");
        }

        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }

        // Prevent nested session errors
        cmd.env("CLAUDECODE", "");

        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let spawn_result = cmd.spawn();

        let mut child = match spawn_result {
            Ok(child) => child,
            Err(e) => {
                return AgentResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to spawn claude process: {e}")),
                    exit_code: None,
                    duration_secs: start.elapsed().as_secs_f64(),
                };
            }
        };

        // Write task to stdin
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(task.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }

        // Wait with timeout
        let result = timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await;

        let duration_secs = start.elapsed().as_secs_f64();

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let code = output.status.code();
                let success = output.status.success();

                AgentResult {
                    success,
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                    exit_code: code,
                    duration_secs,
                }
            }
            Ok(Err(e)) => AgentResult {
                success: false,
                output: String::new(),
                error: Some(format!("Process error: {e}")),
                exit_code: None,
                duration_secs,
            },
            Err(_) => {
                // Timeout — kill_on_drop handles cleanup
                AgentResult {
                    success: false,
                    output: String::new(),
                    error: Some("Claude CLI process timed out".to_string()),
                    exit_code: None,
                    duration_secs,
                }
            }
        }
    }

    fn resource_requirements(&self) -> ResourceRequirements {
        ResourceRequirements::claude_cli()
    }

    fn name(&self) -> &str {
        "claude_cli"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_claude_cli_resource_requirements() {
        let agent = ClaudeCliAgent::new();
        let req = agent.resource_requirements();
        assert_eq!(req.cpu_millicores, 500);
        assert_eq!(req.memory_mb, 250);
    }
}
