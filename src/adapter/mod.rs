//! Model Adapter Sidecar — unified interface for all LLM interactions.
//!
//! Provides capability-based model resolution, circuit breaking, priority queuing,
//! and token budget enforcement. Runs as a supervised sidecar process managed by
//! the kernel's init supervisor.

pub mod circuit_breaker;
pub mod config;
pub mod error;
pub mod provider;
pub mod request;
pub mod response;
pub mod resolver;

use crate::envelope::task_envelope::Priority;

pub use error::AdapterError;
pub use request::AdapterRequest;
pub use response::AdapterResponse;

/// Core trait for the model adapter interface.
/// All loop-level code interacts with the adapter through this trait.
pub trait ModelAdapter: Send + Sync {
    /// Send a completion request to the best available model.
    fn complete(
        &self,
        req: AdapterRequest,
    ) -> impl std::future::Future<Output = Result<AdapterResponse, AdapterError>> + Send;

    /// Query current adapter health and circuit breaker states.
    fn health(&self) -> impl std::future::Future<Output = AdapterHealth> + Send;

    /// Query remaining token budget for a given scope.
    fn budget(&self, scope: BudgetScope) -> impl std::future::Future<Output = TokenBudget> + Send;
}

/// Adapter health summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdapterHealth {
    /// Per-provider circuit breaker states.
    pub providers: Vec<ProviderHealth>,
    /// Overall adapter status.
    pub status: AdapterStatus,
}

/// Health of a single provider.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderHealth {
    pub provider_id: String,
    pub circuit_state: circuit_breaker::CircuitState,
    pub latency_avg_ms: u64,
}

/// Overall adapter status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AdapterStatus {
    Healthy,
    Degraded,
    AllProvidersDown,
}

/// Scope for budget queries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BudgetScope {
    pub priority: Priority,
    pub loop_origin: LoopOrigin,
}

/// Token budget remaining.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenBudget {
    pub remaining: u64,
    pub total: u64,
    pub priority: Priority,
}

/// Which loop originated a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoopOrigin {
    Loop0Meta,
    Loop1Agentic,
    Loop2Harness,
}

/// Level of model degradation in the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DegradationLevel {
    /// Primary model used.
    None,
    /// Secondary model (similar capability tier).
    Mild,
    /// Tertiary/fallback (lower capability tier).
    Significant,
}
