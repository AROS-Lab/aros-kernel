//! Adapter request types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::envelope::task_envelope::{Priority, SecurityZone};

use super::LoopOrigin;

/// Capability requirements for model selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityReq {
    /// Minimum context window in tokens.
    pub min_context: u64,
    /// Whether the model must support streaming.
    pub supports_streaming: bool,
    /// Whether the model must support tool use.
    pub tool_use: bool,
    /// Whether the model must support vision.
    pub vision: bool,
    /// Minimum acceptable quality tier.
    pub min_quality_tier: QualityTier,
}

impl Default for CapabilityReq {
    fn default() -> Self {
        Self {
            min_context: 8_000,
            supports_streaming: false,
            tool_use: false,
            vision: false,
            min_quality_tier: QualityTier::Haiku,
        }
    }
}

/// Quality tier for model selection (minimum acceptable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QualityTier {
    Haiku,
    Sonnet,
    Opus,
}

/// Context source attribution for telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSource {
    /// Which memory tier this context came from.
    pub tier: MemoryTier,
    /// Token count for this context chunk.
    pub token_count: u64,
    /// How this context was retrieved.
    pub retrieval_method: Option<String>,
    /// Whether this context can be trimmed under budget pressure.
    pub expendable: bool,
}

/// Memory tier for context attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemoryTier {
    L1Working,
    L2Session,
    L3LongTerm,
    L4ErrorJournal,
}

/// A message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

/// A tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A complete adapter request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterRequest {
    // === Identity ===
    pub request_id: Uuid,
    pub dag_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub loop_origin: LoopOrigin,

    // === Capability Requirements ===
    pub capabilities: CapabilityReq,

    // === Security ===
    pub security_zone: SecurityZone,

    // === Priority ===
    pub priority: Priority,

    // === Budget ===
    pub token_budget_remaining: u64,

    // === Context Attribution ===
    pub context_sources: Vec<ContextSource>,

    // === Adversarial Critique ===
    /// If true, select a different provider than the last one used (for cross-model review).
    pub require_different_provider: bool,

    // === Payload ===
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolDef>>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u64>,
}

impl AdapterRequest {
    /// Create a minimal request for testing.
    pub fn simple(messages: Vec<Message>, priority: Priority, zone: SecurityZone) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            dag_id: None,
            task_id: None,
            loop_origin: LoopOrigin::Loop1Agentic,
            capabilities: CapabilityReq::default(),
            security_zone: zone,
            priority,
            token_budget_remaining: 100_000,
            context_sources: Vec::new(),
            require_different_provider: false,
            messages,
            tools: None,
            temperature: None,
            max_tokens: None,
        }
    }
}
