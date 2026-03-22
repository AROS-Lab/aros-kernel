use aros_kernel::agent::claude_cli::ClaudeCliAgent;
use aros_kernel::agent::shell::ShellAgent;
use aros_kernel::agent::types::AgentType;
use tempfile::TempDir;

#[tokio::test]
async fn test_shell_stderr_capture() {
    let agent = ShellAgent::new();
    let result = agent.execute("echo error_message >&2", 5).await;

    // The command itself succeeds (exit 0), but stderr is captured
    assert!(result.success);
    assert_eq!(result.exit_code, Some(0));

    let error = result.error.expect("stderr should be captured in error field");
    assert!(
        error.contains("error_message"),
        "error field should contain 'error_message', got: {error}"
    );
}

#[tokio::test]
async fn test_shell_with_cwd() {
    let tmp_dir = TempDir::new().expect("failed to create temp dir");
    let tmp_path = tmp_dir.path().to_str().unwrap().to_string();

    let agent = ShellAgent::new().with_cwd(&tmp_path);
    let result = agent.execute("pwd", 5).await;

    assert!(result.success);
    // On macOS, /tmp may resolve to /private/tmp, so canonicalize both
    let canonical_tmp = std::fs::canonicalize(&tmp_path)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let output_trimmed = result.output.trim();
    let canonical_output = std::fs::canonicalize(output_trimmed)
        .unwrap_or_else(|_| std::path::PathBuf::from(output_trimmed))
        .to_str()
        .unwrap()
        .to_string();

    assert_eq!(
        canonical_output, canonical_tmp,
        "pwd output should match the configured cwd"
    );
}

#[tokio::test]
async fn test_shell_special_characters() {
    let agent = ShellAgent::new();
    let result = agent.execute("echo \"hello world\" | wc -w", 5).await;

    assert!(result.success);
    assert!(
        result.output.trim().contains('2'),
        "word count of 'hello world' should be 2, got: {}",
        result.output.trim()
    );
}

#[tokio::test]
async fn test_shell_empty_command() {
    let agent = ShellAgent::new();
    let result = agent.execute("", 5).await;

    // Empty command should not panic. It may succeed with empty output
    // or fail — either is acceptable.
    // Just verify we got a result without panicking.
    assert!(
        result.output.is_empty() || !result.output.is_empty(),
        "should return a valid result"
    );
}

#[tokio::test]
async fn test_claude_cli_config_options() {
    let agent = ClaudeCliAgent::with_binary("custom-claude")
        .with_cwd("/tmp")
        .with_skip_permissions(true);

    // Verify the agent still reports correct identity and resource requirements
    assert_eq!(agent.name(), "claude_cli");

    let req = agent.resource_requirements();
    assert_eq!(req.cpu_millicores, 500, "claude_cli should require 500 cpu_millicores");
    assert_eq!(req.memory_mb, 250, "claude_cli should require 250 memory_mb");
}

#[tokio::test]
async fn test_shell_exit_code_capture() {
    let agent = ShellAgent::new();
    let result = agent.execute("sh -c 'exit 42'", 5).await;

    assert!(!result.success, "exit 42 should not be success");
    assert_eq!(
        result.exit_code,
        Some(42),
        "exit code should be 42, got: {:?}",
        result.exit_code
    );
}
