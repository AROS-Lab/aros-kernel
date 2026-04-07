use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::error::EnvelopeError;

/// Version of the envelope schema for forward compatibility.
pub const ENVELOPE_VERSION: u32 = 1;

/// Generate an ISO 8601 timestamp from the current system time.
fn iso8601_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    // Break epoch seconds into date/time components.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Civil date from days since epoch (algorithm from Howard Hinnant).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Security zone determines what the subprocess can access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SecurityZone {
    /// Any provider, any network access.
    Green,
    /// Approved providers only, restricted network.
    Yellow,
    /// Local models only (e.g. Ollama), no external network.
    Red,
}

/// Priority tier for resource allocation and model adapter queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    /// Loop 0 meta-observations, health checks. Always admitted.
    P0Critical,
    /// Loop 1 task execution. Standard admission.
    P1Normal,
    /// SIE experiments, shadow tests. First to shed under pressure.
    P2Background,
}

/// Resource budget for a single task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceBudget {
    /// Maximum resident set size in MB.
    pub max_rss_mb: u32,
    /// Maximum wall-clock time.
    pub max_wall_time: Duration,
    /// Maximum model tokens (input + output combined).
    pub max_tokens: u64,
    /// Soft ceiling percentage (0.0-1.0) that triggers budget warning.
    pub budget_warning_threshold: f64,
}

impl Default for ResourceBudget {
    fn default() -> Self {
        Self {
            max_rss_mb: 512,
            max_wall_time: Duration::from_secs(300),
            max_tokens: 100_000,
            budget_warning_threshold: 0.9,
        }
    }
}

/// A tool endpoint available to the task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEndpoint {
    pub name: String,
    pub socket_path: String,
    pub capabilities: Vec<String>,
}

/// Checkpoint policy for the task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointPolicy {
    /// Checkpoint every N seconds.
    pub interval: Duration,
    /// Also checkpoint after each tool call.
    pub on_tool_call: bool,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(60),
            on_tool_call: true,
        }
    }
}

/// The actual task to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Human-readable title.
    pub title: String,
    /// Detailed description / prompt.
    pub description: String,
    /// Working directory for execution.
    pub working_dir: Option<String>,
    /// Environment variables to set.
    pub env_vars: HashMap<String, String>,
    /// Maximum retry count.
    pub max_retries: u32,
}

/// The task envelope — the complete contract for Loop 1 execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEnvelope {
    /// Unique task identifier.
    pub task_id: String,
    /// Parent DAG identifier (from Loop 2).
    pub parent_dag_id: String,
    /// The task specification (what to do).
    pub task_spec: TaskSpec,
    /// Security zone for this task.
    pub security_zone: SecurityZone,
    /// Priority tier.
    pub priority: Priority,
    /// Resource budget.
    pub resource_budget: ResourceBudget,
    /// Available tool endpoints.
    pub tool_endpoints: Vec<ToolEndpoint>,
    /// Checkpoint policy.
    pub checkpoint_policy: CheckpointPolicy,
    /// When this envelope was created (ISO 8601).
    pub created_at: String,
    /// Schema version for forward compatibility.
    pub envelope_version: u32,
}

impl TaskEnvelope {
    /// Create a new envelope with defaults.
    pub fn new(
        task_id: impl Into<String>,
        parent_dag_id: impl Into<String>,
        task_spec: TaskSpec,
        security_zone: SecurityZone,
        priority: Priority,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            parent_dag_id: parent_dag_id.into(),
            task_spec,
            security_zone,
            priority,
            resource_budget: ResourceBudget::default(),
            tool_endpoints: Vec::new(),
            checkpoint_policy: CheckpointPolicy::default(),
            created_at: iso8601_now(),
            envelope_version: ENVELOPE_VERSION,
        }
    }

    /// Check if the token budget has been exceeded.
    pub fn is_token_budget_exceeded(&self, tokens_used: u64) -> bool {
        tokens_used > self.resource_budget.max_tokens
    }

    /// Check if the token budget warning threshold has been reached.
    pub fn is_token_budget_warning(&self, tokens_used: u64) -> bool {
        let threshold =
            (self.resource_budget.max_tokens as f64 * self.resource_budget.budget_warning_threshold)
                as u64;
        tokens_used >= threshold
    }

    /// Check if wall time has been exceeded.
    pub fn is_wall_time_exceeded(&self, elapsed: Duration) -> bool {
        elapsed > self.resource_budget.max_wall_time
    }

    /// Validate the envelope (all required fields present, version correct).
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.task_id.is_empty() {
            return Err(EnvelopeError::EmptyTaskId);
        }
        if self.parent_dag_id.is_empty() {
            return Err(EnvelopeError::EmptyDagId);
        }
        if self.envelope_version != ENVELOPE_VERSION {
            return Err(EnvelopeError::UnsupportedVersion(self.envelope_version));
        }
        if self.resource_budget.budget_warning_threshold < 0.0
            || self.resource_budget.budget_warning_threshold > 1.0
        {
            return Err(EnvelopeError::InvalidBudget(
                "budget_warning_threshold must be between 0.0 and 1.0".into(),
            ));
        }
        if self.resource_budget.max_tokens == 0 {
            return Err(EnvelopeError::InvalidBudget(
                "max_tokens must be greater than 0".into(),
            ));
        }
        if self.resource_budget.max_rss_mb == 0 {
            return Err(EnvelopeError::InvalidBudget(
                "max_rss_mb must be greater than 0".into(),
            ));
        }
        if self.resource_budget.max_wall_time.is_zero() {
            return Err(EnvelopeError::InvalidBudget(
                "max_wall_time must be greater than 0".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task_spec() -> TaskSpec {
        TaskSpec {
            title: "Test task".into(),
            description: "Do something useful".into(),
            working_dir: Some("/tmp/work".into()),
            env_vars: HashMap::from([("FOO".into(), "bar".into())]),
            max_retries: 3,
        }
    }

    fn sample_envelope() -> TaskEnvelope {
        TaskEnvelope::new(
            "task-001",
            "dag-abc",
            sample_task_spec(),
            SecurityZone::Green,
            Priority::P1Normal,
        )
    }

    #[test]
    fn test_create_envelope() {
        let env = sample_envelope();
        assert_eq!(env.task_id, "task-001");
        assert_eq!(env.parent_dag_id, "dag-abc");
        assert_eq!(env.envelope_version, ENVELOPE_VERSION);
        assert_eq!(env.security_zone, SecurityZone::Green);
        assert_eq!(env.priority, Priority::P1Normal);
        assert!(!env.created_at.is_empty());
    }

    #[test]
    fn test_validate_ok() {
        let env = sample_envelope();
        assert!(env.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_task_id() {
        let mut env = sample_envelope();
        env.task_id = String::new();
        let err = env.validate().unwrap_err();
        assert!(matches!(err, EnvelopeError::EmptyTaskId));
    }

    #[test]
    fn test_validate_empty_dag_id() {
        let mut env = sample_envelope();
        env.parent_dag_id = String::new();
        let err = env.validate().unwrap_err();
        assert!(matches!(err, EnvelopeError::EmptyDagId));
    }

    #[test]
    fn test_validate_bad_version() {
        let mut env = sample_envelope();
        env.envelope_version = 999;
        let err = env.validate().unwrap_err();
        assert!(matches!(err, EnvelopeError::UnsupportedVersion(999)));
    }

    #[test]
    fn test_validate_bad_threshold_high() {
        let mut env = sample_envelope();
        env.resource_budget.budget_warning_threshold = 1.5;
        let err = env.validate().unwrap_err();
        assert!(matches!(err, EnvelopeError::InvalidBudget(_)));
    }

    #[test]
    fn test_validate_bad_threshold_negative() {
        let mut env = sample_envelope();
        env.resource_budget.budget_warning_threshold = -0.1;
        assert!(env.validate().is_err());
    }

    #[test]
    fn test_validate_zero_tokens() {
        let mut env = sample_envelope();
        env.resource_budget.max_tokens = 0;
        assert!(env.validate().is_err());
    }

    #[test]
    fn test_validate_zero_rss() {
        let mut env = sample_envelope();
        env.resource_budget.max_rss_mb = 0;
        assert!(env.validate().is_err());
    }

    #[test]
    fn test_validate_zero_wall_time() {
        let mut env = sample_envelope();
        env.resource_budget.max_wall_time = Duration::ZERO;
        assert!(env.validate().is_err());
    }

    #[test]
    fn test_token_budget_exceeded() {
        let env = sample_envelope();
        assert!(!env.is_token_budget_exceeded(100_000));
        assert!(env.is_token_budget_exceeded(100_001));
    }

    #[test]
    fn test_token_budget_warning() {
        let env = sample_envelope();
        // Default threshold is 0.9, max_tokens is 100_000 => warning at 90_000
        assert!(!env.is_token_budget_warning(89_999));
        assert!(env.is_token_budget_warning(90_000));
        assert!(env.is_token_budget_warning(100_000));
    }

    #[test]
    fn test_wall_time_exceeded() {
        let env = sample_envelope();
        assert!(!env.is_wall_time_exceeded(Duration::from_secs(300)));
        assert!(env.is_wall_time_exceeded(Duration::from_secs(301)));
    }

    #[test]
    fn test_serialization_roundtrip() {
        let env = sample_envelope();
        let json = serde_json::to_string_pretty(&env).expect("serialize");
        let deserialized: TaskEnvelope = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.task_id, env.task_id);
        assert_eq!(deserialized.parent_dag_id, env.parent_dag_id);
        assert_eq!(deserialized.security_zone, env.security_zone);
        assert_eq!(deserialized.priority, env.priority);
        assert_eq!(deserialized.envelope_version, env.envelope_version);
        assert_eq!(deserialized.task_spec.title, env.task_spec.title);
        assert_eq!(
            deserialized.resource_budget.max_tokens,
            env.resource_budget.max_tokens
        );
        assert_eq!(deserialized.created_at, env.created_at);
    }

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::P0Critical < Priority::P1Normal);
        assert!(Priority::P1Normal < Priority::P2Background);
    }

    #[test]
    fn test_default_resource_budget() {
        let budget = ResourceBudget::default();
        assert_eq!(budget.max_rss_mb, 512);
        assert_eq!(budget.max_wall_time, Duration::from_secs(300));
        assert_eq!(budget.max_tokens, 100_000);
        assert!((budget.budget_warning_threshold - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_default_checkpoint_policy() {
        let policy = CheckpointPolicy::default();
        assert_eq!(policy.interval, Duration::from_secs(60));
        assert!(policy.on_tool_call);
    }

    #[test]
    fn test_tool_endpoint_serialization() {
        let endpoint = ToolEndpoint {
            name: "bash".into(),
            socket_path: "/tmp/bash.sock".into(),
            capabilities: vec!["execute".into(), "read".into()],
        };
        let json = serde_json::to_string(&endpoint).expect("serialize");
        let deserialized: ToolEndpoint = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.name, "bash");
        assert_eq!(deserialized.capabilities.len(), 2);
    }

    #[test]
    fn test_envelope_with_tool_endpoints() {
        let mut env = sample_envelope();
        env.tool_endpoints.push(ToolEndpoint {
            name: "bash".into(),
            socket_path: "/tmp/bash.sock".into(),
            capabilities: vec!["execute".into()],
        });
        assert!(env.validate().is_ok());
        let json = serde_json::to_string(&env).expect("serialize");
        let deserialized: TaskEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.tool_endpoints.len(), 1);
        assert_eq!(deserialized.tool_endpoints[0].name, "bash");
    }

    #[test]
    fn test_created_at_is_iso8601() {
        let env = sample_envelope();
        // Basic format check: YYYY-MM-DDTHH:MM:SSZ
        assert!(env.created_at.contains('T'));
        assert!(env.created_at.ends_with('Z'));
        assert_eq!(env.created_at.len(), 20);
    }

    #[test]
    fn test_security_zone_equality() {
        assert_ne!(SecurityZone::Green, SecurityZone::Red);
        assert_eq!(SecurityZone::Yellow, SecurityZone::Yellow);
    }
}
