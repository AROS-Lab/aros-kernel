use std::path::PathBuf;

use super::graph::{DagGraph, Node, NodeStatus};

/// Errors that can occur during DAG persistence operations.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("No checkpoint found at {0}")]
    NoCheckpoint(PathBuf),
}

/// Save/load DAG state to/from files for resume support.
pub struct DagPersistence {
    base_path: PathBuf,
}

impl DagPersistence {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }

    /// Save full DAG state to files.
    ///
    /// Creates:
    ///   {base_path}/dag.json         — full graph serialized
    ///   {base_path}/state/node-{id}.json — per-node state for each node
    pub fn save_checkpoint(&self, graph: &DagGraph) -> Result<(), PersistenceError> {
        // Ensure directories exist.
        let state_dir = self.base_path.join("state");
        std::fs::create_dir_all(&state_dir)?;

        // Write full graph.
        let json = serde_json::to_string_pretty(graph)?;
        std::fs::write(self.dag_json_path(), json)?;

        // Write individual node state files.
        for node in graph.nodes().values() {
            self.save_node_state(node)?;
        }

        Ok(())
    }

    /// Load DAG state from files.
    ///
    /// Reads {base_path}/dag.json and restores the DagGraph.
    /// Any nodes with status `InProgress` are reset to `Pending` (crash recovery).
    pub fn load_checkpoint(&self) -> Result<DagGraph, PersistenceError> {
        let path = self.dag_json_path();
        if !path.exists() {
            return Err(PersistenceError::NoCheckpoint(self.base_path.clone()));
        }

        let json = std::fs::read_to_string(&path)?;
        let mut graph: DagGraph = serde_json::from_str(&json)?;

        // Reset any InProgress nodes to Pending for crash recovery.
        let ids: Vec<String> = graph
            .nodes()
            .iter()
            .filter(|(_, n)| n.status == NodeStatus::InProgress)
            .map(|(id, _)| id.clone())
            .collect();

        for id in ids {
            if let Some(node) = graph.get_node_mut(&id) {
                node.status = NodeStatus::Pending;
            }
        }

        Ok(graph)
    }

    /// Save a single node's state (for incremental saves after completion).
    pub fn save_node_state(&self, node: &Node) -> Result<(), PersistenceError> {
        let state_dir = self.base_path.join("state");
        std::fs::create_dir_all(&state_dir)?;

        let path = state_dir.join(format!("node-{}.json", node.id));
        let json = serde_json::to_string_pretty(node)?;
        std::fs::write(path, json)?;

        Ok(())
    }

    /// Check if a checkpoint exists at the base path.
    pub fn has_checkpoint(&self) -> bool {
        self.dag_json_path().exists()
    }

    /// Delete the checkpoint directory.
    pub fn clear(&self) -> Result<(), PersistenceError> {
        if self.base_path.exists() {
            std::fs::remove_dir_all(&self.base_path)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn dag_json_path(&self) -> PathBuf {
        self.base_path.join("dag.json")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::graph::{AgentLevel, DagGraph, Node, NodeStatus};
    use tempfile::TempDir;

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

    fn make_persistence(dir: &TempDir) -> DagPersistence {
        DagPersistence::new(dir.path().join(".aros"))
    }

    fn make_sample_graph() -> DagGraph {
        let mut g = DagGraph::new();
        g.add_node(make_node("eng-1", vec![])).unwrap();
        g.add_node(make_node("eng-2", vec!["eng-1"])).unwrap();
        g.add_node(make_node("eng-3", vec!["eng-1"])).unwrap();
        g
    }

    #[test]
    fn test_save_and_load_checkpoint() {
        let tmp = TempDir::new().unwrap();
        let p = make_persistence(&tmp);
        let graph = make_sample_graph();

        p.save_checkpoint(&graph).unwrap();
        let loaded = p.load_checkpoint().unwrap();

        assert_eq!(loaded.node_count(), 3);
        assert!(loaded.get_node("eng-1").is_some());
        assert!(loaded.get_node("eng-2").is_some());
        assert!(loaded.get_node("eng-3").is_some());
        assert_eq!(
            loaded.get_node("eng-2").unwrap().depends_on,
            vec!["eng-1".to_string()]
        );
    }

    #[test]
    fn test_resume_resets_in_progress() {
        let tmp = TempDir::new().unwrap();
        let p = make_persistence(&tmp);
        let mut graph = make_sample_graph();

        // Mark some nodes as InProgress (simulating a crash mid-execution).
        graph.get_node_mut("eng-1").unwrap().status = NodeStatus::Done;
        graph.get_node_mut("eng-2").unwrap().status = NodeStatus::InProgress;
        graph.get_node_mut("eng-3").unwrap().status = NodeStatus::InProgress;

        p.save_checkpoint(&graph).unwrap();
        let loaded = p.load_checkpoint().unwrap();

        // Done nodes stay Done.
        assert_eq!(loaded.get_node("eng-1").unwrap().status, NodeStatus::Done);
        // InProgress nodes are reset to Pending.
        assert_eq!(
            loaded.get_node("eng-2").unwrap().status,
            NodeStatus::Pending
        );
        assert_eq!(
            loaded.get_node("eng-3").unwrap().status,
            NodeStatus::Pending
        );
    }

    #[test]
    fn test_save_node_state() {
        let tmp = TempDir::new().unwrap();
        let p = make_persistence(&tmp);
        let node = make_node("eng-1", vec![]);

        p.save_node_state(&node).unwrap();

        let path = tmp.path().join(".aros/state/node-eng-1.json");
        assert!(path.exists());

        let json = std::fs::read_to_string(&path).unwrap();
        let loaded: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.id, "eng-1");
        assert_eq!(loaded.status, NodeStatus::Pending);
    }

    #[test]
    fn test_has_checkpoint() {
        let tmp = TempDir::new().unwrap();
        let p = make_persistence(&tmp);

        assert!(!p.has_checkpoint());

        let graph = make_sample_graph();
        p.save_checkpoint(&graph).unwrap();

        assert!(p.has_checkpoint());
    }

    #[test]
    fn test_clear_checkpoint() {
        let tmp = TempDir::new().unwrap();
        let p = make_persistence(&tmp);
        let graph = make_sample_graph();

        p.save_checkpoint(&graph).unwrap();
        assert!(p.has_checkpoint());

        p.clear().unwrap();
        assert!(!p.has_checkpoint());
    }

    #[test]
    fn test_load_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let p = make_persistence(&tmp);

        let result = p.load_checkpoint();
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            PersistenceError::NoCheckpoint(_)
        ));
    }
}
