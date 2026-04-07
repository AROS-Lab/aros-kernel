//! Adapter response types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::circuit_breaker::CircuitState;
use super::request::MemoryTier;
use super::DegradationLevel;

/// A tool call returned by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Budget advisory returned with the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetAdvisory {
    pub budget_remaining: u64,
    pub recommended_tier_cuts: Vec<TierCut>,
}

/// Recommendation to trim a specific memory tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierCut {
    pub tier: MemoryTier,
    pub suggested_reduction_tokens: u64,
}

/// A complete adapter response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterResponse {
    pub request_id: Uuid,

    // === Resolution ===
    pub provider: String,
    pub model: String,
    pub degradation_level: DegradationLevel,

    // === Usage ===
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub latency_ms: u64,

    // === Content ===
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,

    // === Budget Advisory ===
    pub budget_advisory: Option<BudgetAdvisory>,

    // === Telemetry ===
    pub retry_count: u32,
    pub circuit_state: CircuitState,
}
