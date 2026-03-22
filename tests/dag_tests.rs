//! Integration tests for the DAG subsystem.
//!
//! These tests exercise cross-module interactions between graph, executor,
//! runtime, and persistence modules.

use std::sync::Arc;
use std::time::Duration;

use aros_kernel::dag::executor::{DagExecutor, TaskExecutor};
use aros_kernel::dag::graph::{AgentLevel, DagGraph, Node, NodeResult, NodeStatus};
use aros_kernel::dag::persistence::DagPersistence;
use aros_kernel::dag::runtime::RuntimeDag;
use tokio::sync::RwLock;

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

fn mock_executor(delay_ms: u64) -> TaskExecutor {
    Arc::new(move |node: Node| {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            NodeResult {
                success: true,
                output: format!("Done: {}", node.id),
                error: None,
                duration_secs: delay_ms as f64 / 1000.0,
            }
        })
    })
}

fn failing_executor(fail_ids: Vec<String>) -> TaskExecutor {
    Arc::new(move |node: Node| {
        let should_fail = fail_ids.contains(&node.id);
        tokio::spawn(async move {
            if should_fail {
                NodeResult {
                    success: false,
                    output: String::new(),
                    error: Some("Simulated failure".into()),
                    duration_secs: 0.0,
                }
            } else {
                NodeResult {
                    success: true,
                    output: format!("Done: {}", node.id),
                    error: None,
                    duration_secs: 0.01,
                }
            }
        })
    })
}

// ---------------------------------------------------------------------------
// Parallel dispatch tests
// ---------------------------------------------------------------------------

/// 5-node DAG: A, B, C are independent; D depends on A and B; E depends on C
/// and D. Verify A, B, C dispatch first (parallel), then D, then E.
#[tokio::test]
async fn test_five_node_dag_parallel() {
    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec![])).unwrap();
    graph.add_node(make_node("C", vec![])).unwrap();
    graph.add_node(make_node("D", vec!["A", "B"])).unwrap();
    graph.add_node(make_node("E", vec!["C", "D"])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = mock_executor(10);

    // Tick 1: A, B, C should all be dispatched (independent, no deps).
    let results = executor.execute_tick(&task_fn).await;
    let mut tick1_ids: Vec<String> = results.iter().map(|r| r.node_id.clone()).collect();
    tick1_ids.sort();
    assert_eq!(tick1_ids, vec!["A", "B", "C"]);

    // After tick 1: A, B, C are Done. D should now be ready. E still waits.
    {
        let g = graph.read().await;
        assert_eq!(g.get_node("A").unwrap().status, NodeStatus::Done);
        assert_eq!(g.get_node("B").unwrap().status, NodeStatus::Done);
        assert_eq!(g.get_node("C").unwrap().status, NodeStatus::Done);
        assert_eq!(g.get_node("D").unwrap().status, NodeStatus::Pending);
        assert_eq!(g.get_node("E").unwrap().status, NodeStatus::Pending);

        let ready_ids: Vec<String> = g.ready_nodes().iter().map(|n| n.id.clone()).collect();
        assert!(ready_ids.contains(&"D".to_string()));
        assert!(!ready_ids.contains(&"E".to_string()));
    }

    // Tick 2: D should be dispatched.
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, "D");

    // Tick 3: E should be dispatched.
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, "E");

    // DAG should now be complete.
    let g = graph.read().await;
    assert!(g.is_complete());
    assert_eq!(g.done_count(), 5);
}

/// 5 independent nodes with max_parallel=2. Verify at most 2 run per tick.
#[tokio::test]
async fn test_max_parallel_respected() {
    let mut graph = DagGraph::new();
    for i in 0..5 {
        graph
            .add_node(make_node(&format!("N{i}"), vec![]))
            .unwrap();
    }

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 2);
    let task_fn = mock_executor(10);

    let mut total_dispatched = 0;

    // Execute ticks until all are done, checking max_parallel each time.
    loop {
        let results = executor.execute_tick(&task_fn).await;
        if results.is_empty() {
            break;
        }
        // Each tick should dispatch at most 2.
        assert!(
            results.len() <= 2,
            "Expected at most 2 per tick, got {}",
            results.len()
        );
        total_dispatched += results.len();
    }

    assert_eq!(total_dispatched, 5);

    let g = graph.read().await;
    assert!(g.is_complete());
}

// ---------------------------------------------------------------------------
// Runtime mutation + executor integration
// ---------------------------------------------------------------------------

/// Start executing a 2-node DAG. Add a 3rd node mid-execution via RuntimeDag.
/// Verify the 3rd node eventually runs.
#[tokio::test]
async fn test_runtime_add_during_execution() {
    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec!["A"])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let rt = RuntimeDag::new(graph.clone());
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = mock_executor(10);

    // Tick 1: dispatch A.
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, "A");

    // Mid-execution: add node C that depends on A (which is now Done).
    rt.add_node(make_node("C", vec!["A"])).await.unwrap();

    // Tick 2: both B and C should be ready now (A is Done).
    let results = executor.execute_tick(&task_fn).await;
    let mut tick2_ids: Vec<String> = results.iter().map(|r| r.node_id.clone()).collect();
    tick2_ids.sort();
    assert_eq!(tick2_ids, vec!["B", "C"]);

    let g = graph.read().await;
    assert!(g.is_complete());
    assert_eq!(g.node_count(), 3);
}

/// Try to add a node that would create a cycle during execution. Verify it's
/// rejected and the existing execution can continue.
#[tokio::test]
async fn test_runtime_cycle_rejection() {
    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec!["A"])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let rt = RuntimeDag::new(graph.clone());
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = mock_executor(10);

    // Tick 1: dispatch A.
    executor.execute_tick(&task_fn).await;

    // Try to add a node that creates a cycle: A depends on B, B depends on A.
    // We'll try adding a node "X" that depends on B, and then try to make B
    // depend on X -- but simpler: add a node with dep on B that would create
    // cycle if added with wrong deps. Actually, add_node won't create a cycle
    // for a new node with forward deps. Let's try update_dependencies instead.
    let result = rt
        .update_dependencies("A", vec!["B".to_string()])
        .await;
    // A depends on B, B depends on A => cycle. But A is Done (not Pending), so
    // this should be rejected as NodeInProgress.
    assert!(result.is_err());

    // Try the cycle scenario with a pending node.
    // Add C that depends on B.
    rt.add_node(make_node("C", vec!["B"])).await.unwrap();

    // Try to make B depend on C => B->A (done) is fine, but C->B->? and B
    // depending on C would be C->B->C = cycle.
    // But B is Pending, so this should work up to cycle check.
    let result = rt
        .update_dependencies("B", vec!["A".to_string(), "C".to_string()])
        .await;
    assert!(result.is_err()); // Cycle: B -> C -> B

    // Execution should still work fine despite the rejected mutation.
    let result = executor.run(task_fn).await;
    assert!(result.is_ok());

    let g = graph.read().await;
    assert!(g.is_complete());
}

// ---------------------------------------------------------------------------
// Persistence + resume integration
// ---------------------------------------------------------------------------

/// Create a 4-node DAG (A->B->C->D). Execute A and B. Save checkpoint. Load
/// checkpoint. Verify B stays Done. Resume execution for remaining nodes.
#[tokio::test]
async fn test_save_resume_partial_dag() {
    let tmp = tempfile::TempDir::new().unwrap();
    let persistence = DagPersistence::new(tmp.path().join("checkpoint"));

    // Build the DAG: A -> B -> C -> D
    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec!["A"])).unwrap();
    graph.add_node(make_node("C", vec!["B"])).unwrap();
    graph.add_node(make_node("D", vec!["C"])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = mock_executor(10);

    // Execute tick 1: A
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, "A");

    // Execute tick 2: B
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].node_id, "B");

    // Simulate C being in-progress when we "crash"
    {
        let mut g = graph.write().await;
        g.get_node_mut("C").unwrap().status = NodeStatus::InProgress;
    }

    // Save checkpoint
    {
        let g = graph.read().await;
        persistence.save_checkpoint(&g).unwrap();
    }

    // Load checkpoint — InProgress nodes should be reset to Pending.
    let restored = persistence.load_checkpoint().unwrap();

    assert_eq!(restored.get_node("A").unwrap().status, NodeStatus::Done);
    assert_eq!(restored.get_node("B").unwrap().status, NodeStatus::Done);
    assert_eq!(restored.get_node("C").unwrap().status, NodeStatus::Pending); // was InProgress, reset
    assert_eq!(restored.get_node("D").unwrap().status, NodeStatus::Pending);

    // Resume execution from restored graph.
    let restored = Arc::new(RwLock::new(restored));
    let executor2 = DagExecutor::new(restored.clone(), 10);
    let task_fn2 = mock_executor(10);
    let result = executor2.run(task_fn2).await;
    assert!(result.is_ok());

    let g = restored.read().await;
    assert!(g.is_complete());
    assert_eq!(g.done_count(), 4);
}

/// Execute a node, save checkpoint, load checkpoint, verify NodeResult is
/// preserved.
#[tokio::test]
async fn test_persistence_with_results() {
    let tmp = tempfile::TempDir::new().unwrap();
    let persistence = DagPersistence::new(tmp.path().join("checkpoint"));

    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec!["A"])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = mock_executor(10);

    // Execute A.
    executor.execute_tick(&task_fn).await;

    // Save.
    {
        let g = graph.read().await;
        persistence.save_checkpoint(&g).unwrap();
    }

    // Load and verify result is preserved.
    let restored = persistence.load_checkpoint().unwrap();
    let node_a = restored.get_node("A").unwrap();
    assert_eq!(node_a.status, NodeStatus::Done);

    let result = node_a.result.as_ref().expect("NodeResult should be preserved");
    assert!(result.success);
    assert_eq!(result.output, "Done: A");
    assert!(result.error.is_none());
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

/// One node fails. Verify the DAG reports failure correctly and dependent nodes
/// are not dispatched.
#[tokio::test]
async fn test_dag_with_failing_node() {
    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec!["A"])).unwrap();
    graph.add_node(make_node("C", vec![])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = failing_executor(vec!["A".to_string()]);

    let result = executor.run(task_fn).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("failed"),
        "Expected error about failed nodes, got: {err_msg}"
    );

    let g = graph.read().await;
    assert!(g.has_failed());
    assert_eq!(g.get_node("A").unwrap().status, NodeStatus::Failed);

    // B should never have run (depends on failed A).
    assert_eq!(g.get_node("B").unwrap().status, NodeStatus::Pending);

    // C should have completed (independent of A).
    assert_eq!(g.get_node("C").unwrap().status, NodeStatus::Done);

    // Verify the failure result is captured.
    let a_result = g.get_node("A").unwrap().result.as_ref().unwrap();
    assert!(!a_result.success);
    assert_eq!(a_result.error.as_deref(), Some("Simulated failure"));
}

/// A node depends on a failed node — the executor should detect the stuck state
/// and return an error.
#[tokio::test]
async fn test_dag_stuck_detection() {
    let mut graph = DagGraph::new();
    graph.add_node(make_node("A", vec![])).unwrap();
    graph.add_node(make_node("B", vec!["A"])).unwrap();
    graph.add_node(make_node("C", vec!["B"])).unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = failing_executor(vec!["A".to_string()]);

    let result = executor.run(task_fn).await;
    assert!(result.is_err());

    let g = graph.read().await;
    // A failed, B and C are stuck (Pending, will never become ready).
    assert_eq!(g.get_node("A").unwrap().status, NodeStatus::Failed);
    assert_eq!(g.get_node("B").unwrap().status, NodeStatus::Pending);
    assert_eq!(g.get_node("C").unwrap().status, NodeStatus::Pending);
    assert!(!g.is_complete());
    assert!(g.has_failed());
}
