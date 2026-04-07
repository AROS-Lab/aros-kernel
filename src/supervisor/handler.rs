//! KernelRequestHandler — the supervisor's implementation of `dispatch::server::RequestHandler`.
//!
//! Routes incoming JSON-RPC messages from loop subprocesses to the appropriate
//! kernel subsystems (DAG executor, governor, trigger bus).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::sync::RwLock;

use crate::dispatch::contracts::{LoopTrigger, TriggerKind};
use crate::dispatch::rpc::{
    RpcMethod, ERROR_INTERNAL, ERROR_INVALID_PARAMS, ERROR_PERMISSION_DENIED,
};
use crate::dispatch::server::RequestHandler;
use crate::envelope::task_envelope::Priority;
use crate::governor::ResourceGovernor;
use crate::store::StateStore;
use crate::supervisor::kernel::KernelSupervisor;
use crate::supervisor::process::ProcessId;

/// Tracks active Loop 1 task instances and their socket paths.
#[derive(Debug)]
pub struct TaskRegistry {
    /// task_id → socket path for active Loop 1 subprocesses.
    tasks: RwLock<HashMap<String, PathBuf>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new Loop 1 task with its socket path.
    pub async fn register(&self, task_id: String, socket_path: PathBuf) {
        self.tasks.write().await.insert(task_id, socket_path);
    }

    /// Remove a task (on completion or failure).
    pub async fn remove(&self, task_id: &str) -> Option<PathBuf> {
        self.tasks.write().await.remove(task_id)
    }

    /// Look up the socket path for a task.
    pub async fn get(&self, task_id: &str) -> Option<PathBuf> {
        self.tasks.read().await.get(task_id).cloned()
    }

    /// Number of active tasks.
    pub async fn active_count(&self) -> usize {
        self.tasks.read().await.len()
    }
}

impl Default for TaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The kernel-side request handler for the RPC server.
///
/// Receives JSON-RPC requests from loop subprocesses on `kernel.sock`
/// and routes them to the appropriate kernel subsystem.
#[derive(Clone)]
pub struct KernelRequestHandler {
    supervisor: Arc<KernelSupervisor>,
    governor: Arc<ResourceGovernor>,
    task_registry: Arc<TaskRegistry>,
    state_store: Arc<std::sync::Mutex<Box<dyn StateStore>>>,
    trigger_seq: Arc<AtomicU64>,
}

impl KernelRequestHandler {
    pub fn new(
        supervisor: Arc<KernelSupervisor>,
        governor: Arc<ResourceGovernor>,
        task_registry: Arc<TaskRegistry>,
        state_store: Box<dyn StateStore>,
    ) -> Self {
        Self {
            supervisor,
            governor,
            task_registry,
            state_store: Arc::new(std::sync::Mutex::new(state_store)),
            trigger_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_seq(&self) -> u64 {
        self.trigger_seq.fetch_add(1, Ordering::SeqCst)
    }

    /// Handle task.progress — Loop 1 reports incremental progress.
    async fn handle_task_progress(&self, params: Option<Value>) -> Result<Value, (i32, String)> {
        let params = params.ok_or((ERROR_INVALID_PARAMS, "missing params".into()))?;

        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or((ERROR_INVALID_PARAMS, "missing task_id".into()))?;
        let dag_id = params
            .get("dag_id")
            .and_then(|v| v.as_str())
            .ok_or((ERROR_INVALID_PARAMS, "missing dag_id".into()))?;
        let phase = params
            .get("phase")
            .and_then(|v| v.as_str())
            .ok_or((ERROR_INVALID_PARAMS, "missing phase".into()))?;

        // Verify task is registered
        if self.task_registry.get(task_id).await.is_none() {
            return Err((
                ERROR_PERMISSION_DENIED,
                format!("unknown task_id: {task_id}"),
            ));
        }

        tracing::debug!(
            task_id,
            dag_id,
            phase,
            "task progress reported"
        );

        // TODO: Update DAG node status via DagRuntime
        // TODO: Emit telemetry span

        Ok(serde_json::json!({
            "status": "acknowledged",
            "task_id": task_id,
        }))
    }

    /// Handle task.complete — Loop 1 reports successful completion.
    async fn handle_task_complete(&self, params: Option<Value>) -> Result<Value, (i32, String)> {
        let params = params.ok_or((ERROR_INVALID_PARAMS, "missing params".into()))?;

        let task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or((ERROR_INVALID_PARAMS, "missing task_id".into()))?
            .to_string();
        let dag_id = params
            .get("dag_id")
            .and_then(|v| v.as_str())
            .ok_or((ERROR_INVALID_PARAMS, "missing dag_id".into()))?;
        let tokens_used = params
            .get("tokens_used")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // Remove from registry
        let socket_path = self.task_registry.remove(&task_id).await;
        if socket_path.is_none() {
            return Err((
                ERROR_PERMISSION_DENIED,
                format!("unknown task_id: {task_id}"),
            ));
        }

        // Release governor budget
        // TODO: determine priority from envelope lookup
        self.governor.task_ended(Priority::P1Normal, 0);
        if tokens_used > 0 {
            self.governor.tokens_used(Priority::P1Normal, tokens_used);
        }

        tracing::info!(
            task_id = task_id.as_str(),
            dag_id,
            tokens_used,
            "task completed"
        );

        // TODO: Mark DAG node as Done
        // TODO: Emit telemetry span
        // TODO: Clean up socket file at socket_path

        Ok(serde_json::json!({
            "status": "completed",
            "task_id": task_id,
        }))
    }

    /// Handle task.submit — Loop 2 dispatches a new task to Loop 1.
    async fn handle_task_submit(&self, params: Option<Value>) -> Result<Value, (i32, String)> {
        let params = params.ok_or((ERROR_INVALID_PARAMS, "missing params".into()))?;

        // Deserialize the task envelope
        let envelope: crate::envelope::TaskEnvelope =
            serde_json::from_value(params).map_err(|e| {
                (
                    ERROR_INVALID_PARAMS,
                    format!("invalid TaskEnvelope: {e}"),
                )
            })?;

        // Validate the envelope
        envelope.validate().map_err(|e| {
            (ERROR_INVALID_PARAMS, format!("envelope validation failed: {e}"))
        })?;

        // Admission check
        let decision = self
            .governor
            .check_admission(
                envelope.priority,
                envelope.resource_budget.max_rss_mb,
                envelope.resource_budget.max_tokens as u64,
            )
            .await;

        match decision {
            crate::governor::AdmissionDecision::Admitted => {}
            crate::governor::AdmissionDecision::Queued { reason } => {
                return Ok(serde_json::json!({
                    "status": "queued",
                    "task_id": envelope.task_id,
                    "reason": reason,
                }));
            }
            crate::governor::AdmissionDecision::Throttled { reason } => {
                return Err((ERROR_BUDGET_EXCEEDED, format!("throttled: {reason}")));
            }
            crate::governor::AdmissionDecision::Shed { reason } => {
                return Err((ERROR_BUDGET_EXCEEDED, format!("shed: {reason}")));
            }
        }

        // Record admission in governor
        self.governor
            .task_started(envelope.priority, envelope.resource_budget.max_rss_mb);

        let task_id = envelope.task_id.clone();
        tracing::info!(
            task_id = task_id.as_str(),
            priority = ?envelope.priority,
            security_zone = ?envelope.security_zone,
            "task admitted"
        );

        // TODO: Spawn Loop 1 subprocess with dedicated socket
        // TODO: Register task_id → socket_path in task_registry
        // For now, register with a placeholder path
        // In production: spawn subprocess, bind socket, register path

        Ok(serde_json::json!({
            "status": "admitted",
            "task_id": task_id,
        }))
    }

    /// Handle loop.trigger — route a LoopTrigger to its target.
    async fn handle_loop_trigger(&self, params: Option<Value>) -> Result<Value, (i32, String)> {
        let params = params.ok_or((ERROR_INVALID_PARAMS, "missing params".into()))?;

        let trigger: LoopTrigger = serde_json::from_value(params)
            .map_err(|e| (ERROR_INVALID_PARAMS, format!("invalid LoopTrigger: {e}")))?;

        tracing::debug!(
            seq = trigger.seq,
            source = ?trigger.source,
            target = ?trigger.target,
            "routing trigger"
        );

        // Verify source process is running
        let health = self.supervisor.health().await;
        let source_running = health
            .active_processes
            .iter()
            .any(|p| p.id == trigger.source && p.state == crate::supervisor::process::ProcessState::Running);

        if !source_running {
            return Err((
                ERROR_PERMISSION_DENIED,
                format!("source process {:?} is not running", trigger.source),
            ));
        }

        match &trigger.kind {
            TriggerKind::MetaCycleRequest { trigger_source } => {
                tracing::info!(trigger_source, "meta cycle requested");
                // TODO: Validate with identity checker, then respond with MetaCycleAuthorized
                Ok(serde_json::json!({
                    "status": "authorized",
                    "cycle_id": format!("cycle-{}", self.next_seq()),
                }))
            }
            TriggerKind::MetaCycleComplete {
                cycle_id,
                policy_changed,
                drift_score,
            } => {
                tracing::info!(
                    cycle_id,
                    policy_changed,
                    drift_score,
                    "meta cycle completed"
                );

                // Persist drift score and policy state for UI read path
                let mut store = self.state_store.lock().map_err(|e| {
                    (ERROR_INTERNAL, format!("state store lock poisoned: {e}"))
                })?;

                // Write drift score — UI drift gauge reads this
                store
                    .put(
                        "sie/identity/last_drift",
                        drift_score.to_string().into_bytes(),
                    )
                    .map_err(|e| (ERROR_INTERNAL, format!("state store write failed: {e}")))?;

                // Write cycle ID as latest completed cycle
                store
                    .put(
                        "sie/meta/last_cycle",
                        cycle_id.as_bytes().to_vec(),
                    )
                    .map_err(|e| (ERROR_INTERNAL, format!("state store write failed: {e}")))?;

                // If policy changed, update the policy head pointer
                if *policy_changed {
                    store
                        .put(
                            "sie/policy/head",
                            cycle_id.as_bytes().to_vec(),
                        )
                        .map_err(|e| (ERROR_INTERNAL, format!("state store write failed: {e}")))?;

                    tracing::info!(cycle_id, "policy head updated");
                }

                Ok(serde_json::json!({
                    "status": "persisted",
                    "cycle_id": cycle_id,
                    "drift_score": drift_score,
                    "policy_updated": policy_changed,
                }))
            }
            TriggerKind::TaskCancel { task_id, reason } => {
                tracing::info!(task_id, reason, "cancel requested");
                // TODO: Forward to loop1-{task_id}.sock via RpcClient
                if let Some(_socket_path) = self.task_registry.get(task_id).await {
                    // TODO: RpcClient::connect(socket_path).call("task.cancel", ...)
                    Ok(serde_json::json!({
                        "status": "cancel_forwarded",
                        "task_id": task_id,
                    }))
                } else {
                    Err((
                        ERROR_INVALID_PARAMS,
                        format!("unknown task_id: {task_id}"),
                    ))
                }
            }
            _ => {
                // Generic trigger routing — forward to target process
                // TODO: Look up target socket path and forward via RpcClient
                Ok(serde_json::json!({
                    "status": "routed",
                    "seq": trigger.seq,
                    "target": format!("{:?}", trigger.target),
                }))
            }
        }
    }
}

use crate::dispatch::rpc::ERROR_BUDGET_EXCEEDED;

impl RequestHandler for KernelRequestHandler {
    async fn handle(
        &self,
        method: RpcMethod,
        params: Option<Value>,
    ) -> Result<Value, (i32, String)> {
        match method {
            RpcMethod::Ping => Ok(Value::String("pong".to_string())),
            RpcMethod::TaskSubmit => self.handle_task_submit(params).await,
            RpcMethod::TaskProgress => self.handle_task_progress(params).await,
            RpcMethod::TaskComplete => self.handle_task_complete(params).await,
            RpcMethod::TaskCancel => {
                // Wrap as a trigger for uniform routing
                let params = params.ok_or((ERROR_INVALID_PARAMS, "missing params".into()))?;
                let task_id = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .ok_or((ERROR_INVALID_PARAMS, "missing task_id".into()))?
                    .to_string();
                let reason = params
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("requested")
                    .to_string();

                let trigger = LoopTrigger::new(
                    self.next_seq(),
                    ProcessId::Kernel,
                    ProcessId::Loop1Agentic,
                    TriggerKind::TaskCancel { task_id, reason },
                );

                self.handle_loop_trigger(Some(serde_json::to_value(trigger).map_err(
                    |e| (ERROR_INTERNAL, format!("serialization error: {e}")),
                )?))
                .await
            }
            RpcMethod::LoopTrigger => self.handle_loop_trigger(params).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governor::GovernorConfig;
    use crate::store::SqliteStateStore;
    use crate::supervisor::process::RestartPolicy;

    async fn make_handler() -> KernelRequestHandler {
        let supervisor = Arc::new(KernelSupervisor::new());
        supervisor
            .register(ProcessId::Kernel, RestartPolicy::default())
            .await;
        supervisor
            .update_state(ProcessId::Kernel, crate::supervisor::process::ProcessState::Running)
            .await
            .unwrap();
        supervisor
            .register(ProcessId::Loop0Meta, RestartPolicy::default())
            .await;
        supervisor
            .update_state(ProcessId::Loop0Meta, crate::supervisor::process::ProcessState::Running)
            .await
            .unwrap();
        supervisor
            .register(ProcessId::Loop1Agentic, RestartPolicy::default())
            .await;
        supervisor
            .update_state(
                ProcessId::Loop1Agentic,
                crate::supervisor::process::ProcessState::Running,
            )
            .await
            .unwrap();
        supervisor
            .register(ProcessId::Loop2Harness, RestartPolicy::default())
            .await;
        supervisor
            .update_state(
                ProcessId::Loop2Harness,
                crate::supervisor::process::ProcessState::Running,
            )
            .await
            .unwrap();

        let governor = Arc::new(ResourceGovernor::new(GovernorConfig::default()));
        let registry = Arc::new(TaskRegistry::new());
        let store = Box::new(SqliteStateStore::open(":memory:").unwrap());

        KernelRequestHandler::new(supervisor, governor, registry, store)
    }

    #[tokio::test]
    async fn test_ping() {
        let handler = make_handler().await;
        let result = handler.handle(RpcMethod::Ping, None).await;
        assert_eq!(result.unwrap(), Value::String("pong".to_string()));
    }

    #[tokio::test]
    async fn test_task_progress_unknown_task() {
        let handler = make_handler().await;
        let params = serde_json::json!({
            "task_id": "nonexistent",
            "dag_id": "dag-1",
            "phase": "executing",
        });
        let result = handler
            .handle(RpcMethod::TaskProgress, Some(params))
            .await;
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ERROR_PERMISSION_DENIED);
    }

    #[tokio::test]
    async fn test_task_progress_known_task() {
        let handler = make_handler().await;
        handler
            .task_registry
            .register("task-1".into(), PathBuf::from("/tmp/loop1-task-1.sock"))
            .await;

        let params = serde_json::json!({
            "task_id": "task-1",
            "dag_id": "dag-1",
            "phase": "executing",
        });
        let result = handler
            .handle(RpcMethod::TaskProgress, Some(params))
            .await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "acknowledged");
    }

    #[tokio::test]
    async fn test_task_complete_releases_budget() {
        let handler = make_handler().await;
        handler
            .task_registry
            .register("task-2".into(), PathBuf::from("/tmp/loop1-task-2.sock"))
            .await;

        // Start a task in governor to have something to release
        handler.governor.task_started(Priority::P1Normal, 256);

        let params = serde_json::json!({
            "task_id": "task-2",
            "dag_id": "dag-1",
            "tokens_used": 5000_u64,
        });
        let result = handler
            .handle(RpcMethod::TaskComplete, Some(params))
            .await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "completed");

        // Task should be removed from registry
        assert!(handler.task_registry.get("task-2").await.is_none());
    }

    #[tokio::test]
    async fn test_task_submit_admission() {
        let handler = make_handler().await;

        let envelope = crate::envelope::TaskEnvelope::new(
            "test-task",
            "test-dag",
            crate::envelope::TaskSpec {
                title: "test task".into(),
                description: "test".into(),
                working_dir: None,
                env_vars: std::collections::HashMap::new(),
                max_retries: 0,
            },
            crate::envelope::SecurityZone::Green,
            Priority::P1Normal,
        );

        let params = serde_json::to_value(envelope).unwrap();
        let result = handler.handle(RpcMethod::TaskSubmit, Some(params)).await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "admitted");
    }

    #[tokio::test]
    async fn test_loop_trigger_meta_cycle_request() {
        let handler = make_handler().await;

        let trigger = LoopTrigger::new(
            1,
            ProcessId::Loop0Meta,
            ProcessId::Kernel,
            TriggerKind::MetaCycleRequest {
                trigger_source: "schedule".into(),
            },
        );

        let params = serde_json::to_value(trigger).unwrap();
        let result = handler
            .handle(RpcMethod::LoopTrigger, Some(params))
            .await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "authorized");
    }

    #[tokio::test]
    async fn test_loop_trigger_from_stopped_process_rejected() {
        let supervisor = Arc::new(KernelSupervisor::new());
        supervisor
            .register(ProcessId::Loop0Meta, RestartPolicy::default())
            .await;
        // Don't transition to Running — stays in Starting

        let governor = Arc::new(ResourceGovernor::new(GovernorConfig::default()));
        let registry = Arc::new(TaskRegistry::new());
        let store = Box::new(SqliteStateStore::open(":memory:").unwrap());
        let handler = KernelRequestHandler::new(supervisor, governor, registry, store);

        let trigger = LoopTrigger::new(
            1,
            ProcessId::Loop0Meta,
            ProcessId::Kernel,
            TriggerKind::MetaCycleRequest {
                trigger_source: "test".into(),
            },
        );

        let params = serde_json::to_value(trigger).unwrap();
        let result = handler
            .handle(RpcMethod::LoopTrigger, Some(params))
            .await;
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, ERROR_PERMISSION_DENIED);
    }

    #[tokio::test]
    async fn test_task_cancel_unknown_task() {
        let handler = make_handler().await;
        let params = serde_json::json!({
            "task_id": "nonexistent",
            "reason": "timeout",
        });
        let result = handler.handle(RpcMethod::TaskCancel, Some(params)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_meta_cycle_complete_persists_to_store() {
        let handler = make_handler().await;

        let trigger = LoopTrigger::new(
            1,
            ProcessId::Loop0Meta,
            ProcessId::Kernel,
            TriggerKind::MetaCycleComplete {
                cycle_id: "cycle-42".into(),
                policy_changed: true,
                drift_score: 0.15,
            },
        );

        let params = serde_json::to_value(trigger).unwrap();
        let result = handler
            .handle(RpcMethod::LoopTrigger, Some(params))
            .await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["status"], "persisted");
        assert_eq!(val["policy_updated"], true);

        // Verify state store writes
        let store = handler.state_store.lock().unwrap();
        let drift = store.get("sie/identity/last_drift").unwrap().unwrap();
        assert_eq!(String::from_utf8(drift).unwrap(), "0.15");

        let head = store.get("sie/policy/head").unwrap().unwrap();
        assert_eq!(String::from_utf8(head).unwrap(), "cycle-42");

        let last_cycle = store.get("sie/meta/last_cycle").unwrap().unwrap();
        assert_eq!(String::from_utf8(last_cycle).unwrap(), "cycle-42");
    }

    #[tokio::test]
    async fn test_meta_cycle_complete_no_policy_change() {
        let handler = make_handler().await;

        let trigger = LoopTrigger::new(
            2,
            ProcessId::Loop0Meta,
            ProcessId::Kernel,
            TriggerKind::MetaCycleComplete {
                cycle_id: "cycle-43".into(),
                policy_changed: false,
                drift_score: 0.05,
            },
        );

        let params = serde_json::to_value(trigger).unwrap();
        let result = handler
            .handle(RpcMethod::LoopTrigger, Some(params))
            .await;
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["policy_updated"], false);

        // drift_score and last_cycle should be written, but NOT policy/head
        let store = handler.state_store.lock().unwrap();
        let drift = store.get("sie/identity/last_drift").unwrap().unwrap();
        assert_eq!(String::from_utf8(drift).unwrap(), "0.05");

        assert!(store.get("sie/policy/head").unwrap().is_none());
    }

    #[tokio::test]
    async fn test_task_registry_lifecycle() {
        let registry = TaskRegistry::new();
        assert_eq!(registry.active_count().await, 0);

        registry
            .register("t1".into(), PathBuf::from("/tmp/t1.sock"))
            .await;
        registry
            .register("t2".into(), PathBuf::from("/tmp/t2.sock"))
            .await;
        assert_eq!(registry.active_count().await, 2);

        assert!(registry.get("t1").await.is_some());
        assert!(registry.get("t3").await.is_none());

        registry.remove("t1").await;
        assert_eq!(registry.active_count().await, 1);
        assert!(registry.get("t1").await.is_none());
    }
}
