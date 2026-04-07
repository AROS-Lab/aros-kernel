//! Per-provider circuit breaker: Closed → Open → HalfOpen → Closed.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Circuit breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CircuitState {
    /// Healthy — requests flow normally.
    Closed,
    /// Probing — allow a limited number of requests to test recovery.
    HalfOpen,
    /// Unhealthy — skip this provider.
    Open,
}

/// Configuration for a circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before transitioning to Open.
    pub failure_threshold: u32,
    /// Duration to wait before transitioning from Open to HalfOpen.
    pub open_duration: Duration,
    /// Number of successful probes required to transition from HalfOpen to Closed.
    pub probe_success_count: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            open_duration: Duration::from_secs(30),
            probe_success_count: 2,
        }
    }
}

/// Per-provider circuit breaker.
#[derive(Debug)]
pub struct CircuitBreaker {
    provider_id: String,
    state: CircuitState,
    config: CircuitBreakerConfig,
    consecutive_failures: u32,
    consecutive_successes: u32,
    last_state_change: Instant,
}

impl CircuitBreaker {
    /// Create a new circuit breaker in the Closed state.
    pub fn new(provider_id: impl Into<String>, config: CircuitBreakerConfig) -> Self {
        Self {
            provider_id: provider_id.into(),
            state: CircuitState::Closed,
            config,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_state_change: Instant::now(),
        }
    }

    /// Create a circuit breaker recovering from a restart (starts HalfOpen).
    pub fn recovering(provider_id: impl Into<String>, config: CircuitBreakerConfig) -> Self {
        Self {
            provider_id: provider_id.into(),
            state: CircuitState::HalfOpen,
            config,
            consecutive_failures: 0,
            consecutive_successes: 0,
            last_state_change: Instant::now(),
        }
    }

    /// Get the current state, automatically transitioning Open → HalfOpen if duration elapsed.
    pub fn state(&mut self) -> CircuitState {
        if self.state == CircuitState::Open
            && self.last_state_change.elapsed() >= self.config.open_duration
        {
            self.transition(CircuitState::HalfOpen);
        }
        self.state
    }

    /// Get the provider ID.
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// Record a successful request.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        match self.state {
            CircuitState::Closed => {
                // Stay closed.
            }
            CircuitState::HalfOpen => {
                self.consecutive_successes += 1;
                if self.consecutive_successes >= self.config.probe_success_count {
                    self.transition(CircuitState::Closed);
                }
            }
            CircuitState::Open => {
                // Shouldn't happen — requests shouldn't reach an open circuit.
            }
        }
    }

    /// Record a failed request.
    pub fn record_failure(&mut self) {
        self.consecutive_successes = 0;
        self.consecutive_failures += 1;
        match self.state {
            CircuitState::Closed => {
                if self.consecutive_failures >= self.config.failure_threshold {
                    self.transition(CircuitState::Open);
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in HalfOpen → back to Open.
                self.transition(CircuitState::Open);
            }
            CircuitState::Open => {
                // Already open.
            }
        }
    }

    /// Whether requests should be allowed through.
    pub fn allows_request(&mut self) -> bool {
        match self.state() {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => false,
        }
    }

    fn transition(&mut self, new_state: CircuitState) {
        self.state = new_state;
        self.last_state_change = Instant::now();
        self.consecutive_failures = 0;
        self.consecutive_successes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_starts_closed() {
        let mut cb = CircuitBreaker::new("test", CircuitBreakerConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allows_request());
    }

    #[test]
    fn test_opens_after_threshold_failures() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let mut cb = CircuitBreaker::new("test", config);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allows_request());
    }

    #[test]
    fn test_success_resets_failure_count() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let mut cb = CircuitBreaker::new("test", config);

        cb.record_failure();
        cb.record_failure();
        cb.record_success(); // Reset
        cb.record_failure();
        // Only 1 consecutive failure now, should still be closed
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_success_closes() {
        let config = CircuitBreakerConfig {
            probe_success_count: 2,
            ..Default::default()
        };
        let mut cb = CircuitBreaker::recovering("test", config);
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_half_open_failure_reopens() {
        let mut cb = CircuitBreaker::recovering("test", CircuitBreakerConfig::default());

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_open_transitions_to_half_open_after_duration() {
        let config = CircuitBreakerConfig {
            failure_threshold: 1,
            open_duration: Duration::from_millis(0), // Immediate for testing
            ..Default::default()
        };
        let mut cb = CircuitBreaker::new("test", config);

        cb.record_failure(); // → Open
        assert_eq!(cb.state(), CircuitState::HalfOpen); // Immediate transition
    }

    #[test]
    fn test_recovering_starts_half_open() {
        let mut cb = CircuitBreaker::recovering("test", CircuitBreakerConfig::default());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }
}
