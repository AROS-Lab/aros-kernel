use std::sync::Arc;
use tokio::sync::RwLock;

use super::graph::{DagGraph, Node, NodeResult, NodeStatus};

/// Result of executing a single node.
pub struct ExecutionResult {
    pub node_id: String,
    pub result: NodeResult,
}

/// Function type for executing a node's task.
pub type TaskExecutor = Arc<dyn Fn(Node) -> tokio::task::JoinHandle<NodeResult> + Send + Sync>;

/// DAG Executor — walks graph, dispatches ready nodes in parallel, collects
/// results.
pub struct DagExecutor {
    graph: Arc<RwLock<DagGraph>>,
    max_parallel: usize,
    tick_interval_ms: u64,
}

impl DagExecutor {
    pub fn new(graph: Arc<RwLock<DagGraph>>, max_parallel: usize) -> Self {
        Self {
            graph,
            max_parallel,
            tick_interval_ms: 50,
        }
    }

    /// Execute one tick: find ready nodes, dispatch up to `max_parallel`,
    /// wait for completion, update statuses.
    pub async fn execute_tick(&self, task_fn: &TaskExecutor) -> Vec<ExecutionResult> {
        // 1. Lock graph, find ready nodes, take up to max_parallel
        let mut nodes_to_run: Vec<Node> = Vec::new();
        {
            let mut graph = self.graph.write().await;
            let ready_ids: Vec<String> = graph
                .ready_nodes()
                .into_iter()
                .take(self.max_parallel)
                .map(|n| n.id.clone())
                .collect();

            for id in &ready_ids {
                if let Some(node) = graph.get_node_mut(id) {
                    node.status = NodeStatus::InProgress;
                    nodes_to_run.push(node.clone());
                }
            }
        }

        if nodes_to_run.is_empty() {
            return Vec::new();
        }

        // 2. Spawn each via task_fn, collect JoinHandles
        let handles: Vec<(String, tokio::task::JoinHandle<NodeResult>)> = nodes_to_run
            .into_iter()
            .map(|node| {
                let id = node.id.clone();
                let handle = task_fn(node);
                (id, handle)
            })
            .collect();

        // 3. Wait for all to complete
        let mut results = Vec::new();
        for (node_id, handle) in handles {
            match handle.await {
                Ok(node_result) => {
                    results.push(ExecutionResult {
                        node_id,
                        result: node_result,
                    });
                }
                Err(e) => {
                    results.push(ExecutionResult {
                        node_id,
                        result: NodeResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Task panicked: {e}")),
                            duration_secs: 0.0,
                        },
                    });
                }
            }
        }

        // 4. Lock graph, update statuses based on results
        {
            let mut graph = self.graph.write().await;
            for er in &results {
                if let Some(node) = graph.get_node_mut(&er.node_id) {
                    node.status = if er.result.success {
                        NodeStatus::Done
                    } else {
                        NodeStatus::Failed
                    };
                    node.result = Some(er.result.clone());
                }
            }
        }

        results
    }

    /// Run the executor loop until the DAG is complete or no progress can be
    /// made.
    pub async fn run(&self, task_fn: TaskExecutor) -> Result<(), String> {
        loop {
            let results = self.execute_tick(&task_fn).await;

            let graph = self.graph.read().await;
            if graph.is_complete() {
                return Ok(());
            }
            if results.is_empty() && graph.ready_nodes().is_empty() {
                if graph.has_failed() {
                    return Err("DAG has failed nodes with no recovery path".into());
                }
                return Err("DAG is stuck: no ready nodes and not complete".into());
            }
            drop(graph);

            tokio::time::sleep(tokio::time::Duration::from_millis(self.tick_interval_ms)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::graph::{AgentLevel, Node, NodeResult, NodeStatus};
    use std::time::Duration;

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
                    output: format!("Completed {}", node.id),
                    error: None,
                    duration_secs: delay_ms as f64 / 1000.0,
                }
            })
        })
    }

    fn failing_executor(fail_id: String) -> TaskExecutor {
        Arc::new(move |node: Node| {
            let should_fail = node.id == fail_id;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if should_fail {
                    NodeResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Node {} failed", node.id)),
                        duration_secs: 0.01,
                    }
                } else {
                    NodeResult {
                        success: true,
                        output: format!("Completed {}", node.id),
                        error: None,
                        duration_secs: 0.01,
                    }
                }
            })
        })
    }

    #[tokio::test]
    async fn test_execute_tick_dispatches_ready() {
        let mut graph = DagGraph::new();
        graph.add_node(make_node("a", vec![])).unwrap();
        graph.add_node(make_node("b", vec![])).unwrap();

        let graph = Arc::new(RwLock::new(graph));
        let executor = DagExecutor::new(graph.clone(), 4);
        let task_fn = mock_executor(10);

        let results = executor.execute_tick(&task_fn).await;
        assert_eq!(results.len(), 2);

        let g = graph.read().await;
        assert_eq!(g.get_node("a").unwrap().status, NodeStatus::Done);
        assert_eq!(g.get_node("b").unwrap().status, NodeStatus::Done);
    }

    #[tokio::test]
    async fn test_execute_tick_respects_max_parallel() {
        let mut graph = DagGraph::new();
        graph.add_node(make_node("a", vec![])).unwrap();
        graph.add_node(make_node("b", vec![])).unwrap();
        graph.add_node(make_node("c", vec![])).unwrap();

        let graph = Arc::new(RwLock::new(graph));
        let executor = DagExecutor::new(graph.clone(), 1);
        let task_fn = mock_executor(10);

        let results = executor.execute_tick(&task_fn).await;
        assert_eq!(results.len(), 1);

        // Only one node should be Done
        let g = graph.read().await;
        let done_count = g.done_count();
        assert_eq!(done_count, 1);
    }

    #[tokio::test]
    async fn test_execute_tick_respects_dependencies() {
        let mut graph = DagGraph::new();
        graph.add_node(make_node("a", vec![])).unwrap();
        graph.add_node(make_node("b", vec!["a"])).unwrap();

        let graph = Arc::new(RwLock::new(graph));
        let executor = DagExecutor::new(graph.clone(), 4);
        let task_fn = mock_executor(10);

        // First tick: only "a" is ready
        let results = executor.execute_tick(&task_fn).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "a");

        // Second tick: now "b" is ready
        let results = executor.execute_tick(&task_fn).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "b");
    }

    #[tokio::test]
    async fn test_run_completes_dag() {
        let mut graph = DagGraph::new();
        graph.add_node(make_node("a", vec![])).unwrap();
        graph.add_node(make_node("b", vec!["a"])).unwrap();
        graph.add_node(make_node("c", vec!["a"])).unwrap();
        graph.add_node(make_node("d", vec!["b", "c"])).unwrap();

        let graph = Arc::new(RwLock::new(graph));
        let executor = DagExecutor::new(graph.clone(), 4);
        let task_fn = mock_executor(10);

        let result = executor.run(task_fn).await;
        assert!(result.is_ok());

        let g = graph.read().await;
        assert!(g.is_complete());
        assert_eq!(g.done_count(), 4);
    }

    #[tokio::test]
    async fn test_run_detects_failure() {
        let mut graph = DagGraph::new();
        graph.add_node(make_node("a", vec![])).unwrap();
        graph.add_node(make_node("b", vec!["a"])).unwrap();

        let graph = Arc::new(RwLock::new(graph));
        let executor = DagExecutor::new(graph.clone(), 4);
        let task_fn = failing_executor("a".to_string());

        let result = executor.run(task_fn).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("failed"));
    }
}
