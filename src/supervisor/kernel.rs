use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use super::error::SupervisorError;
use super::health::{HealthLevel, HealthStatus, ProcessHealth};
use super::process::{ChildHandle, ProcessId, ProcessState, RestartPolicy, RestartTracker};

/// The kernel supervisor — manages all child processes.
pub struct KernelSupervisor {
    children: Arc<RwLock<HashMap<ProcessId, ChildHandle>>>,
    kernel_start: Instant,
    health_state: Arc<RwLock<HealthStatus>>,
}

impl KernelSupervisor {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            children: Arc::new(RwLock::new(HashMap::new())),
            kernel_start: now,
            health_state: Arc::new(RwLock::new(HealthStatus {
                status: HealthLevel::Healthy,
                kernel_uptime_secs: 0,
                active_processes: Vec::new(),
                last_checkpoint: None,
            })),
        }
    }

    /// Register a new child process with its restart policy.
    pub async fn register(&self, id: ProcessId, policy: RestartPolicy) {
        let handle = ChildHandle {
            id,
            state: ProcessState::Starting,
            restart_tracker: RestartTracker::new(policy),
            started_at: Instant::now(),
            pid: None,
        };
        let mut children = self.children.write().await;
        children.insert(id, handle);
        drop(children);
        self.refresh_health().await;
    }

    /// Update a child's state. Triggers restart logic if Failed.
    pub async fn update_state(
        &self,
        id: ProcessId,
        state: ProcessState,
    ) -> Result<(), SupervisorError> {
        {
            let mut children = self.children.write().await;
            let handle = children
                .get_mut(&id)
                .ok_or(SupervisorError::ProcessNotFound(id))?;

            // Validate transition: cannot go from Stopped back to Running directly.
            if handle.state == ProcessState::Stopped && state == ProcessState::Running {
                return Err(SupervisorError::InvalidTransition(
                    id,
                    handle.state.to_string(),
                    state.to_string(),
                ));
            }

            handle.state = state;

            // If transitioning to Running, reset the restart tracker.
            if state == ProcessState::Running {
                handle.restart_tracker.reset();
            }
        }

        if state == ProcessState::Failed {
            self.handle_failure(id).await?;
        }

        self.refresh_health().await;
        Ok(())
    }

    /// Get current health status.
    pub async fn health(&self) -> HealthStatus {
        self.refresh_health().await;
        self.health_state.read().await.clone()
    }

    /// Handle a child failure — apply restart policy with backoff.
    async fn handle_failure(&self, id: ProcessId) -> Result<(), SupervisorError> {
        let mut children = self.children.write().await;
        let handle = children
            .get_mut(&id)
            .ok_or(SupervisorError::ProcessNotFound(id))?;

        match handle.restart_tracker.record_restart() {
            Ok(_backoff) => {
                handle.state = ProcessState::Restarting;
                handle.started_at = Instant::now();
                // In a real implementation, we'd spawn the process again after the
                // backoff duration. For now we just mark the state.
                Ok(())
            }
            Err(_) => {
                // Exceeded max restarts — leave in Failed state.
                Err(SupervisorError::MaxRestartsExceeded(id))
            }
        }
    }

    /// Check if all registered processes are Running.
    pub async fn all_healthy(&self) -> bool {
        let children = self.children.read().await;
        !children.is_empty()
            && children
                .values()
                .all(|h| h.state == ProcessState::Running)
    }

    /// Refresh the cached health state from current child states.
    async fn refresh_health(&self) {
        let children = self.children.read().await;
        let process_healths: Vec<ProcessHealth> = children
            .values()
            .map(|h| ProcessHealth {
                id: h.id,
                state: h.state,
                uptime_secs: h.started_at.elapsed().as_secs(),
                restart_count: h.restart_tracker.restart_count(),
            })
            .collect();
        drop(children);

        let new_health = HealthStatus::compute(&process_healths, self.kernel_start);
        let mut health = self.health_state.write().await;
        *health = new_health;
    }
}

impl Default for KernelSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_check_health() {
        let sv = KernelSupervisor::new();
        sv.register(ProcessId::Loop0Meta, RestartPolicy::default())
            .await;

        let health = sv.health().await;
        assert_eq!(health.active_processes.len(), 1);
        assert_eq!(health.active_processes[0].id, ProcessId::Loop0Meta);
        assert_eq!(health.active_processes[0].state, ProcessState::Starting);
    }

    #[tokio::test]
    async fn update_to_running_makes_healthy() {
        let sv = KernelSupervisor::new();
        sv.register(ProcessId::Loop0Meta, RestartPolicy::default())
            .await;
        sv.update_state(ProcessId::Loop0Meta, ProcessState::Running)
            .await
            .unwrap();

        assert!(sv.all_healthy().await);
        let health = sv.health().await;
        assert_eq!(health.status, HealthLevel::Healthy);
    }

    #[tokio::test]
    async fn failure_triggers_restart() {
        let sv = KernelSupervisor::new();
        sv.register(ProcessId::ModelAdapter, RestartPolicy::default())
            .await;
        sv.update_state(ProcessId::ModelAdapter, ProcessState::Running)
            .await
            .unwrap();
        sv.update_state(ProcessId::ModelAdapter, ProcessState::Failed)
            .await
            .unwrap();

        let children = sv.children.read().await;
        let handle = children.get(&ProcessId::ModelAdapter).unwrap();
        assert_eq!(handle.state, ProcessState::Restarting);
    }

    #[tokio::test]
    async fn max_restarts_exceeded() {
        let policy = RestartPolicy {
            max_restarts: 1,
            restart_window: std::time::Duration::from_secs(60),
            backoff_base: std::time::Duration::from_millis(10),
            backoff_max: std::time::Duration::from_secs(1),
        };
        let sv = KernelSupervisor::new();
        sv.register(ProcessId::Loop1Agentic, policy).await;

        // First failure: should restart.
        sv.update_state(ProcessId::Loop1Agentic, ProcessState::Running)
            .await
            .unwrap();
        sv.update_state(ProcessId::Loop1Agentic, ProcessState::Failed)
            .await
            .unwrap();

        // Second failure: should exceed limit.
        let result = sv
            .update_state(ProcessId::Loop1Agentic, ProcessState::Failed)
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SupervisorError::MaxRestartsExceeded(ProcessId::Loop1Agentic)
        ));
    }

    #[tokio::test]
    async fn not_found_error() {
        let sv = KernelSupervisor::new();
        let result = sv
            .update_state(ProcessId::EmbeddingAdapter, ProcessState::Running)
            .await;
        assert!(matches!(
            result.unwrap_err(),
            SupervisorError::ProcessNotFound(ProcessId::EmbeddingAdapter)
        ));
    }

    #[tokio::test]
    async fn invalid_transition_stopped_to_running() {
        let sv = KernelSupervisor::new();
        sv.register(ProcessId::Loop2Harness, RestartPolicy::default())
            .await;
        sv.update_state(ProcessId::Loop2Harness, ProcessState::Running)
            .await
            .unwrap();
        sv.update_state(ProcessId::Loop2Harness, ProcessState::Stopped)
            .await
            .unwrap();

        let result = sv
            .update_state(ProcessId::Loop2Harness, ProcessState::Running)
            .await;
        assert!(matches!(
            result.unwrap_err(),
            SupervisorError::InvalidTransition(..)
        ));
    }

    #[tokio::test]
    async fn all_healthy_requires_at_least_one_process() {
        let sv = KernelSupervisor::new();
        assert!(!sv.all_healthy().await);
    }

    #[tokio::test]
    async fn mixed_states_not_all_healthy() {
        let sv = KernelSupervisor::new();
        sv.register(ProcessId::Loop0Meta, RestartPolicy::default())
            .await;
        sv.register(ProcessId::Loop1Agentic, RestartPolicy::default())
            .await;

        sv.update_state(ProcessId::Loop0Meta, ProcessState::Running)
            .await
            .unwrap();
        // Loop1Agentic still Starting.

        assert!(!sv.all_healthy().await);
    }
}
