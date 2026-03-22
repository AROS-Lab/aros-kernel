use serde::{Deserialize, Serialize};

use crate::scheduler::admission::ResourceRequirements;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_secs: f64,
}

pub trait AgentType: Send + Sync {
    /// Execute a task and return the result.
    fn execute(
        &self,
        task: &str,
        timeout_secs: u64,
    ) -> impl std::future::Future<Output = AgentResult> + Send;

    /// Resource requirements for scheduling.
    fn resource_requirements(&self) -> ResourceRequirements;

    /// Agent type name.
    fn name(&self) -> &str;
}
