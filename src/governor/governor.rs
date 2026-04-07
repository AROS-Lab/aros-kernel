use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::envelope::task_envelope::Priority;
use crate::hardware::pressure::MemoryPressureLevel;

use super::admission::{AdmissionDecision, RuntimeDecision};
use super::budget::{TierBudget, TierUsage};

/// Configuration for the resource governor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernorConfig {
    /// System-wide RSS ceiling (MB).
    pub system_rss_ceiling_mb: u32,
    /// Reserved headroom (MB) — never allocate into this.
    pub headroom_mb: u32,
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            system_rss_ceiling_mb: 10_240, // 10 GB for 16GB Mac Mini
            headroom_mb: 2_048,            // 2 GB headroom
        }
    }
}

/// A snapshot of usage for a single priority tier (for telemetry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub active_tasks: u64,
    pub tokens_used_this_hour: u64,
    pub rss_allocated_mb: u64,
    pub budget: TierBudget,
}

/// The resource governor — manages admission, runtime budgets, and pressure response.
///
/// Sits above the lower-level `AdmissionController` and `ResourceAllocator`,
/// adding priority-aware budget enforcement and two-phase checks.
pub struct ResourceGovernor {
    pub(crate) tier_budgets: HashMap<Priority, TierBudget>,
    pub(crate) tier_usage: HashMap<Priority, Arc<TierUsage>>,
    pub(crate) pressure_level: Arc<RwLock<MemoryPressureLevel>>,
    pub(crate) config: GovernorConfig,
}

impl ResourceGovernor {
    /// Create a new governor with default per-tier budgets.
    pub fn new(config: GovernorConfig) -> Self {
        let mut tier_budgets = HashMap::new();
        tier_budgets.insert(Priority::P0Critical, TierBudget::p0_critical());
        tier_budgets.insert(Priority::P1Normal, TierBudget::p1_normal());
        tier_budgets.insert(Priority::P2Background, TierBudget::p2_background());

        let mut tier_usage = HashMap::new();
        tier_usage.insert(Priority::P0Critical, Arc::new(TierUsage::new()));
        tier_usage.insert(Priority::P1Normal, Arc::new(TierUsage::new()));
        tier_usage.insert(Priority::P2Background, Arc::new(TierUsage::new()));

        Self {
            tier_budgets,
            tier_usage,
            pressure_level: Arc::new(RwLock::new(MemoryPressureLevel::Normal)),
            config,
        }
    }

    /// Update the current memory pressure level.
    pub async fn update_pressure(&self, level: MemoryPressureLevel) {
        let mut w = self.pressure_level.write().await;
        *w = level;
    }

    /// Read the current memory pressure level.
    pub async fn current_pressure(&self) -> MemoryPressureLevel {
        *self.pressure_level.read().await
    }

    /// Phase 1: Admission check — can this task start?
    pub async fn check_admission(
        &self,
        priority: Priority,
        rss_mb: u32,
        tokens_estimate: u64,
    ) -> AdmissionDecision {
        let pressure = self.current_pressure().await;
        let budget = match self.tier_budgets.get(&priority) {
            Some(b) => b,
            None => {
                return AdmissionDecision::Shed {
                    reason: "unknown priority tier".into(),
                }
            }
        };
        let usage = match self.tier_usage.get(&priority) {
            Some(u) => u,
            None => {
                return AdmissionDecision::Shed {
                    reason: "unknown priority tier".into(),
                }
            }
        };

        // 1. Under critical pressure, shed sheddable tiers
        if pressure == MemoryPressureLevel::Critical && budget.sheddable {
            return AdmissionDecision::Shed {
                reason: "system under critical pressure; sheddable tier".into(),
            };
        }

        // 2. Check concurrent task limit
        let active = usage
            .active_tasks
            .load(std::sync::atomic::Ordering::SeqCst);
        if active >= budget.max_concurrent as u64 {
            return AdmissionDecision::Queued {
                reason: format!(
                    "tier concurrent limit reached ({}/{})",
                    active, budget.max_concurrent
                ),
            };
        }

        // 3. Check token budget
        let tokens_used = usage
            .tokens_used_this_hour
            .load(std::sync::atomic::Ordering::SeqCst);
        if tokens_used.saturating_add(tokens_estimate) > budget.max_tokens_per_hour {
            if budget.sheddable {
                return AdmissionDecision::Shed {
                    reason: format!(
                        "token budget exhausted ({}/{}); sheddable tier",
                        tokens_used, budget.max_tokens_per_hour
                    ),
                };
            } else {
                return AdmissionDecision::Throttled {
                    reason: format!(
                        "token budget exhausted ({}/{})",
                        tokens_used, budget.max_tokens_per_hour
                    ),
                };
            }
        }

        // 4. Check RSS limit
        let rss_used = usage
            .rss_allocated_mb
            .load(std::sync::atomic::Ordering::SeqCst);
        if rss_used + rss_mb as u64 > budget.max_rss_mb as u64 {
            return AdmissionDecision::Queued {
                reason: format!(
                    "tier RSS limit reached ({} + {} > {} MB)",
                    rss_used, rss_mb, budget.max_rss_mb
                ),
            };
        }

        // 5. Check system-wide RSS ceiling
        let total_rss: u64 = self
            .tier_usage
            .values()
            .map(|u| u.rss_allocated_mb.load(std::sync::atomic::Ordering::SeqCst))
            .sum();
        let ceiling =
            self.config.system_rss_ceiling_mb as u64 - self.config.headroom_mb as u64;
        if total_rss + rss_mb as u64 > ceiling {
            return AdmissionDecision::Queued {
                reason: format!(
                    "system RSS ceiling would be exceeded ({} + {} > {} MB)",
                    total_rss, rss_mb, ceiling
                ),
            };
        }

        AdmissionDecision::Admitted
    }

    /// Phase 2: Runtime check — is this task still within budget?
    pub async fn check_runtime(
        &self,
        priority: Priority,
        tokens_used: u64,
        _rss_current_mb: u32,
    ) -> RuntimeDecision {
        let budget = match self.tier_budgets.get(&priority) {
            Some(b) => b,
            None => {
                return RuntimeDecision::Exceeded {
                    reason: "unknown priority tier".into(),
                }
            }
        };

        let pressure = self.current_pressure().await;

        // Under critical pressure, non-P0 tasks should wrap up
        if pressure == MemoryPressureLevel::Critical && priority != Priority::P0Critical {
            return RuntimeDecision::Exceeded {
                reason: "system under critical pressure".into(),
            };
        }

        // Check token budget
        if tokens_used > budget.max_tokens_per_hour {
            return RuntimeDecision::Exceeded {
                reason: format!(
                    "token budget exceeded ({}/{})",
                    tokens_used, budget.max_tokens_per_hour
                ),
            };
        }

        // Warning at 80% of token budget
        let warning_threshold = (budget.max_tokens_per_hour as f64 * 0.8) as u64;
        if tokens_used > warning_threshold {
            return RuntimeDecision::Warning {
                reason: format!(
                    "approaching token budget ({}/{})",
                    tokens_used, budget.max_tokens_per_hour
                ),
            };
        }

        // Warning under Warn pressure for non-P0
        if pressure == MemoryPressureLevel::Warn && priority != Priority::P0Critical {
            return RuntimeDecision::Warning {
                reason: "system memory pressure is elevated".into(),
            };
        }

        RuntimeDecision::Continue
    }

    /// Get current usage snapshot for telemetry.
    pub fn usage_snapshot(&self) -> HashMap<Priority, UsageSnapshot> {
        self.tier_usage
            .iter()
            .map(|(priority, usage)| {
                let snapshot = UsageSnapshot {
                    active_tasks: usage
                        .active_tasks
                        .load(std::sync::atomic::Ordering::SeqCst),
                    tokens_used_this_hour: usage
                        .tokens_used_this_hour
                        .load(std::sync::atomic::Ordering::SeqCst),
                    rss_allocated_mb: usage
                        .rss_allocated_mb
                        .load(std::sync::atomic::Ordering::SeqCst),
                    budget: self.tier_budgets[priority].clone(),
                };
                (*priority, snapshot)
            })
            .collect()
    }

    /// Record task start for a priority tier.
    pub fn task_started(&self, priority: Priority, rss_mb: u32) {
        if let Some(usage) = self.tier_usage.get(&priority) {
            usage.record_task_start(rss_mb as u64);
        }
    }

    /// Record task end for a priority tier.
    pub fn task_ended(&self, priority: Priority, rss_mb: u32) {
        if let Some(usage) = self.tier_usage.get(&priority) {
            usage.record_task_end(rss_mb as u64);
        }
    }

    /// Record token usage for a priority tier.
    pub fn tokens_used(&self, priority: Priority, count: u64) {
        if let Some(usage) = self.tier_usage.get(&priority) {
            usage.record_tokens(count);
        }
    }

    /// Reset hourly token counters across all tiers.
    pub fn reset_hourly_tokens(&self) {
        for usage in self.tier_usage.values() {
            usage.reset_hourly_tokens();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_governor() -> ResourceGovernor {
        ResourceGovernor::new(GovernorConfig::default())
    }

    #[tokio::test]
    async fn test_p0_admitted_under_critical_pressure() {
        let gov = make_governor();
        gov.update_pressure(MemoryPressureLevel::Critical).await;

        let decision = gov.check_admission(Priority::P0Critical, 100, 1000).await;
        assert_eq!(decision, AdmissionDecision::Admitted);
    }

    #[tokio::test]
    async fn test_p2_shed_under_critical_pressure() {
        let gov = make_governor();
        gov.update_pressure(MemoryPressureLevel::Critical).await;

        let decision = gov
            .check_admission(Priority::P2Background, 100, 1000)
            .await;
        assert!(matches!(decision, AdmissionDecision::Shed { .. }));
    }

    #[tokio::test]
    async fn test_p1_admitted_under_critical_pressure() {
        let gov = make_governor();
        gov.update_pressure(MemoryPressureLevel::Critical).await;

        // P1 is not sheddable, so it should still be admitted under critical pressure
        let decision = gov.check_admission(Priority::P1Normal, 100, 1000).await;
        assert_eq!(decision, AdmissionDecision::Admitted);
    }

    #[tokio::test]
    async fn test_admission_normal_pressure() {
        let gov = make_governor();

        let decision = gov
            .check_admission(Priority::P1Normal, 100, 1000)
            .await;
        assert_eq!(decision, AdmissionDecision::Admitted);
    }

    #[tokio::test]
    async fn test_concurrent_limit_queued() {
        let gov = make_governor();
        // P0 has max_concurrent = 2
        gov.task_started(Priority::P0Critical, 100);
        gov.task_started(Priority::P0Critical, 100);

        let decision = gov
            .check_admission(Priority::P0Critical, 100, 1000)
            .await;
        assert!(matches!(decision, AdmissionDecision::Queued { .. }));
    }

    #[tokio::test]
    async fn test_token_budget_throttle_p1() {
        let gov = make_governor();
        // P1 max_tokens_per_hour = 500_000. Exhaust it.
        gov.tokens_used(Priority::P1Normal, 500_000);

        let decision = gov
            .check_admission(Priority::P1Normal, 100, 1000)
            .await;
        assert!(matches!(decision, AdmissionDecision::Throttled { .. }));
    }

    #[tokio::test]
    async fn test_token_budget_shed_p2() {
        let gov = make_governor();
        // P2 max_tokens_per_hour = 200_000. Exhaust it.
        gov.tokens_used(Priority::P2Background, 200_000);

        let decision = gov
            .check_admission(Priority::P2Background, 100, 1000)
            .await;
        assert!(matches!(decision, AdmissionDecision::Shed { .. }));
    }

    #[tokio::test]
    async fn test_rss_limit_queued() {
        let gov = make_governor();
        // P0 max_rss_mb = 512. Allocate most of it.
        gov.task_started(Priority::P0Critical, 500);

        let decision = gov
            .check_admission(Priority::P0Critical, 100, 1000)
            .await;
        assert!(matches!(decision, AdmissionDecision::Queued { .. }));
    }

    #[tokio::test]
    async fn test_task_lifecycle_tracking() {
        let gov = make_governor();

        gov.task_started(Priority::P1Normal, 256);
        gov.task_started(Priority::P1Normal, 128);
        gov.tokens_used(Priority::P1Normal, 5000);

        let snapshot = gov.usage_snapshot();
        let p1 = &snapshot[&Priority::P1Normal];
        assert_eq!(p1.active_tasks, 2);
        assert_eq!(p1.rss_allocated_mb, 384);
        assert_eq!(p1.tokens_used_this_hour, 5000);

        gov.task_ended(Priority::P1Normal, 256);
        let snapshot = gov.usage_snapshot();
        let p1 = &snapshot[&Priority::P1Normal];
        assert_eq!(p1.active_tasks, 1);
        assert_eq!(p1.rss_allocated_mb, 128);
    }

    #[tokio::test]
    async fn test_usage_snapshot_all_tiers() {
        let gov = make_governor();
        let snapshot = gov.usage_snapshot();

        assert!(snapshot.contains_key(&Priority::P0Critical));
        assert!(snapshot.contains_key(&Priority::P1Normal));
        assert!(snapshot.contains_key(&Priority::P2Background));

        // All start at zero
        for (_, s) in &snapshot {
            assert_eq!(s.active_tasks, 0);
            assert_eq!(s.tokens_used_this_hour, 0);
            assert_eq!(s.rss_allocated_mb, 0);
        }
    }

    #[tokio::test]
    async fn test_runtime_continue() {
        let gov = make_governor();
        let decision = gov.check_runtime(Priority::P1Normal, 1000, 100).await;
        assert_eq!(decision, RuntimeDecision::Continue);
    }

    #[tokio::test]
    async fn test_runtime_warning_at_80_percent() {
        let gov = make_governor();
        // P1 max = 500_000. 80% = 400_000. Use 410_000.
        let decision = gov
            .check_runtime(Priority::P1Normal, 410_000, 100)
            .await;
        assert!(matches!(decision, RuntimeDecision::Warning { .. }));
    }

    #[tokio::test]
    async fn test_runtime_exceeded() {
        let gov = make_governor();
        // P1 max = 500_000. Use 600_000.
        let decision = gov
            .check_runtime(Priority::P1Normal, 600_000, 100)
            .await;
        assert!(matches!(decision, RuntimeDecision::Exceeded { .. }));
    }

    #[tokio::test]
    async fn test_runtime_critical_pressure_non_p0() {
        let gov = make_governor();
        gov.update_pressure(MemoryPressureLevel::Critical).await;

        let p1 = gov.check_runtime(Priority::P1Normal, 100, 50).await;
        assert!(matches!(p1, RuntimeDecision::Exceeded { .. }));

        let p2 = gov.check_runtime(Priority::P2Background, 100, 50).await;
        assert!(matches!(p2, RuntimeDecision::Exceeded { .. }));

        // P0 should continue
        let p0 = gov.check_runtime(Priority::P0Critical, 100, 50).await;
        assert_eq!(p0, RuntimeDecision::Continue);
    }

    #[tokio::test]
    async fn test_runtime_warn_pressure_non_p0() {
        let gov = make_governor();
        gov.update_pressure(MemoryPressureLevel::Warn).await;

        let p1 = gov.check_runtime(Priority::P1Normal, 100, 50).await;
        assert!(matches!(p1, RuntimeDecision::Warning { .. }));

        // P0 should continue even under warn pressure
        let p0 = gov.check_runtime(Priority::P0Critical, 100, 50).await;
        assert_eq!(p0, RuntimeDecision::Continue);
    }

    #[tokio::test]
    async fn test_reset_hourly_tokens() {
        let gov = make_governor();
        gov.tokens_used(Priority::P0Critical, 10_000);
        gov.tokens_used(Priority::P1Normal, 50_000);
        gov.tokens_used(Priority::P2Background, 20_000);

        gov.reset_hourly_tokens();

        let snapshot = gov.usage_snapshot();
        for (_, s) in &snapshot {
            assert_eq!(s.tokens_used_this_hour, 0);
        }
    }

    #[tokio::test]
    async fn test_system_rss_ceiling() {
        // Use a very small ceiling to trigger the check
        let config = GovernorConfig {
            system_rss_ceiling_mb: 500,
            headroom_mb: 100,
        };
        let gov = ResourceGovernor::new(config);

        // Effective ceiling = 500 - 100 = 400 MB
        // Allocate 350 MB to P1
        gov.task_started(Priority::P1Normal, 350);

        // Trying to add 100 more should exceed 400 ceiling
        let decision = gov
            .check_admission(Priority::P1Normal, 100, 100)
            .await;
        assert!(matches!(decision, AdmissionDecision::Queued { .. }));
    }

    #[test]
    fn test_governor_config_default() {
        let config = GovernorConfig::default();
        assert_eq!(config.system_rss_ceiling_mb, 10_240);
        assert_eq!(config.headroom_mb, 2_048);
    }
}
