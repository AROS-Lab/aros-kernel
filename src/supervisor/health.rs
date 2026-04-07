use serde::{Deserialize, Serialize};
use std::time::Instant;

use super::process::{ProcessId, ProcessState};

/// Health status reported by the kernel supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: HealthLevel,
    pub kernel_uptime_secs: u64,
    pub active_processes: Vec<ProcessHealth>,
    pub last_checkpoint: Option<String>, // ISO 8601 timestamp
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthLevel {
    Healthy,
    Degraded,
    Recovering,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessHealth {
    pub id: ProcessId,
    pub state: ProcessState,
    pub uptime_secs: u64,
    pub restart_count: u32,
}

impl HealthStatus {
    /// Compute aggregate health from individual process states.
    pub fn compute(processes: &[ProcessHealth], kernel_start: Instant) -> Self {
        let kernel_uptime_secs = kernel_start.elapsed().as_secs();

        let status = if processes.is_empty() {
            HealthLevel::Healthy
        } else {
            let all_running = processes
                .iter()
                .all(|p| p.state == ProcessState::Running);
            let any_failed = processes
                .iter()
                .any(|p| p.state == ProcessState::Failed);
            let any_restarting = processes
                .iter()
                .any(|p| p.state == ProcessState::Restarting || p.state == ProcessState::Starting);

            if all_running {
                HealthLevel::Healthy
            } else if any_failed {
                HealthLevel::Degraded
            } else if any_restarting {
                HealthLevel::Recovering
            } else {
                // All stopped or stopping — still considered degraded.
                HealthLevel::Degraded
            }
        };

        Self {
            status,
            kernel_uptime_secs,
            active_processes: processes.to_vec(),
            last_checkpoint: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_process(id: ProcessId, state: ProcessState) -> ProcessHealth {
        ProcessHealth {
            id,
            state,
            uptime_secs: 100,
            restart_count: 0,
        }
    }

    #[test]
    fn all_running_is_healthy() {
        let procs = vec![
            make_process(ProcessId::Loop0Meta, ProcessState::Running),
            make_process(ProcessId::Loop1Agentic, ProcessState::Running),
        ];
        let health = HealthStatus::compute(&procs, Instant::now());
        assert_eq!(health.status, HealthLevel::Healthy);
    }

    #[test]
    fn any_failed_is_degraded() {
        let procs = vec![
            make_process(ProcessId::Loop0Meta, ProcessState::Running),
            make_process(ProcessId::Loop1Agentic, ProcessState::Failed),
        ];
        let health = HealthStatus::compute(&procs, Instant::now());
        assert_eq!(health.status, HealthLevel::Degraded);
    }

    #[test]
    fn restarting_is_recovering() {
        let procs = vec![
            make_process(ProcessId::Loop0Meta, ProcessState::Running),
            make_process(ProcessId::ModelAdapter, ProcessState::Restarting),
        ];
        let health = HealthStatus::compute(&procs, Instant::now());
        assert_eq!(health.status, HealthLevel::Recovering);
    }

    #[test]
    fn starting_is_recovering() {
        let procs = vec![
            make_process(ProcessId::Loop2Harness, ProcessState::Starting),
        ];
        let health = HealthStatus::compute(&procs, Instant::now());
        assert_eq!(health.status, HealthLevel::Recovering);
    }

    #[test]
    fn empty_processes_is_healthy() {
        let health = HealthStatus::compute(&[], Instant::now());
        assert_eq!(health.status, HealthLevel::Healthy);
    }

    #[test]
    fn failed_takes_priority_over_restarting() {
        let procs = vec![
            make_process(ProcessId::Loop0Meta, ProcessState::Failed),
            make_process(ProcessId::ModelAdapter, ProcessState::Restarting),
        ];
        let health = HealthStatus::compute(&procs, Instant::now());
        assert_eq!(health.status, HealthLevel::Degraded);
    }

    #[test]
    fn health_status_serde_roundtrip() {
        let health = HealthStatus {
            status: HealthLevel::Healthy,
            kernel_uptime_secs: 42,
            active_processes: vec![make_process(ProcessId::Kernel, ProcessState::Running)],
            last_checkpoint: Some("2026-04-06T00:00:00Z".into()),
        };
        let json = serde_json::to_string(&health).unwrap();
        let back: HealthStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back.status, HealthLevel::Healthy);
        assert_eq!(back.kernel_uptime_secs, 42);
        assert_eq!(back.active_processes.len(), 1);
        assert_eq!(back.last_checkpoint.as_deref(), Some("2026-04-06T00:00:00Z"));
    }
}
