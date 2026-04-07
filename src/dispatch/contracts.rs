//! Loop trigger contracts — typed interfaces between loops.
//!
//! These define the kernel-mediated triggers that coordinate loop execution:
//! - Loop 0 (Meta) ↔ Kernel: self-improvement triggers
//! - Loop 2 (Harness) → Loop 1 (Agentic): task dispatch
//! - Loop 1 → Kernel: completion/progress signals
//!
//! Uses canonical types from `envelope` and `supervisor` modules.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::task_envelope::TaskEnvelope;
use crate::supervisor::process::ProcessId;

/// Trigger kinds that flow through the kernel event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum TriggerKind {
    // ── Loop 2 → Loop 1 ─────────────────────────────────────────────
    /// Dispatch a task to a new Loop 1 subprocess.
    TaskDispatch(TaskEnvelope),

    // ── Loop 1 → Kernel ─────────────────────────────────────────────
    /// Loop 1 reports incremental progress.
    TaskProgress {
        task_id: String,
        dag_id: String,
        phase: String,
        detail: Option<Value>,
    },

    /// Loop 1 reports successful completion.
    TaskComplete {
        task_id: String,
        dag_id: String,
        output: String,
        tokens_used: u64,
        duration_secs: f64,
    },

    /// Loop 1 reports failure.
    TaskFailed {
        task_id: String,
        dag_id: String,
        error: String,
        retryable: bool,
    },

    // ── Kernel → Loop 1 ─────────────────────────────────────────────
    /// Kernel requests Loop 1 to cancel the current task.
    TaskCancel {
        task_id: String,
        reason: String,
    },

    // ── Loop 0 (Meta) ↔ Kernel ──────────────────────────────────────
    /// Meta Loop requests a self-improvement cycle.
    MetaCycleRequest {
        /// Signal that triggered the cycle (e.g., "schedule", "drift_detected", "manual").
        trigger_source: String,
    },

    /// Kernel acknowledges and authorizes a meta cycle.
    MetaCycleAuthorized {
        cycle_id: String,
    },

    /// Meta Loop completed a cycle; kernel should persist results.
    MetaCycleComplete {
        cycle_id: String,
        policy_changed: bool,
        drift_score: f64,
    },

    // ── Health / System ─────────────────────────────────────────────
    /// Health check ping.
    Ping,

    /// Pong response.
    Pong,
}

/// A loop trigger wraps a TriggerKind with routing metadata.
///
/// Uses `ProcessId` from the supervisor module for source/target identification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopTrigger {
    /// Monotonic sequence number for ordering.
    pub seq: u64,
    /// Source process identifier.
    pub source: ProcessId,
    /// Target process identifier (Kernel if routed through event bus).
    pub target: ProcessId,
    /// The trigger payload.
    pub kind: TriggerKind,
    /// Distributed trace parent for correlation (W3C Trace Context format).
    #[serde(default)]
    pub trace_parent: Option<String>,
}

impl LoopTrigger {
    pub fn new(seq: u64, source: ProcessId, target: ProcessId, kind: TriggerKind) -> Self {
        Self {
            seq,
            source,
            target,
            kind,
            trace_parent: None,
        }
    }

    pub fn with_trace(mut self, trace_parent: String) -> Self {
        self.trace_parent = Some(trace_parent);
        self
    }
}

/// Trait for components that can receive loop triggers.
///
/// The kernel routes triggers to the appropriate handler based on target ProcessId.
pub trait TriggerSink: Send + Sync {
    fn accept(
        &self,
        trigger: LoopTrigger,
    ) -> impl std::future::Future<Output = Result<(), TriggerError>> + Send;
}

#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    #[error("Target process not running: {0:?}")]
    TargetNotRunning(ProcessId),
    #[error("Trigger rejected: {0}")]
    Rejected(String),
    #[error("Dispatch error: {0}")]
    Dispatch(String),
}
