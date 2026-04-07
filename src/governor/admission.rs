/// Result of an admission decision (Phase 1).
///
/// Follows queue -> throttle -> shed ordering under increasing pressure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// Task admitted — proceed with execution.
    Admitted,
    /// Task queued — resources temporarily unavailable, retry later.
    Queued { reason: String },
    /// Task throttled — tier is at capacity, slow down.
    Throttled { reason: String },
    /// Task shed — system under severe pressure, reject entirely.
    Shed { reason: String },
}

/// Result of a runtime budget check (Phase 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeDecision {
    /// Continue executing.
    Continue,
    /// Budget warning — approaching limits.
    Warning { reason: String },
    /// Budget exceeded — task should wrap up.
    Exceeded { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admission_decision_eq() {
        assert_eq!(AdmissionDecision::Admitted, AdmissionDecision::Admitted);
        assert_ne!(
            AdmissionDecision::Admitted,
            AdmissionDecision::Queued {
                reason: "test".into()
            }
        );
    }

    #[test]
    fn test_runtime_decision_eq() {
        assert_eq!(RuntimeDecision::Continue, RuntimeDecision::Continue);
        assert_ne!(
            RuntimeDecision::Continue,
            RuntimeDecision::Warning {
                reason: "test".into()
            }
        );
    }

    #[test]
    fn test_admission_decision_debug() {
        let shed = AdmissionDecision::Shed {
            reason: "pressure".into(),
        };
        let debug_str = format!("{:?}", shed);
        assert!(debug_str.contains("pressure"));
    }

    #[test]
    fn test_runtime_decision_debug() {
        let exceeded = RuntimeDecision::Exceeded {
            reason: "over budget".into(),
        };
        let debug_str = format!("{:?}", exceeded);
        assert!(debug_str.contains("over budget"));
    }
}
