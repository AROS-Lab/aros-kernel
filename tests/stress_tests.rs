use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aros_kernel::dag::executor::{DagExecutor, TaskExecutor};
use aros_kernel::dag::graph::{AgentLevel, DagGraph, Node, NodeResult, NodeStatus};
use aros_kernel::dag::persistence::DagPersistence;
use aros_kernel::scheduler::allocator::ResourceAllocator;
use aros_kernel::scheduler::admission::ResourceRequirements;
use aros_kernel::agent::shell::ShellAgent;
use aros_kernel::agent::types::AgentType;

use tempfile::TempDir;
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

// ---------------------------------------------------------------------------
// Stress Tests
// ---------------------------------------------------------------------------

/// 1000-node DAG: 10 root nodes, each with 99 children in a chain.
/// Verify node_count, topological_sort validity, and ready_nodes.
#[test]
fn test_large_dag_1000_nodes() {
    let mut graph = DagGraph::new();

    for root in 0..10 {
        let root_id = format!("root-{root}");
        graph.add_node(make_node(&root_id, vec![])).unwrap();

        let mut prev_id = root_id;
        for child in 1..100 {
            let child_id = format!("root-{root}-child-{child}");
            graph
                .add_node(make_node(&child_id, vec![&prev_id]))
                .unwrap();
            prev_id = child_id;
        }
    }

    // Verify total node count
    assert_eq!(graph.node_count(), 1000);

    // Verify topological sort
    let sorted = graph.topological_sort().unwrap();
    assert_eq!(sorted.len(), 1000);

    // No duplicates
    let unique: HashSet<&String> = sorted.iter().collect();
    assert_eq!(unique.len(), 1000);

    // Verify ready_nodes returns exactly 10 root nodes
    let ready = graph.ready_nodes();
    assert_eq!(ready.len(), 10);
    for node in &ready {
        assert!(node.id.starts_with("root-"));
        assert!(!node.id.contains("-child-"));
    }
}

/// 100-node linear chain: node-0 -> node-1 -> ... -> node-99.
/// Execute with mock executor (1ms delay). Verify all complete in order.
#[tokio::test]
async fn test_deep_dependency_chain() {
    let mut graph = DagGraph::new();

    graph.add_node(make_node("node-0", vec![])).unwrap();
    for i in 1..100 {
        let prev = format!("node-{}", i - 1);
        let current = format!("node-{i}");
        graph.add_node(make_node(&current, vec![&prev])).unwrap();
    }

    assert_eq!(graph.node_count(), 100);

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 10);
    let task_fn = mock_executor(1);

    let result = executor.run(task_fn).await;
    assert!(result.is_ok(), "DAG execution failed: {:?}", result.err());

    let g = graph.read().await;
    assert!(g.is_complete());
    assert_eq!(g.done_count(), 100);

    // Verify ordering via topological sort: each node's result exists
    for i in 0..100 {
        let id = format!("node-{i}");
        let node = g.get_node(&id).unwrap();
        assert_eq!(node.status, NodeStatus::Done);
        assert!(node.result.is_some());
    }
}

/// 50 independent nodes, max_parallel=20, 10ms delay each.
/// Verify all complete and total time proves parallelism (< 500ms).
#[tokio::test]
async fn test_high_parallelism_executor() {
    let mut graph = DagGraph::new();

    for i in 0..50 {
        let id = format!("parallel-{i}");
        graph.add_node(make_node(&id, vec![])).unwrap();
    }

    let graph = Arc::new(RwLock::new(graph));
    let executor = DagExecutor::new(graph.clone(), 20);
    let task_fn = mock_executor(10);

    let start = Instant::now();
    let result = executor.run(task_fn).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "DAG execution failed: {:?}", result.err());

    let g = graph.read().await;
    assert!(g.is_complete());
    assert_eq!(g.done_count(), 50);

    // With 50 nodes, max_parallel=20, 10ms each:
    // ceil(50/20) = 3 ticks * 10ms = 30ms + tick intervals.
    // Should be well under 500ms (proves parallelism vs sequential 500ms).
    assert!(
        elapsed < Duration::from_millis(500),
        "Execution took {:?}, expected < 500ms (parallelism not working?)",
        elapsed
    );
}

/// Allocate ResourceRequirements::shell() 1000 times, verify totals.
/// Release all 1000, verify all back to 0.
#[test]
fn test_stress_allocator_1000() {
    let allocator = ResourceAllocator::new();
    let req = ResourceRequirements::shell();

    for _ in 0..1000 {
        allocator.allocate(&req);
    }

    assert_eq!(allocator.allocated_cpu(), 200_000);
    assert_eq!(allocator.active_agents(), 1000);

    for _ in 0..1000 {
        allocator.release(&req);
    }

    assert_eq!(allocator.allocated_cpu(), 0);
    assert_eq!(allocator.active_agents(), 0);
}

/// Create a 500-node DAG with mixed statuses, save checkpoint, load it back,
/// verify all 500 nodes restored with correct statuses.
#[test]
fn test_large_dag_persistence() {
    let tmp = TempDir::new().unwrap();
    let persistence = DagPersistence::new(tmp.path().join(".aros"));

    let mut graph = DagGraph::new();

    // Build 500 nodes: 50 roots, each with 9 children in a chain
    for root in 0..50 {
        let root_id = format!("persist-{root}");
        graph.add_node(make_node(&root_id, vec![])).unwrap();

        let mut prev_id = root_id.clone();
        for child in 1..10 {
            let child_id = format!("persist-{root}-child-{child}");
            graph
                .add_node(make_node(&child_id, vec![&prev_id]))
                .unwrap();
            prev_id = child_id;
        }
    }

    assert_eq!(graph.node_count(), 500);

    // Mark some nodes as Done, some as InProgress (to test crash recovery)
    for root in 0..50 {
        let root_id = format!("persist-{root}");
        graph.get_node_mut(&root_id).unwrap().status = NodeStatus::Done;

        // Mark first child as Done, second as InProgress
        let child1_id = format!("persist-{root}-child-1");
        graph.get_node_mut(&child1_id).unwrap().status = NodeStatus::Done;

        let child2_id = format!("persist-{root}-child-2");
        graph.get_node_mut(&child2_id).unwrap().status = NodeStatus::InProgress;
    }

    // Save and load
    persistence.save_checkpoint(&graph).unwrap();
    let loaded = persistence.load_checkpoint().unwrap();

    // Verify all 500 nodes restored
    assert_eq!(loaded.node_count(), 500);

    // Verify statuses
    for root in 0..50 {
        let root_id = format!("persist-{root}");
        assert_eq!(
            loaded.get_node(&root_id).unwrap().status,
            NodeStatus::Done,
            "Root node {root_id} should be Done"
        );

        let child1_id = format!("persist-{root}-child-1");
        assert_eq!(
            loaded.get_node(&child1_id).unwrap().status,
            NodeStatus::Done,
            "Child 1 of root {root} should be Done"
        );

        // InProgress nodes should be reset to Pending (crash recovery)
        let child2_id = format!("persist-{root}-child-2");
        assert_eq!(
            loaded.get_node(&child2_id).unwrap().status,
            NodeStatus::Pending,
            "Child 2 of root {root} should be reset to Pending"
        );

        // Remaining children should still be Pending
        for child in 3..10 {
            let child_id = format!("persist-{root}-child-{child}");
            assert_eq!(
                loaded.get_node(&child_id).unwrap().status,
                NodeStatus::Pending,
                "Child {child} of root {root} should be Pending"
            );
        }
    }
}

/// Run ShellAgent with 'seq 1 10000', verify output contains "10000"
/// and has ~50000+ chars.
#[tokio::test]
async fn test_shell_large_output() {
    let agent = ShellAgent::new();
    let result = agent.execute("seq 1 10000", 30).await;

    assert!(result.success, "Shell command failed: {:?}", result.error);
    assert!(
        result.output.contains("10000"),
        "Output should contain '10000'"
    );
    assert!(
        result.output.len() >= 48000,
        "Output length {} should be >= 48000 chars",
        result.output.len()
    );
}
