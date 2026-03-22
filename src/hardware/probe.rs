use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemResources {
    pub cpu_count: usize,
    pub ram_total_mb: u64,
    pub ram_available_mb: u64,
    pub load_avg_1: f64,
    pub load_avg_5: f64,
    pub load_avg_15: f64,
}

static CACHE: Mutex<Option<(Instant, SystemResources)>> = Mutex::new(None);

const CACHE_DURATION: Duration = Duration::from_secs(2);

/// Probe system resources with 2-second caching.
pub fn probe_system() -> SystemResources {
    let mut cache = CACHE.lock().unwrap();
    if let Some((ref ts, ref res)) = *cache
        && ts.elapsed() < CACHE_DURATION
    {
        return res.clone();
    }

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();

    let load = sysinfo::System::load_average();

    let resources = SystemResources {
        cpu_count: sys.cpus().len(),
        ram_total_mb: sys.total_memory() / (1024 * 1024),
        ram_available_mb: sys.available_memory() / (1024 * 1024),
        load_avg_1: load.one,
        load_avg_5: load.five,
        load_avg_15: load.fifteen,
    };

    *cache = Some((Instant::now(), resources.clone()));
    resources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_probe_returns_valid_cpu_count() {
        let res = probe_system();
        assert!(res.cpu_count > 0, "CPU count must be > 0");
    }

    #[test]
    fn test_probe_returns_valid_ram() {
        let res = probe_system();
        assert!(res.ram_total_mb > 0, "Total RAM must be > 0");
        assert!(res.ram_available_mb > 0, "Available RAM must be > 0");
        assert!(
            res.ram_available_mb <= res.ram_total_mb,
            "Available RAM ({}) must be <= total RAM ({})",
            res.ram_available_mb,
            res.ram_total_mb
        );
    }

    #[test]
    fn test_probe_caching() {
        // Clear cache first
        {
            let mut cache = CACHE.lock().unwrap();
            *cache = None;
        }

        let first = probe_system();
        let second = probe_system(); // called immediately, should return cached

        assert_eq!(first.cpu_count, second.cpu_count);
        assert_eq!(first.ram_total_mb, second.ram_total_mb);
        assert_eq!(first.ram_available_mb, second.ram_available_mb);
        assert_eq!(first.load_avg_1.to_bits(), second.load_avg_1.to_bits());
        assert_eq!(first.load_avg_5.to_bits(), second.load_avg_5.to_bits());
        assert_eq!(first.load_avg_15.to_bits(), second.load_avg_15.to_bits());
    }
}
