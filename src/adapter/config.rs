//! Adapter configuration types (maps to TOML config format from spec).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::envelope::task_envelope::SecurityZone;

use super::request::QualityTier;

/// Configuration for a single model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique provider identifier (e.g., "anthropic", "ollama-local").
    pub id: String,
    /// API endpoint URL.
    pub endpoint: String,
    /// Available models at this provider.
    pub models: Vec<String>,
    /// Rank in the fallback chain (lower = preferred).
    pub fallback_rank: u32,
    /// Which security zones this provider is allowed in.
    pub zone_allowlist: Vec<SecurityZone>,
    /// Provider capabilities.
    pub capabilities: ProviderCapabilities,
}

/// What a provider can do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub max_context: u64,
    pub tool_use: bool,
    pub vision: bool,
    pub streaming: bool,
    /// Highest quality tier available at this provider.
    pub max_quality_tier: QualityTier,
}

/// Retry configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_retries_per_provider: u32,
    pub base_delay: Duration,
    pub jitter_max: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries_per_provider: 3,
            base_delay: Duration::from_millis(500),
            jitter_max: Duration::from_millis(200),
        }
    }
}

/// Budget configuration for priority tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Reserved tokens for P0 (never lendable).
    pub p0_reserved_tokens: u64,
    /// Main pool for P1.
    pub p1_pool_tokens: u64,
    /// P2 uses spare capacity only.
    pub p2_spare_only: bool,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            p0_reserved_tokens: 50_000,
            p1_pool_tokens: 500_000,
            p2_spare_only: true,
        }
    }
}

/// Complete adapter configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterConfig {
    /// Path to the Unix Domain Socket.
    pub socket_path: String,
    /// Configured providers.
    pub providers: Vec<ProviderConfig>,
    /// Retry settings.
    pub retry: RetryConfig,
    /// Budget settings.
    pub budget: BudgetConfig,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            socket_path: "/run/aros/adapter.sock".to_string(),
            providers: vec![],
            retry: RetryConfig::default(),
            budget: BudgetConfig::default(),
        }
    }
}
