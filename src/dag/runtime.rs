use std::sync::Arc;
use tokio::sync::RwLock;

use super::graph::{DagError, DagGraph, Node, NodeStatus};

/// RuntimeDag provides safe runtime mutation of a DAG while an executor may be
/// running. It wraps the same `Arc<RwLock<DagGraph>>` that the executor uses.
pub struct RuntimeDag {
    graph: Arc<RwLock<DagGraph>>,
}

#[derive(Debug, Clone)]
pub struct DagStats {
    pub total: usize,
    pub pending: usize,
    pub in_progress: usize,
    pub done: usize,
    pub failed: usize,
}

impl RuntimeDag {
    pub fn new(graph: Arc<RwLock<DagGraph>>) -> Self {
        Self { graph }
    }

    /// Get a clone of the Arc for sharing with the executor.
    pub fn graph_ref(&self) -> Arc<RwLock<DagGraph>> {
        Arc::clone(&self.graph)
    }

    /// Add a node at runtime. Validates no cycles via DagGraph::add_node.
    pub async fn add_node(&self, node: Node) -> Result<(), DagError> {
        let mut graph = self.graph.write().await;
        graph.add_node(node)
    }

    /// Remove a node at runtime. Only if status is Pending.
    /// Returns error if node is InProgress or if other nodes depend on it.
    pub async fn remove_node(&self, id: &str) -> Result<Node, DagError> {
        let mut graph = self.graph.write().await;

        // Check node exists and is Pending.
        match graph.get_node(id) {
            None => return Err(DagError::NodeNotFound(id.to_string())),
            Some(node) => {
                if node.status == NodeStatus::InProgress {
                    return Err(DagError::NodeInProgress(id.to_string()));
                }
                if node.status != NodeStatus::Pending {
                    return Err(DagError::NodeInProgress(id.to_string()));
                }
            }
        }

        graph.remove_node(id)
    }

    /// Update dependencies of an existing node.
    /// Only allowed if node is Pending. Validates no cycles after update.
    pub async fn update_dependencies(
        &self,
        node_id: &str,
        new_deps: Vec<String>,
    ) -> Result<(), DagError> {
        let mut graph = self.graph.write().await;

        // Verify node exists and is Pending.
        let node = graph
            .get_node(node_id)
            .ok_or_else(|| DagError::NodeNotFound(node_id.to_string()))?;

        if node.status != NodeStatus::Pending {
            return Err(DagError::NodeInProgress(node_id.to_string()));
        }

        // Save old deps for rollback.
        let old_deps = node.depends_on.clone();

        // Apply new deps.
        graph.get_node_mut(node_id).unwrap().depends_on = new_deps;

        // Check for cycles.
        if graph.has_cycle() {
            // Revert.
            graph.get_node_mut(node_id).unwrap().depends_on = old_deps;
            return Err(DagError::CycleDetected(node_id.to_string()));
        }

        Ok(())
    }

    /// Get current DAG stats.
    pub async fn stats(&self) -> DagStats {
        let graph = self.graph.read().await;
        let mut stats = DagStats {
            total: 0,
            pending: 0,
            in_progress: 0,
            done: 0,
            failed: 0,
        };

        for node in graph.nodes().values() {
            stats.total += 1;
            match node.status {
                NodeStatus::Pending | NodeStatus::Blocked => stats.pending += 1,
                NodeStatus::InProgress => stats.in_progress += 1,
                NodeStatus::Done => stats.done += 1,
                NodeStatus::Failed => stats.failed += 1,
            }
        }

        stats
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::graph::{AgentLevel, Node, NodeStatus};

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

    fn make_graph_arc() -> Arc<RwLock<DagGraph>> {
        Arc::new(RwLock::new(DagGraph::new()))
    }

    #[tokio::test]
    async fn test_add_node_runtime() {
        let graph = make_graph_arc();
        {
            let mut g = graph.write().await;
            g.add_node(make_node("a", vec![])).unwrap();
            g.add_node(make_node("b", vec!["a"])).unwrap();
        }

        let rt = RuntimeDag::new(graph.clone());
        // Add a new node with no deps — should appear in ready_nodes.
        rt.add_node(make_node("c", vec![])).await.unwrap();

        let g = graph.read().await;
        assert_eq!(g.node_count(), 3);
        let ready_ids: Vec<String> = g.ready_nodes().iter().map(|n| n.id.clone()).collect();
        assert!(ready_ids.contains(&"c".to_string()));
    }

    #[tokio::test]
    async fn test_remove_pending_node() {
        let graph = make_graph_arc();
        {
            let mut g = graph.write().await;
            g.add_node(make_node("a", vec![])).unwrap();
            g.add_node(make_node("b", vec![])).unwrap();
        }

        let rt = RuntimeDag::new(graph.clone());
        let removed = rt.remove_node("b").await.unwrap();
        assert_eq!(removed.id, "b");

        let g = graph.read().await;
        assert_eq!(g.node_count(), 1);
        assert!(g.get_node("b").is_none());
    }

    #[tokio::test]
    async fn test_remove_in_progress_fails() {
        let graph = make_graph_arc();
        {
            let mut g = graph.write().await;
            g.add_node(make_node("a", vec![])).unwrap();
            g.get_node_mut("a").unwrap().status = NodeStatus::InProgress;
        }

        let rt = RuntimeDag::new(graph.clone());
        let result = rt.remove_node("a").await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DagError::NodeInProgress(_)
        ));
    }

    #[tokio::test]
    async fn test_update_dependencies() {
        let graph = make_graph_arc();
        {
            let mut g = graph.write().await;
            g.add_node(make_node("a", vec![])).unwrap();
            g.add_node(make_node("b", vec![])).unwrap();
            g.add_node(make_node("c", vec!["a"])).unwrap();
        }

        let rt = RuntimeDag::new(graph.clone());

        // Change c's dependency from a to b.
        rt.update_dependencies("c", vec!["b".to_string()])
            .await
            .unwrap();

        let g = graph.read().await;
        let node_c = g.get_node("c").unwrap();
        assert_eq!(node_c.depends_on, vec!["b".to_string()]);

        // Mark b as Done — c should now be ready.
        drop(g);
        {
            let mut g = graph.write().await;
            g.get_node_mut("b").unwrap().status = NodeStatus::Done;
        }
        let g = graph.read().await;
        let ready_ids: Vec<String> = g.ready_nodes().iter().map(|n| n.id.clone()).collect();
        assert!(ready_ids.contains(&"c".to_string()));
    }

    #[tokio::test]
    async fn test_update_dependencies_cycle_fails() {
        let graph = make_graph_arc();
        {
            let mut g = graph.write().await;
            g.add_node(make_node("a", vec![])).unwrap();
            g.add_node(make_node("b", vec!["a"])).unwrap();
        }

        let rt = RuntimeDag::new(graph.clone());

        // Try to make a depend on b — would create a->b->a cycle.
        let result = rt
            .update_dependencies("a", vec!["b".to_string()])
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DagError::CycleDetected(_)));

        // Verify deps were reverted.
        let g = graph.read().await;
        let node_a = g.get_node("a").unwrap();
        assert!(node_a.depends_on.is_empty());
    }

    #[tokio::test]
    async fn test_stats() {
        let graph = make_graph_arc();
        {
            let mut g = graph.write().await;
            g.add_node(make_node("a", vec![])).unwrap();
            g.add_node(make_node("b", vec![])).unwrap();
            g.add_node(make_node("c", vec![])).unwrap();
            g.add_node(make_node("d", vec![])).unwrap();
            g.add_node(make_node("e", vec![])).unwrap();

            g.get_node_mut("a").unwrap().status = NodeStatus::Done;
            g.get_node_mut("b").unwrap().status = NodeStatus::InProgress;
            g.get_node_mut("c").unwrap().status = NodeStatus::Failed;
            // d and e remain Pending
        }

        let rt = RuntimeDag::new(graph.clone());
        let stats = rt.stats().await;
        assert_eq!(stats.total, 5);
        assert_eq!(stats.done, 1);
        assert_eq!(stats.in_progress, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.pending, 2);
    }
}
