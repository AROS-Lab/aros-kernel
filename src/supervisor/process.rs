use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use super::error::SupervisorError;

/// Identity of a supervised process — used for ACL enforcement in state store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessId {
    Init,
    Kernel,
    Loop0Meta,
    Loop1Agentic,
    Loop2Harness,
    ModelAdapter,
    EmbeddingAdapter,
}

/// Current state of a supervised child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
    Restarting,
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Restart policy for a supervised process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub restart_window: Duration,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            restart_window: Duration::from_secs(300), // 5 minutes
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// Tracks restart history for backoff calculation.
#[derive(Debug, Clone)]
pub struct RestartTracker {
    policy: RestartPolicy,
    restart_times: Vec<Instant>,
    consecutive_failures: u32,
}

impl RestartTracker {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            restart_times: Vec::new(),
            consecutive_failures: 0,
        }
    }

    /// Record a restart attempt. Returns `Ok(backoff_duration)` or `Err` if max exceeded.
    pub fn record_restart(&mut self) -> Result<Duration, SupervisorError> {
        let now = Instant::now();

        // Prune restarts outside the window.
        let cutoff = now - self.policy.restart_window;
        self.restart_times.retain(|t| *t >= cutoff);

        // Check if we've exceeded the limit within the window.
        if self.restart_times.len() as u32 >= self.policy.max_restarts {
            // We use a dummy ProcessId here; the caller re-wraps if needed.
            return Err(SupervisorError::MaxRestartsExceeded(ProcessId::Init));
        }

        self.restart_times.push(now);
        self.consecutive_failures += 1;

        Ok(self.current_backoff())
    }

    /// Reset after a successful sustained run.
    pub fn reset(&mut self) {
        self.restart_times.clear();
        self.consecutive_failures = 0;
    }

    /// Current backoff with exponential increase, capped at `backoff_max`.
    fn current_backoff(&self) -> Duration {
        let exp = self.consecutive_failures.saturating_sub(1);
        let multiplier = 2u64.saturating_pow(exp);
        let backoff = self.policy.backoff_base.saturating_mul(multiplier as u32);
        std::cmp::min(backoff, self.policy.backoff_max)
    }

    /// Number of restarts recorded within the current window.
    pub fn restart_count(&self) -> u32 {
        self.restart_times.len() as u32
    }

    /// Number of consecutive failures since last reset.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }
}

/// Handle to a supervised child process.
#[derive(Debug)]
pub struct ChildHandle {
    pub id: ProcessId,
    pub state: ProcessState,
    pub restart_tracker: RestartTracker,
    pub started_at: Instant,
    pub pid: Option<u32>, // OS process ID when running as subprocess
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_restart_policy() {
        let policy = RestartPolicy::default();
        assert_eq!(policy.max_restarts, 5);
        assert_eq!(policy.restart_window, Duration::from_secs(300));
        assert_eq!(policy.backoff_base, Duration::from_millis(100));
        assert_eq!(policy.backoff_max, Duration::from_secs(30));
    }

    #[test]
    fn tracker_records_restarts_within_limit() {
        let policy = RestartPolicy {
            max_restarts: 3,
            restart_window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(10),
        };
        let mut tracker = RestartTracker::new(policy);

        // First restart: backoff = 100ms * 2^0 = 100ms
        let d1 = tracker.record_restart().unwrap();
        assert_eq!(d1, Duration::from_millis(100));

        // Second restart: backoff = 100ms * 2^1 = 200ms
        let d2 = tracker.record_restart().unwrap();
        assert_eq!(d2, Duration::from_millis(200));

        // Third restart: backoff = 100ms * 2^2 = 400ms
        let d3 = tracker.record_restart().unwrap();
        assert_eq!(d3, Duration::from_millis(400));

        // Fourth restart: should exceed limit (max_restarts = 3)
        let result = tracker.record_restart();
        assert!(result.is_err());
    }

    #[test]
    fn tracker_backoff_caps_at_max() {
        let policy = RestartPolicy {
            max_restarts: 100,
            restart_window: Duration::from_secs(600),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(5),
        };
        let mut tracker = RestartTracker::new(policy);

        // After enough restarts the backoff should cap.
        for _ in 0..10 {
            let d = tracker.record_restart().unwrap();
            assert!(d <= Duration::from_secs(5));
        }
    }

    #[test]
    fn tracker_reset_clears_history() {
        let policy = RestartPolicy {
            max_restarts: 2,
            restart_window: Duration::from_secs(60),
            ..Default::default()
        };
        let mut tracker = RestartTracker::new(policy);

        tracker.record_restart().unwrap();
        tracker.record_restart().unwrap();
        assert!(tracker.record_restart().is_err());

        tracker.reset();
        assert_eq!(tracker.restart_count(), 0);
        assert_eq!(tracker.consecutive_failures(), 0);

        // Should work again after reset.
        assert!(tracker.record_restart().is_ok());
    }

    #[test]
    fn process_state_display() {
        assert_eq!(ProcessState::Running.to_string(), "Running");
        assert_eq!(ProcessState::Failed.to_string(), "Failed");
    }

    #[test]
    fn process_id_serde_roundtrip() {
        let id = ProcessId::Loop1Agentic;
        let json = serde_json::to_string(&id).unwrap();
        let back: ProcessId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn process_state_serde_roundtrip() {
        let state = ProcessState::Restarting;
        let json = serde_json::to_string(&state).unwrap();
        let back: ProcessState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }
}
