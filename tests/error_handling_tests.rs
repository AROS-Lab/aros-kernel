use std::sync::Arc;

use tempfile::TempDir;
use tokio::sync::RwLock;

use aros_kernel::agent::claude_cli::ClaudeCliAgent;
use aros_kernel::agent::shell::ShellAgent;
use aros_kernel::agent::types::AgentType;
use aros_kernel::dag::executor::{DagExecutor, TaskExecutor};
use aros_kernel::dag::graph::{AgentLevel, DagGraph, Node, NodeStatus};
use aros_kernel::dag::persistence::{DagPersistence, PersistenceError};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_node(id: &str, deps: Vec<&str>) -> Node {
    Node {
        id: id.to_string(),
        title: format!("Task {id}"),
        description: String::new(),
        depends_on: deps.into_iter().map(String::from).collect(),
        status: NodeStatus::Pending,
        agent_level: AgentLevel::Agent,
        output_files: vec![],
        retry_count: 0,
        result: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Corrupted JSON checkpoint
// ---------------------------------------------------------------------------

#[test]
fn test_corrupted_json_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().join("checkpoint");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("dag.json"), "{{not valid json}}").unwrap();

    let persistence = DagPersistence::new(&base);
    let result = persistence.load_checkpoint();

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), PersistenceError::Serde(_)),
        "Expected PersistenceError::Serde for corrupted JSON"
    );
}

// ---------------------------------------------------------------------------
// 2. Save to read-only directory
// ---------------------------------------------------------------------------

#[test]
fn test_save_to_readonly_dir() {
    // Skip on CI or if running as root (permissions not enforceable).
    if std::env::var("CI").is_ok() {
        eprintln!("Skipping test_save_to_readonly_dir on CI");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let readonly_dir = tmp.path().join("readonly");
    std::fs::create_dir_all(&readonly_dir).unwrap();

    // Make the directory read-only.
    let mut perms = std::fs::metadata(&readonly_dir).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&readonly_dir, perms).unwrap();

    let persistence = DagPersistence::new(readonly_dir.join("subdir"));
    let graph = {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g
    };

    let result = persistence.save_checkpoint(&graph);

    // Restore permissions so TempDir cleanup works.
    let mut perms = std::fs::metadata(&readonly_dir).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&readonly_dir, perms).unwrap();

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), PersistenceError::Io(_)),
        "Expected PersistenceError::Io for read-only directory"
    );
}

// ---------------------------------------------------------------------------
// 3. Shell agent with invalid UTF-8
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_shell_invalid_utf8() {
    let agent = ShellAgent::new();
    // printf outputs raw bytes that are not valid UTF-8.
    let result = agent.execute("printf '\\x80\\x81\\x82'", 5).await;

    // The key assertion: it should not panic. Output may contain replacement chars.
    assert!(result.success);
}

// ---------------------------------------------------------------------------
// 4. Missing binary execution via ClaudeCliAgent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_missing_binary_execution() {
    let agent = ClaudeCliAgent::with_binary("/nonexistent/binary/path");
    let result = agent.execute("hello", 5).await;

    assert!(!result.success, "Expected failure for nonexistent binary");
    let error = result.error.expect("Expected error message");
    // The error should mention spawn failure (which implies binary not found).
    assert!(
        error.contains("spawn") || error.contains("No such file") || error.contains("not found"),
        "Error should indicate binary not found, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// 5. Dependency on nonexistent node
// ---------------------------------------------------------------------------

#[test]
fn test_dependency_on_nonexistent_node() {
    let mut g = DagGraph::new();
    // Node A depends on "ghost" which does not exist in the graph.
    g.add_node(make_node("a", vec!["ghost"])).unwrap();

    let ready = g.ready_nodes();
    // "a" should NOT be ready because "ghost" is not Done (it doesn't exist).
    assert!(
        ready.is_empty(),
        "Node with dependency on nonexistent node should not be ready"
    );
}

// ---------------------------------------------------------------------------
// 6. Partial checkpoint recovery
// ---------------------------------------------------------------------------

#[test]
fn test_partial_checkpoint_recovery() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().join("checkpoint");
    let persistence = DagPersistence::new(&base);

    // Create a 3-node DAG and save checkpoint.
    let mut graph = DagGraph::new();
    graph.add_node(make_node("n1", vec![])).unwrap();
    graph.add_node(make_node("n2", vec!["n1"])).unwrap();
    graph.add_node(make_node("n3", vec!["n1"])).unwrap();
    graph.get_node_mut("n1").unwrap().status = NodeStatus::Done;

    persistence.save_checkpoint(&graph).unwrap();

    // Delete one node's state file from state/ directory.
    let state_file = base.join("state/node-n2.json");
    assert!(state_file.exists(), "State file should exist before deletion");
    std::fs::remove_file(&state_file).unwrap();

    // Load checkpoint — dag.json is the source of truth, not individual state files.
    let loaded = persistence.load_checkpoint().unwrap();
    assert_eq!(loaded.node_count(), 3);
    assert!(loaded.get_node("n1").is_some());
    assert!(loaded.get_node("n2").is_some());
    assert!(loaded.get_node("n3").is_some());
    assert_eq!(loaded.get_node("n1").unwrap().status, NodeStatus::Done);
}

// ---------------------------------------------------------------------------
// 7. Executor with panicking task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_executor_with_panicking_task() {
    let mut graph = DagGraph::new();
    graph.add_node(make_node("panic-node", vec![])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 4);

    // TaskExecutor that panics inside the spawned task.
    let task_fn: TaskExecutor = Arc::new(|_node: Node| {
        tokio::spawn(async move {
            panic!("intentional test panic");
        })
    });

    let results = executor.execute_tick(&task_fn).await;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, "panic-node");
    assert!(!results[0].result.success, "Panicking task should be marked as failed");
    assert!(
        results[0].result.error.as_ref().unwrap().contains("panic"),
        "Error should mention panic"
    );

    // Verify node is marked Failed in the graph.
    let g = graph.read().await;
    assert_eq!(
        g.get_node("panic-node").unwrap().status,
        NodeStatus::Failed,
        "Panicking node should be marked Failed"
    );
}
