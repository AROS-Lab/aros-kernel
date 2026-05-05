//! Supervisor integration tests targeting documented gaps in
//! `tests/GAP_ANALYSIS.md` §3: terminal `Failed` state persistence,
//! restart-window edge cases, and repeated max-restarts semantics.
//!
//! These complement the inline tests in `src/supervisor/` by exercising
//! the public API at the integration boundary.

use std::time::Duration;

use aros_kernel::supervisor::error::SupervisorError;
use aros_kernel::supervisor::kernel::KernelSupervisor;
use aros_kernel::supervisor::process::{
    ProcessId, ProcessState, RestartPolicy, RestartTracker,
};

fn fast_policy(max_restarts: u32) -> RestartPolicy {
    RestartPolicy {
        max_restarts,
        restart_window: Duration::from_secs(60),
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(10),
    }
}

#[tokio::test]
async fn failed_state_persists_after_max_restarts_exceeded() {
    let sv = KernelSupervisor::new();
    sv.register(ProcessId::ModelAdapter, fast_policy(1)).await;

    sv.update_state(ProcessId::ModelAdapter, ProcessState::Running)
        .await
        .unwrap();

    // First failure consumes the only allowed restart.
    sv.update_state(ProcessId::ModelAdapter, ProcessState::Failed)
        .await
        .unwrap();

    // Second failure exceeds the limit and should bubble an error.
    let err = sv
        .update_state(ProcessId::ModelAdapter, ProcessState::Failed)
        .await
        .expect_err("max restarts must error");
    assert!(matches!(
        err,
        SupervisorError::MaxRestartsExceeded(ProcessId::ModelAdapter)
    ));

    // Health snapshot must still report ModelAdapter as Failed
    // (i.e. the supervisor did not silently revert state on error).
    let health = sv.health().await;
    let process = health
        .active_processes
        .iter()
        .find(|p| p.id == ProcessId::ModelAdapter)
        .expect("ModelAdapter should still be tracked");
    assert_eq!(
        process.state,
        ProcessState::Failed,
        "process must remain in terminal Failed state after max restarts"
    );
}

#[tokio::test]
async fn repeated_failure_after_terminal_state_returns_same_error() {
    let sv = KernelSupervisor::new();
    sv.register(ProcessId::EmbeddingAdapter, fast_policy(1))
        .await;
    sv.update_state(ProcessId::EmbeddingAdapter, ProcessState::Running)
        .await
        .unwrap();

    // Drive into terminal Failed state.
    sv.update_state(ProcessId::EmbeddingAdapter, ProcessState::Failed)
        .await
        .unwrap();
    let _ = sv
        .update_state(ProcessId::EmbeddingAdapter, ProcessState::Failed)
        .await;

    // A third failure must still yield MaxRestartsExceeded — supervisor
    // should not "leak" allowed restarts on repeated terminal calls.
    let err = sv
        .update_state(ProcessId::EmbeddingAdapter, ProcessState::Failed)
        .await
        .expect_err("repeated failure must keep erroring");
    assert!(matches!(
        err,
        SupervisorError::MaxRestartsExceeded(ProcessId::EmbeddingAdapter)
    ));
}

#[test]
fn restart_tracker_zero_max_restarts_immediately_fails() {
    // Boundary: a policy with max_restarts == 0 must reject the very
    // first restart attempt — the kernel should never silently allow one.
    let policy = RestartPolicy {
        max_restarts: 0,
        restart_window: Duration::from_secs(60),
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(10),
    };
    let mut tracker = RestartTracker::new(policy);
    assert!(tracker.record_restart().is_err());
    assert_eq!(tracker.restart_count(), 0);
}

#[test]
fn restart_tracker_window_eviction_allows_new_restarts() {
    // After the restart_window elapses, old entries must be evicted so
    // that new restarts are once again permitted.
    let policy = RestartPolicy {
        max_restarts: 2,
        restart_window: Duration::from_millis(50),
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(10),
    };
    let mut tracker = RestartTracker::new(policy);

    tracker.record_restart().unwrap();
    tracker.record_restart().unwrap();
    assert!(tracker.record_restart().is_err()); // window full

    std::thread::sleep(Duration::from_millis(70));

    // After the window expires, restarts should be possible again.
    assert!(
        tracker.record_restart().is_ok(),
        "restart_window must evict old entries"
    );
}

#[test]
fn restart_tracker_consecutive_failures_persist_through_window_eviction() {
    // consecutive_failures is *not* tied to the restart_window — only
    // an explicit reset() clears it. This invariant guards against
    // backoff getting reset by elapsed wall-clock time alone.
    let policy = RestartPolicy {
        max_restarts: 10,
        restart_window: Duration::from_millis(20),
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(100),
    };
    let mut tracker = RestartTracker::new(policy);

    for _ in 0..3 {
        tracker.record_restart().unwrap();
        std::thread::sleep(Duration::from_millis(30)); // exceed window
    }

    assert_eq!(
        tracker.consecutive_failures(),
        3,
        "consecutive_failures must persist across window eviction"
    );
}

#[tokio::test]
async fn unknown_process_state_update_returns_process_not_found() {
    let sv = KernelSupervisor::new();
    let err = sv
        .update_state(ProcessId::Loop0Meta, ProcessState::Running)
        .await
        .expect_err("unregistered process must error");
    assert!(matches!(
        err,
        SupervisorError::ProcessNotFound(ProcessId::Loop0Meta)
    ));
}

#[tokio::test]
async fn running_to_running_resets_restart_tracker() {
    // Once a process re-enters Running after a Restarting cycle, its
    // restart_tracker must reset so subsequent failures don't carry
    // forward count from a previous restart episode.
    let sv = KernelSupervisor::new();
    sv.register(ProcessId::Loop1Agentic, fast_policy(2)).await;
    sv.update_state(ProcessId::Loop1Agentic, ProcessState::Running)
        .await
        .unwrap();

    // Fail once → Restarting
    sv.update_state(ProcessId::Loop1Agentic, ProcessState::Failed)
        .await
        .unwrap();

    // Recover → Running should reset the tracker
    sv.update_state(ProcessId::Loop1Agentic, ProcessState::Running)
        .await
        .unwrap();

    // We should now be able to fail twice more (the full max_restarts=2
    // budget) without exceeding the limit.
    sv.update_state(ProcessId::Loop1Agentic, ProcessState::Failed)
        .await
        .unwrap();
    sv.update_state(ProcessId::Loop1Agentic, ProcessState::Running)
        .await
        .unwrap();
    sv.update_state(ProcessId::Loop1Agentic, ProcessState::Failed)
        .await
        .expect("post-recovery failures must not exceed reset budget");
}

#[tokio::test]
async fn all_healthy_false_when_any_process_failed() {
    // Even one Failed process must block all_healthy() from returning
    // true — this guards the kernel readiness gate.
    let sv = KernelSupervisor::new();
    sv.register(ProcessId::Kernel, RestartPolicy::default())
        .await;
    sv.register(ProcessId::Loop0Meta, fast_policy(0)).await;

    sv.update_state(ProcessId::Kernel, ProcessState::Running)
        .await
        .unwrap();
    sv.update_state(ProcessId::Loop0Meta, ProcessState::Running)
        .await
        .unwrap();
    // Drive Loop0Meta into Failed (zero-restart policy → terminal on first failure).
    let _ = sv
        .update_state(ProcessId::Loop0Meta, ProcessState::Failed)
        .await;

    assert!(
        !sv.all_healthy().await,
        "all_healthy must be false when any process is non-Running"
    );
}
