//! Provider registry — tracks providers and their circuit breakers.

use std::collections::HashMap;

use super::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use super::config::ProviderConfig;
use super::{AdapterHealth, AdapterStatus, ProviderHealth};

/// Registry of all configured providers with their circuit breakers.
pub struct ProviderRegistry {
    providers: Vec<ProviderConfig>,
    circuit_breakers: HashMap<String, CircuitBreaker>,
}

impl ProviderRegistry {
    /// Create a new registry from provider configs.
    pub fn new(providers: Vec<ProviderConfig>, cb_config: CircuitBreakerConfig) -> Self {
        let circuit_breakers = providers
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    CircuitBreaker::new(p.id.clone(), cb_config.clone()),
                )
            })
            .collect();

        Self {
            providers,
            circuit_breakers,
        }
    }

    /// Create a registry recovering from a restart (all breakers start HalfOpen).
    pub fn recovering(providers: Vec<ProviderConfig>, cb_config: CircuitBreakerConfig) -> Self {
        let circuit_breakers = providers
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    CircuitBreaker::recovering(p.id.clone(), cb_config.clone()),
                )
            })
            .collect();

        Self {
            providers,
            circuit_breakers,
        }
    }

    /// Get all provider configs.
    pub fn providers(&self) -> &[ProviderConfig] {
        &self.providers
    }

    /// Get the circuit breaker for a provider.
    pub fn circuit_breaker(&mut self, provider_id: &str) -> Option<&mut CircuitBreaker> {
        self.circuit_breakers.get_mut(provider_id)
    }

    /// Check if a provider's circuit allows requests.
    pub fn is_available(&mut self, provider_id: &str) -> bool {
        self.circuit_breakers
            .get_mut(provider_id)
            .is_some_and(|cb| cb.allows_request())
    }

    /// Record a success for a provider.
    pub fn record_success(&mut self, provider_id: &str) {
        if let Some(cb) = self.circuit_breakers.get_mut(provider_id) {
            cb.record_success();
        }
    }

    /// Record a failure for a provider.
    pub fn record_failure(&mut self, provider_id: &str) {
        if let Some(cb) = self.circuit_breakers.get_mut(provider_id) {
            cb.record_failure();
        }
    }

    /// Get health summary.
    pub fn health(&mut self) -> AdapterHealth {
        let providers: Vec<ProviderHealth> = self
            .circuit_breakers
            .iter_mut()
            .map(|(id, cb)| ProviderHealth {
                provider_id: id.clone(),
                circuit_state: cb.state(),
                latency_avg_ms: 0, // TODO: track running average
            })
            .collect();

        let all_open = providers
            .iter()
            .all(|p| p.circuit_state == CircuitState::Open);
        let any_open = providers
            .iter()
            .any(|p| p.circuit_state == CircuitState::Open);

        let status = if all_open {
            AdapterStatus::AllProvidersDown
        } else if any_open {
            AdapterStatus::Degraded
        } else {
            AdapterStatus::Healthy
        };

        AdapterHealth { providers, status }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::config::ProviderCapabilities;
    use crate::adapter::request::QualityTier;
    use crate::envelope::task_envelope::SecurityZone;

    fn test_provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            endpoint: format!("https://{}.example.com", id),
            models: vec!["model-1".to_string()],
            fallback_rank: 1,
            zone_allowlist: vec![SecurityZone::Green],
            capabilities: ProviderCapabilities {
                max_context: 100_000,
                tool_use: true,
                vision: false,
                streaming: true,
                max_quality_tier: QualityTier::Opus,
            },
        }
    }

    #[test]
    fn test_new_registry_all_closed() {
        let mut registry = ProviderRegistry::new(
            vec![test_provider("a"), test_provider("b")],
            CircuitBreakerConfig::default(),
        );
        assert!(registry.is_available("a"));
        assert!(registry.is_available("b"));
    }

    #[test]
    fn test_recovering_registry_all_half_open() {
        let mut registry = ProviderRegistry::recovering(
            vec![test_provider("a")],
            CircuitBreakerConfig::default(),
        );
        let health = registry.health();
        assert_eq!(health.providers[0].circuit_state, CircuitState::HalfOpen);
    }

    #[test]
    fn test_failure_opens_circuit() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let mut registry = ProviderRegistry::new(vec![test_provider("a")], config);

        registry.record_failure("a");
        assert!(registry.is_available("a"));

        registry.record_failure("a");
        assert!(!registry.is_available("a"));
    }

    #[test]
    fn test_health_degraded() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            ..Default::default()
        };
        let mut registry =
            ProviderRegistry::new(vec![test_provider("a"), test_provider("b")], config);

        registry.record_failure("a"); // Opens a
        let health = registry.health();
        assert_eq!(health.status, AdapterStatus::Degraded);
    }

    #[test]
    fn test_health_all_down() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: std::time::Duration::from_secs(9999),
            ..Default::default()
        };
        let mut registry =
            ProviderRegistry::new(vec![test_provider("a"), test_provider("b")], config);

        registry.record_failure("a");
        registry.record_failure("b");
        let health = registry.health();
        assert_eq!(health.status, AdapterStatus::AllProvidersDown);
    }
}
