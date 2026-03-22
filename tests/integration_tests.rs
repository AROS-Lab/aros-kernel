//! End-to-end integration tests: DAG + scheduler + agents

use std::sync::Arc;
use std::time::Instant;

use aros_kernel::agent::shell::ShellAgent;
use aros_kernel::agent::types::AgentType;
use aros_kernel::dag::executor::{DagExecutor, TaskExecutor};
use aros_kernel::dag::graph::{AgentLevel, DagGraph, Node, NodeResult, NodeStatus};
use aros_kernel::dag::persistence::DagPersistence;
use aros_kernel::dag::runtime::RuntimeDag;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_shell_node(id: &str, description: &str, deps: Vec<&str>) -> Node {
    Node {
        id: id.to_string(),
        title: format!("Task {id}"),
        description: description.to_string(),
        depends_on: deps.into_iter().map(String::from).collect(),
        status: NodeStatus::Pending,
        agent_level: AgentLevel::Agent,
        output_files: vec![],
        retry_count: 0,
        result: None,
    }
}

/// Build a TaskExecutor that delegates to a ShellAgent, running each node's
/// description as a shell command.
fn shell_task_executor() -> TaskExecutor {
    let agent = Arc::new(ShellAgent::new());
    Arc::new(move |node: Node| {
        let agent = Arc::clone(&agent);
        tokio::spawn(async move {
            let start = Instant::now();
            let agent_result = agent.execute(&node.description, 30).await;
            NodeResult {
                success: agent_result.success,
                output: agent_result.output,
                error: agent_result.error,
                duration_secs: start.elapsed().as_secs_f64(),
            }
        })
    })
}

// ---------------------------------------------------------------------------
// Test 1: Full DAG with shell agents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_dag_with_shell_agents() {
    // Build DAG:
    //   A (echo step-a)  ─┐
    //                      ├─► C (echo step-c, depends on A) ─┐
    //   B (echo step-b)  ─┤                                   ├─► D (depends on B, C)
    //                      └───────────────────────────────────┘
    let mut graph = DagGraph::new();
    graph
        .add_node(make_shell_node("a", "echo step-a", vec![]))
        .unwrap();
    graph
        .add_node(make_shell_node("b", "echo step-b", vec![]))
        .unwrap();
    graph
        .add_node(make_shell_node("c", "echo step-c", vec!["a"]))
        .unwrap();
    graph
        .add_node(make_shell_node(
            "d",
            "echo step-d && sleep 0.1",
            vec!["b", "c"],
        ))
        .unwrap();

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 4);
    let task_fn = shell_task_executor();

    // ---- Record per-node completion timestamps ----
    let start = Instant::now();
    let result = executor.run(task_fn).await;
    let total_elapsed = start.elapsed();

    assert!(result.is_ok(), "DAG execution failed: {:?}", result.err());

    // Verify all 4 nodes completed successfully
    let g = graph.read().await;
    assert!(g.is_complete(), "DAG should be complete");
    assert_eq!(g.done_count(), 4);

    // Verify output content
    let node_a = g.get_node("a").unwrap();
    assert!(
        node_a.result.as_ref().unwrap().output.contains("step-a"),
        "Node A output should contain 'step-a'"
    );

    let node_b = g.get_node("b").unwrap();
    assert!(
        node_b.result.as_ref().unwrap().output.contains("step-b"),
        "Node B output should contain 'step-b'"
    );

    let node_c = g.get_node("c").unwrap();
    assert!(
        node_c.result.as_ref().unwrap().output.contains("step-c"),
        "Node C output should contain 'step-c'"
    );

    let node_d = g.get_node("d").unwrap();
    assert!(
        node_d.result.as_ref().unwrap().output.contains("step-d"),
        "Node D output should contain 'step-d'"
    );

    // D has a `sleep 0.1`, so total time should be at least 100ms
    assert!(
        total_elapsed.as_millis() >= 100,
        "Total execution should take at least 100ms due to node D's sleep"
    );

    // All nodes should report success
    for id in &["a", "b", "c", "d"] {
        let node = g.get_node(id).unwrap();
        assert_eq!(node.status, NodeStatus::Done, "Node {id} should be Done");
        assert!(
            node.result.as_ref().unwrap().success,
            "Node {id} should succeed"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: DAG with persistence (checkpoint + resume)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_dag_with_persistence() {
    let tmp = tempfile::TempDir::new().unwrap();
    let persist_path = tmp.path().join(".aros-checkpoint");

    // Build the same 4-node DAG
    let mut graph = DagGraph::new();
    graph
        .add_node(make_shell_node("a", "echo step-a", vec![]))
        .unwrap();
    graph
        .add_node(make_shell_node("b", "echo step-b", vec![]))
        .unwrap();
    graph
        .add_node(make_shell_node("c", "echo step-c", vec!["a"]))
        .unwrap();
    graph
        .add_node(make_shell_node(
            "d",
            "echo step-d && sleep 0.1",
            vec!["b", "c"],
        ))
        .unwrap();

    let graph_arc = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph_arc.clone(), 4);
    let task_fn = shell_task_executor();

    // Execute first tick: only A and B are ready (no deps)
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 2, "First tick should dispatch A and B");

    // Save checkpoint after A and B complete
    let persistence = DagPersistence::new(&persist_path);
    {
        let g = graph_arc.read().await;
        persistence.save_checkpoint(&g).unwrap();
    }

    // Load checkpoint into a fresh DagPersistence and verify state
    let persistence2 = DagPersistence::new(&persist_path);
    let loaded_graph = persistence2.load_checkpoint().unwrap();

    assert_eq!(loaded_graph.node_count(), 4);
    assert_eq!(
        loaded_graph.get_node("a").unwrap().status,
        NodeStatus::Done,
        "A should be Done after checkpoint load"
    );
    assert_eq!(
        loaded_graph.get_node("b").unwrap().status,
        NodeStatus::Done,
        "B should be Done after checkpoint load"
    );
    assert_eq!(
        loaded_graph.get_node("c").unwrap().status,
        NodeStatus::Pending,
        "C should be Pending after checkpoint load"
    );
    assert_eq!(
        loaded_graph.get_node("d").unwrap().status,
        NodeStatus::Pending,
        "D should be Pending after checkpoint load"
    );

    // Continue execution from the loaded checkpoint
    let resumed_arc = Arc::new(RwLock::new(loaded_graph));
    let executor2 = DagExecutor::new(resumed_arc.clone(), 4);
    let task_fn2 = shell_task_executor();

    let result = executor2.run(task_fn2).await;
    assert!(
        result.is_ok(),
        "Resumed DAG execution failed: {:?}",
        result.err()
    );

    let g = resumed_arc.read().await;
    assert!(g.is_complete(), "Resumed DAG should be complete");
    assert_eq!(g.done_count(), 4);

    // Verify C and D produced output
    assert!(g
        .get_node("c")
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .output
        .contains("step-c"));
    assert!(g
        .get_node("d")
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .output
        .contains("step-d"));
}

// ---------------------------------------------------------------------------
// Test 3: Runtime DAG mutation (add node while executing)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_full_dag_with_runtime_mutation() {
    // Start with A and B (independent)
    let mut graph = DagGraph::new();
    graph
        .add_node(make_shell_node("a", "echo step-a", vec![]))
        .unwrap();
    graph
        .add_node(make_shell_node("b", "echo step-b", vec![]))
        .unwrap();

    let graph_arc = Arc::new(RwLock::new(graph));
    let runtime_dag = RuntimeDag::new(graph_arc.clone());
    let executor = DagExecutor::new(graph_arc.clone(), 4);
    let task_fn = shell_task_executor();

    // Execute first tick: A and B both run
    let results = executor.execute_tick(&task_fn).await;
    assert_eq!(results.len(), 2, "First tick should dispatch A and B");

    // Verify A is done
    {
        let g = graph_arc.read().await;
        assert_eq!(g.get_node("a").unwrap().status, NodeStatus::Done);
        assert_eq!(g.get_node("b").unwrap().status, NodeStatus::Done);
    }

    // Now add node C that depends on A, via RuntimeDag
    runtime_dag
        .add_node(make_shell_node("c", "echo step-c-runtime", vec!["a"]))
        .await
        .unwrap();

    // Verify stats
    let stats = runtime_dag.stats().await;
    assert_eq!(stats.total, 3);
    assert_eq!(stats.done, 2);
    assert_eq!(stats.pending, 1);

    // Continue execution: C should now be ready (A is Done)
    let task_fn2 = shell_task_executor();
    let result = executor.run(task_fn2).await;
    assert!(
        result.is_ok(),
        "Execution after runtime mutation failed: {:?}",
        result.err()
    );

    let g = graph_arc.read().await;
    assert!(g.is_complete());
    assert_eq!(g.done_count(), 3);
    assert!(g
        .get_node("c")
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .output
        .contains("step-c-runtime"));
}

// ---------------------------------------------------------------------------
// Test 4: Scheduler admission + DAG integration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_admission_controller_with_shell_agent() {
    use aros_kernel::scheduler::admission::{AdmissionController, MemoryPressureLevel};
    use aros_kernel::scheduler::allocator::ResourceAllocator;

    let controller = AdmissionController::new(3);
    let allocator = ResourceAllocator::new();
    let agent = ShellAgent::new();
    let req = agent.resource_requirements();

    // Should be able to schedule with plenty of resources
    assert!(controller.can_schedule(&req, &allocator, MemoryPressureLevel::Normal, 4000, 8000));

    // Allocate until at limit
    for _ in 0..3 {
        allocator.allocate(&req);
    }
    assert!(!controller.can_schedule(&req, &allocator, MemoryPressureLevel::Normal, 4000, 8000));

    // Release one and verify scheduling works again
    allocator.release(&req);
    assert!(controller.can_schedule(&req, &allocator, MemoryPressureLevel::Normal, 4000, 8000));

    // Critical pressure blocks regardless
    assert!(!controller.can_schedule(
        &req,
        &allocator,
        MemoryPressureLevel::Critical,
        4000,
        8000
    ));
}
