use std::time::Instant;

use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::scheduler::admission::ResourceRequirements;

use super::types::{AgentResult, AgentType};

pub struct ShellAgent {
    shell: String,
    cwd: Option<String>,
}

impl ShellAgent {
    pub fn new() -> Self {
        Self {
            shell: "/bin/sh".to_string(),
            cwd: None,
        }
    }

    pub fn with_shell(shell: impl Into<String>) -> Self {
        Self {
            shell: shell.into(),
            cwd: None,
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

impl Default for ShellAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentType for ShellAgent {
    async fn execute(&self, task: &str, timeout_secs: u64) -> AgentResult {
        let start = Instant::now();

        let mut cmd = Command::new(&self.shell);
        cmd.arg("-c").arg(task);

        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }

        // Capture stdout and stderr
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // Kill the process group on drop
        cmd.kill_on_drop(true);

        let result = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;

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
                error: Some(format!("Failed to spawn process: {e}")),
                exit_code: None,
                duration_secs,
            },
            Err(_) => {
                // Timeout — kill_on_drop handles cleanup
                AgentResult {
                    success: false,
                    output: String::new(),
                    error: Some("Process timed out".to_string()),
                    exit_code: None,
                    duration_secs,
                }
            }
        }
    }

    fn resource_requirements(&self) -> ResourceRequirements {
        ResourceRequirements::shell()
    }

    fn name(&self) -> &str {
        "shell"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shell_execute_echo() {
        let agent = ShellAgent::new();
        let result = agent.execute("echo hello", 5).await;
        assert!(result.success);
        assert!(result.output.contains("hello"));
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn test_shell_execute_failure() {
        let agent = ShellAgent::new();
        let result = agent.execute("exit 1", 5).await;
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
    }

    #[tokio::test]
    async fn test_shell_timeout() {
        let agent = ShellAgent::new();
        let result = agent.execute("sleep 10", 1).await;
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn test_shell_resource_requirements() {
        let agent = ShellAgent::new();
        let req = agent.resource_requirements();
        assert_eq!(req.cpu_millicores, 200);
        assert_eq!(req.memory_mb, 50);
    }
}
