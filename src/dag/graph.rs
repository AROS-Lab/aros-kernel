use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DagError {
    #[error("Cycle detected: adding node '{0}' would create a cycle")]
    CycleDetected(String),
    #[error("Node not found: '{0}'")]
    NodeNotFound(String),
    #[error("Cannot remove node '{0}': other nodes depend on it")]
    HasDependents(String),
    #[error("Duplicate node id: '{0}'")]
    DuplicateNode(String),
    #[error("Cannot modify node '{0}': it is currently in progress")]
    NodeInProgress(String),
}

// ---------------------------------------------------------------------------
// Domain types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeStatus {
    Pending,
    InProgress,
    Done,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentLevel {
    /// Level 0
    Session,
    /// Level 1
    Agent,
    /// Level 2
    SubAgent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub status: NodeStatus,
    pub agent_level: AgentLevel,
    pub output_files: Vec<String>,
    pub retry_count: u32,
    pub result: Option<NodeResult>,
}

// ---------------------------------------------------------------------------
// DAG Graph
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagGraph {
    nodes: HashMap<String, Node>,
}

impl DagGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Add a node. Returns an error if the id is duplicate or if the addition
    /// would create a cycle.
    pub fn add_node(&mut self, node: Node) -> Result<(), DagError> {
        if self.nodes.contains_key(&node.id) {
            return Err(DagError::DuplicateNode(node.id.clone()));
        }

        // Tentatively insert, then check for cycles.
        let id = node.id.clone();
        self.nodes.insert(id.clone(), node);

        if self.has_cycle() {
            self.nodes.remove(&id);
            return Err(DagError::CycleDetected(id));
        }

        Ok(())
    }

    /// Remove a node. Errors if it doesn't exist or if other nodes depend on it.
    pub fn remove_node(&mut self, id: &str) -> Result<Node, DagError> {
        if !self.nodes.contains_key(id) {
            return Err(DagError::NodeNotFound(id.to_string()));
        }

        // Check if any other node depends on this one.
        for (other_id, other_node) in &self.nodes {
            if other_id != id && other_node.depends_on.contains(&id.to_string()) {
                return Err(DagError::HasDependents(id.to_string()));
            }
        }

        Ok(self.nodes.remove(id).unwrap())
    }

    /// Return a reference to the internal nodes map.
    pub fn nodes(&self) -> &HashMap<String, Node> {
        &self.nodes
    }

    pub fn get_node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.get_mut(id)
    }

    /// Return nodes that are `Pending` and whose every dependency is `Done`.
    pub fn ready_nodes(&self) -> Vec<&Node> {
        self.nodes
            .values()
            .filter(|n| {
                n.status == NodeStatus::Pending
                    && n.depends_on.iter().all(|dep_id| {
                        self.nodes
                            .get(dep_id)
                            .is_some_and(|dep| dep.status == NodeStatus::Done)
                    })
            })
            .collect()
    }

    /// True when every node is `Done`.
    pub fn is_complete(&self) -> bool {
        !self.nodes.is_empty() && self.nodes.values().all(|n| n.status == NodeStatus::Done)
    }

    /// True if any node is `Failed`.
    pub fn has_failed(&self) -> bool {
        self.nodes.values().any(|n| n.status == NodeStatus::Failed)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn done_count(&self) -> usize {
        self.nodes
            .values()
            .filter(|n| n.status == NodeStatus::Done)
            .count()
    }

    /// Kahn's algorithm topological sort. Returns an error if the graph has a
    /// cycle.
    pub fn topological_sort(&self) -> Result<Vec<String>, DagError> {
        // Build in-degree map: count how many in-graph dependencies each node has.
        let mut in_degree: HashMap<&str, usize> = self
            .nodes
            .keys()
            .map(|id| (id.as_str(), 0usize))
            .collect();

        for node in self.nodes.values() {
            for dep_id in &node.depends_on {
                if self.nodes.contains_key(dep_id) {
                    *in_degree.get_mut(node.id.as_str()).unwrap() += 1;
                }
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|&(_, deg)| *deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted: Vec<String> = Vec::new();

        while let Some(id) = queue.pop_front() {
            sorted.push(id.to_string());

            // For every node that depends on `id`, reduce its in-degree.
            for node in self.nodes.values() {
                if node.depends_on.contains(&id.to_string()) {
                    let deg = in_degree.get_mut(node.id.as_str()).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(node.id.as_str());
                    }
                }
            }
        }

        if sorted.len() != self.nodes.len() {
            let stuck = self
                .nodes
                .keys()
                .find(|id| !sorted.contains(id))
                .unwrap();
            return Err(DagError::CycleDetected(stuck.clone()));
        }

        Ok(sorted)
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// DFS-based cycle detection across the entire graph.
    pub(crate) fn has_cycle(&self) -> bool {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut stack: HashSet<&str> = HashSet::new();

        for id in self.nodes.keys() {
            if !visited.contains(id.as_str()) && self.dfs_cycle(id, &mut visited, &mut stack) {
                return true;
            }
        }
        false
    }

    fn dfs_cycle<'a>(
        &'a self,
        id: &'a str,
        visited: &mut HashSet<&'a str>,
        stack: &mut HashSet<&'a str>,
    ) -> bool {
        visited.insert(id);
        stack.insert(id);

        if let Some(node) = self.nodes.get(id) {
            for dep_id in &node.depends_on {
                if self.nodes.contains_key(dep_id.as_str()) {
                    if !visited.contains(dep_id.as_str()) {
                        if self.dfs_cycle(dep_id, visited, stack) {
                            return true;
                        }
                    } else if stack.contains(dep_id.as_str()) {
                        return true;
                    }
                }
            }
        }

        stack.remove(id);
        false
    }
}

impl Default for DagGraph {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_add_and_retrieve_node() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        let n = g.get_node("a").unwrap();
        assert_eq!(n.id, "a");
        assert_eq!(n.status, NodeStatus::Pending);
    }

    #[test]
    fn test_ready_nodes_no_deps() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        let ready = g.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
    }

    #[test]
    fn test_ready_nodes_with_deps() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g.add_node(make_node("b", vec!["a"])).unwrap();

        // b should not be ready because a is still Pending.
        let ready = g.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");

        // Mark a as Done.
        g.get_node_mut("a").unwrap().status = NodeStatus::Done;
        let ready = g.ready_nodes();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "b");
    }

    #[test]
    fn test_cycle_detection() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec!["b"])).unwrap();
        let result = g.add_node(make_node("b", vec!["a"]));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DagError::CycleDetected(_)));
    }

    #[test]
    fn test_remove_node() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        let removed = g.remove_node("a").unwrap();
        assert_eq!(removed.id, "a");
        assert_eq!(g.node_count(), 0);
    }

    #[test]
    fn test_remove_node_with_dependents_fails() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g.add_node(make_node("b", vec!["a"])).unwrap();
        let result = g.remove_node("a");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DagError::HasDependents(_)));
    }

    #[test]
    fn test_is_complete() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g.add_node(make_node("b", vec!["a"])).unwrap();
        assert!(!g.is_complete());

        g.get_node_mut("a").unwrap().status = NodeStatus::Done;
        assert!(!g.is_complete());

        g.get_node_mut("b").unwrap().status = NodeStatus::Done;
        assert!(g.is_complete());
    }

    #[test]
    fn test_topological_sort() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g.add_node(make_node("b", vec!["a"])).unwrap();
        g.add_node(make_node("c", vec!["b"])).unwrap();

        let order = g.topological_sort().unwrap();
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
    }

    #[test]
    fn test_duplicate_node_id() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        let result = g.add_node(make_node("a", vec![]));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), DagError::DuplicateNode(_)));
    }

    #[test]
    fn test_ready_nodes_skips_failed() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g.add_node(make_node("b", vec!["a"])).unwrap();

        // Mark a as Failed -- b should NOT become ready.
        g.get_node_mut("a").unwrap().status = NodeStatus::Failed;
        let ready = g.ready_nodes();
        assert!(ready.is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut g = DagGraph::new();
        g.add_node(make_node("a", vec![])).unwrap();
        g.add_node(make_node("b", vec!["a"])).unwrap();

        let json = serde_json::to_string(&g).unwrap();
        let g2: DagGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(g2.node_count(), 2);
        assert!(g2.get_node("a").is_some());
        assert!(g2.get_node("b").is_some());
    }
}
