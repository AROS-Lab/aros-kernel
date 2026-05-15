use serde::{Deserialize, Serialize};

use super::probe::SystemResources;

/// Thermal pressure severity, mirroring the `MemoryPressureLevel` pattern.
///
/// Thermal throttling is gradual rather than a hard cliff, so the scheduler
/// treats these levels as a graceful-degradation multiplier (see
/// `Recommender::recommend_max_agents_thermal`) rather than a hard stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThermalPressureLevel {
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalResult {
    pub level: ThermalPressureLevel,
    /// Where the reading came from: `xcpm_thermal_level`, `load_ratio`,
    /// or `unavailable` (fail-safe — treated as Nominal, no throttling).
    pub source: String,
}

// ── macOS sysctl helper ─────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod sysctl {
    use std::ffi::CString;
    use std::mem;

    pub unsafe fn sysctl_i32(name: &str) -> Option<i32> {
        let c_name = CString::new(name).ok()?;
        let mut value: i32 = 0;
        let mut size = mem::size_of::<i32>();
        let ret = unsafe {
            libc::sysctlbyname(
                c_name.as_ptr(),
                &mut value as *mut i32 as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret == 0 { Some(value) } else { None }
    }
}

// ── Detection logic ─────────────────────────────────────────────────

/// Classify a raw macOS `machdep.xcpm.cpu_thermal_level` reading.
///
/// The xcpm thermal level is a 0..=N pressure indicator: 0 is nominal,
/// higher values mean the CPU package is closer to its thermal limit.
fn classify_xcpm_level(raw: i32) -> ThermalPressureLevel {
    match raw {
        i32::MIN..=0 => ThermalPressureLevel::Nominal,
        1..=30 => ThermalPressureLevel::Fair,
        31..=60 => ThermalPressureLevel::Serious,
        _ => ThermalPressureLevel::Critical,
    }
}

/// Portable thermal proxy: the 1-minute load average relative to core count.
///
/// Sustained load above the core count is the strongest dependency-free
/// signal that a headless box is working hard enough to heat up. Used as
/// the fallback whenever a direct thermal sensor reading is unavailable.
pub fn detect_from_load_ratio(resources: &SystemResources) -> ThermalResult {
    if resources.cpu_count == 0 {
        // No usable data — fail safe to Nominal so we never throttle blindly.
        return ThermalResult {
            level: ThermalPressureLevel::Nominal,
            source: "unavailable".into(),
        };
    }

    let ratio = resources.load_avg_1 / resources.cpu_count as f64;
    let level = if ratio < 0.70 {
        ThermalPressureLevel::Nominal
    } else if ratio < 1.00 {
        ThermalPressureLevel::Fair
    } else if ratio < 1.50 {
        ThermalPressureLevel::Serious
    } else {
        ThermalPressureLevel::Critical
    };

    ThermalResult {
        level,
        source: "load_ratio".into(),
    }
}

/// Detect thermal pressure from system resources.
///
/// On macOS this prefers the kernel's `machdep.xcpm.cpu_thermal_level`
/// signal and falls back to the portable load-ratio proxy when that
/// sysctl is unavailable (e.g. on Apple Silicon). On other platforms it
/// uses the load-ratio proxy directly. Linux thermal-zone and GPU/power
/// telemetry are intentionally out of scope for this pass.
#[cfg(target_os = "macos")]
pub fn detect_thermal(resources: &SystemResources) -> ThermalResult {
    if let Some(raw) = unsafe { sysctl::sysctl_i32("machdep.xcpm.cpu_thermal_level") } {
        return ThermalResult {
            level: classify_xcpm_level(raw),
            source: "xcpm_thermal_level".into(),
        };
    }
    // sysctl key absent — fall back to the portable proxy.
    detect_from_load_ratio(resources)
}

#[cfg(not(target_os = "macos"))]
pub fn detect_thermal(resources: &SystemResources) -> ThermalResult {
    detect_from_load_ratio(resources)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_resources(cpu_count: usize, load_avg_1: f64) -> SystemResources {
        SystemResources {
            cpu_count,
            ram_total_mb: 16000,
            ram_available_mb: 8000,
            load_avg_1,
            load_avg_5: load_avg_1,
            load_avg_15: load_avg_1,
        }
    }

    #[test]
    fn test_classify_xcpm_level() {
        assert_eq!(classify_xcpm_level(0), ThermalPressureLevel::Nominal);
        assert_eq!(classify_xcpm_level(-5), ThermalPressureLevel::Nominal);
        assert_eq!(classify_xcpm_level(1), ThermalPressureLevel::Fair);
        assert_eq!(classify_xcpm_level(30), ThermalPressureLevel::Fair);
        assert_eq!(classify_xcpm_level(31), ThermalPressureLevel::Serious);
        assert_eq!(classify_xcpm_level(60), ThermalPressureLevel::Serious);
        assert_eq!(classify_xcpm_level(61), ThermalPressureLevel::Critical);
        assert_eq!(classify_xcpm_level(i32::MAX), ThermalPressureLevel::Critical);
    }

    #[test]
    fn test_load_ratio_nominal() {
        // 10 cores, load 2.0 → ratio 0.2 → Nominal
        let res = detect_from_load_ratio(&mock_resources(10, 2.0));
        assert_eq!(res.level, ThermalPressureLevel::Nominal);
        assert_eq!(res.source, "load_ratio");
    }

    #[test]
    fn test_load_ratio_fair() {
        // 10 cores, load 8.0 → ratio 0.8 → Fair
        let res = detect_from_load_ratio(&mock_resources(10, 8.0));
        assert_eq!(res.level, ThermalPressureLevel::Fair);
    }

    #[test]
    fn test_load_ratio_serious() {
        // 10 cores, load 12.0 → ratio 1.2 → Serious
        let res = detect_from_load_ratio(&mock_resources(10, 12.0));
        assert_eq!(res.level, ThermalPressureLevel::Serious);
    }

    #[test]
    fn test_load_ratio_critical() {
        // 10 cores, load 20.0 → ratio 2.0 → Critical
        let res = detect_from_load_ratio(&mock_resources(10, 20.0));
        assert_eq!(res.level, ThermalPressureLevel::Critical);
    }

    #[test]
    fn test_load_ratio_boundaries() {
        // ratio exactly 0.70 → Fair (not Nominal)
        assert_eq!(
            detect_from_load_ratio(&mock_resources(10, 7.0)).level,
            ThermalPressureLevel::Fair
        );
        // ratio exactly 1.00 → Serious
        assert_eq!(
            detect_from_load_ratio(&mock_resources(10, 10.0)).level,
            ThermalPressureLevel::Serious
        );
        // ratio exactly 1.50 → Critical
        assert_eq!(
            detect_from_load_ratio(&mock_resources(10, 15.0)).level,
            ThermalPressureLevel::Critical
        );
    }

    #[test]
    fn test_zero_cpu_count_is_unavailable() {
        // cpu_count == 0 must not divide-by-zero; fail safe to Nominal.
        let res = detect_from_load_ratio(&mock_resources(0, 99.0));
        assert_eq!(res.level, ThermalPressureLevel::Nominal);
        assert_eq!(res.source, "unavailable");
    }

    #[test]
    fn test_detect_thermal_real_hardware() {
        // Whatever platform the suite runs on, detect_thermal must return a
        // valid variant with a non-empty, recognized source.
        let resources = super::super::probe::probe_system();
        let result = detect_thermal(&resources);
        match result.level {
            ThermalPressureLevel::Nominal
            | ThermalPressureLevel::Fair
            | ThermalPressureLevel::Serious
            | ThermalPressureLevel::Critical => {}
        }
        assert!(
            ["xcpm_thermal_level", "load_ratio", "unavailable"]
                .contains(&result.source.as_str()),
            "unexpected thermal source: {}",
            result.source
        );
    }

    #[test]
    fn test_thermal_result_serializes() {
        let res = detect_from_load_ratio(&mock_resources(10, 5.0));
        let json = serde_json::to_string(&res).expect("ThermalResult must serialize");
        assert!(json.contains("level"));
        assert!(json.contains("source"));
    }
}
