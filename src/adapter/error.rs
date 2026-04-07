//! Adapter error types.

use crate::envelope::task_envelope::SecurityZone;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("no provider available for zone {zone:?} with requested capabilities")]
    NoProviderAvailable {
        zone: SecurityZone,
        reason: String,
    },

    #[error("all providers exhausted after retries")]
    AllProvidersExhausted,

    #[error("token budget exceeded: remaining={remaining}, requested={requested}")]
    BudgetExceeded {
        remaining: u64,
        requested: u64,
    },

    #[error("provider error from {provider}: {message}")]
    ProviderError {
        provider: String,
        message: String,
    },

    #[error("request timeout after {timeout_ms}ms")]
    Timeout {
        timeout_ms: u64,
    },

    #[error("circuit open for provider {provider}")]
    CircuitOpen {
        provider: String,
    },

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("configuration error: {0}")]
    Config(String),
}
