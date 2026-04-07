use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Budget allocation per priority tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierBudget {
    /// Maximum concurrent tasks at this priority.
    pub max_concurrent: u32,
    /// Maximum total tokens per hour.
    pub max_tokens_per_hour: u64,
    /// Maximum RSS (MB) allocated to this tier.
    pub max_rss_mb: u32,
    /// Whether this tier can be shed under pressure.
    pub sheddable: bool,
}

impl TierBudget {
    /// Budget for P0 critical tasks — small footprint, never shed.
    pub fn p0_critical() -> Self {
        Self {
            max_concurrent: 2,
            max_tokens_per_hour: 50_000,
            max_rss_mb: 512,
            sheddable: false,
        }
    }

    /// Budget for P1 normal tasks — main workload tier.
    pub fn p1_normal() -> Self {
        Self {
            max_concurrent: 4,
            max_tokens_per_hour: 500_000,
            max_rss_mb: 4096,
            sheddable: false,
        }
    }

    /// Budget for P2 background tasks — first to shed under pressure.
    pub fn p2_background() -> Self {
        Self {
            max_concurrent: 2,
            max_tokens_per_hour: 200_000,
            max_rss_mb: 2048,
            sheddable: true,
        }
    }
}

/// Tracks real-time usage for a priority tier.
#[derive(Debug)]
pub struct TierUsage {
    pub active_tasks: AtomicU64,
    pub tokens_used_this_hour: AtomicU64,
    pub rss_allocated_mb: AtomicU64,
}

impl TierUsage {
    pub fn new() -> Self {
        Self {
            active_tasks: AtomicU64::new(0),
            tokens_used_this_hour: AtomicU64::new(0),
            rss_allocated_mb: AtomicU64::new(0),
        }
    }

    /// Record that a task has started, consuming the given RSS.
    pub fn record_task_start(&self, rss_mb: u64) {
        self.active_tasks.fetch_add(1, Ordering::SeqCst);
        self.rss_allocated_mb.fetch_add(rss_mb, Ordering::SeqCst);
    }

    /// Record that a task has ended, releasing the given RSS.
    pub fn record_task_end(&self, rss_mb: u64) {
        self.active_tasks.fetch_sub(1, Ordering::SeqCst);
        self.rss_allocated_mb
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_sub(rss_mb))
            })
            .ok();
    }

    /// Record token consumption.
    pub fn record_tokens(&self, count: u64) {
        self.tokens_used_this_hour
            .fetch_add(count, Ordering::SeqCst);
    }

    /// Reset the hourly token counter (called by the governor's hourly tick).
    pub fn reset_hourly_tokens(&self) {
        self.tokens_used_this_hour.store(0, Ordering::SeqCst);
    }
}

impl Default for TierUsage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_budget_defaults() {
        let p0 = TierBudget::p0_critical();
        assert_eq!(p0.max_concurrent, 2);
        assert!(!p0.sheddable);

        let p1 = TierBudget::p1_normal();
        assert_eq!(p1.max_concurrent, 4);
        assert!(!p1.sheddable);

        let p2 = TierBudget::p2_background();
        assert_eq!(p2.max_concurrent, 2);
        assert!(p2.sheddable);
    }

    #[test]
    fn test_tier_usage_task_lifecycle() {
        let usage = TierUsage::new();

        usage.record_task_start(256);
        assert_eq!(usage.active_tasks.load(Ordering::SeqCst), 1);
        assert_eq!(usage.rss_allocated_mb.load(Ordering::SeqCst), 256);

        usage.record_task_start(128);
        assert_eq!(usage.active_tasks.load(Ordering::SeqCst), 2);
        assert_eq!(usage.rss_allocated_mb.load(Ordering::SeqCst), 384);

        usage.record_task_end(256);
        assert_eq!(usage.active_tasks.load(Ordering::SeqCst), 1);
        assert_eq!(usage.rss_allocated_mb.load(Ordering::SeqCst), 128);

        usage.record_task_end(128);
        assert_eq!(usage.active_tasks.load(Ordering::SeqCst), 0);
        assert_eq!(usage.rss_allocated_mb.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_tier_usage_rss_saturating_sub() {
        let usage = TierUsage::new();
        usage.record_task_start(100);
        // Releasing more than allocated should saturate to 0.
        usage.record_task_end(200);
        assert_eq!(usage.rss_allocated_mb.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_tier_usage_tokens() {
        let usage = TierUsage::new();
        usage.record_tokens(1000);
        usage.record_tokens(500);
        assert_eq!(usage.tokens_used_this_hour.load(Ordering::SeqCst), 1500);

        usage.reset_hourly_tokens();
        assert_eq!(usage.tokens_used_this_hour.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_tier_usage_thread_safety() {
        let usage = std::sync::Arc::new(TierUsage::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let u = usage.clone();
            handles.push(std::thread::spawn(move || {
                u.record_task_start(100);
                u.record_tokens(50);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(usage.active_tasks.load(Ordering::SeqCst), 10);
        assert_eq!(usage.rss_allocated_mb.load(Ordering::SeqCst), 1000);
        assert_eq!(usage.tokens_used_this_hour.load(Ordering::SeqCst), 500);
    }
}
