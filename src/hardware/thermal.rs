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
    /// Where the reading came from: `xcpm_thermal_level` (macOS),
    /// `sys_thermal_zone` (Linux), `load_ratio` (portable proxy), or
    /// `unavailable` (fail-safe — treated as Nominal, no throttling).
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

// ── Linux thermal-zone helper ───────────────────────────────────────

#[cfg(target_os = "linux")]
mod thermal_zone {
    use std::fs;

    /// Read every `/sys/class/thermal/thermal_zone*/temp` and return the
    /// hottest zone temperature in °C, or `None` if the directory is
    /// absent or no zone yields a valid reading.
    pub fn max_celsius() -> Option<f64> {
        let entries = fs::read_dir("/sys/class/thermal").ok()?;
        let readings = entries.filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("thermal_zone") {
                return None;
            }
            // A missing/unreadable temp file simply drops that zone.
            fs::read_to_string(path.join("temp")).ok()
        });
        super::max_zone_celsius(readings)
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

/// Classify an absolute CPU/SoC temperature (°C) into a pressure level.
///
/// Unlike the relative xcpm index, Linux thermal zones report an absolute
/// temperature, so we band it directly. Cutoffs are heuristic — chosen to
/// mirror the xcpm bands and tunable as field data arrives: `< 60` is
/// comfortable, `>= 95` is at the throttle ceiling of typical x86/ARM
/// packages.
#[cfg(any(target_os = "linux", test))]
fn classify_celsius(celsius: f64) -> ThermalPressureLevel {
    if celsius < 60.0 {
        ThermalPressureLevel::Nominal
    } else if celsius < 80.0 {
        ThermalPressureLevel::Fair
    } else if celsius < 95.0 {
        ThermalPressureLevel::Serious
    } else {
        ThermalPressureLevel::Critical
    }
}

/// Parse a single `/sys/class/thermal/thermal_zone*/temp` reading.
///
/// The sysfs file holds the zone temperature in millidegrees Celsius as
/// ASCII (e.g. `"52000\n"` == 52.0 °C). Returns `None` for empty or
/// non-numeric contents so a flaky zone never poisons the reading.
#[cfg(any(target_os = "linux", test))]
fn parse_zone_millidegrees(raw: &str) -> Option<f64> {
    let milli: i64 = raw.trim().parse().ok()?;
    Some(milli as f64 / 1000.0)
}

/// Reduce a set of raw sysfs `temp` contents to the hottest zone, in °C.
///
/// A box exposes several zones (CPU, GPU, battery, …); the hottest is the
/// one that bounds throttling, so we take the max. Unreadable or
/// non-numeric zones are skipped; if no zone yields a valid reading the
/// result is `None` so the caller falls back to the load-ratio proxy.
#[cfg(any(target_os = "linux", test))]
fn max_zone_celsius<I, S>(zones: I) -> Option<f64>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    zones
        .into_iter()
        .filter_map(|raw| parse_zone_millidegrees(raw.as_ref()))
        .fold(None, |acc, c| Some(acc.map_or(c, |m: f64| m.max(c))))
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
/// Each platform prefers its most direct sensor and falls back to the
/// portable load-ratio proxy when that sensor is unavailable:
/// - **macOS**: the kernel's `machdep.xcpm.cpu_thermal_level` sysctl
///   (absent on some Apple Silicon → proxy).
/// - **Linux**: the hottest `/sys/class/thermal/thermal_zone*/temp` zone
///   (absent in some containers/VMs → proxy).
/// - **Other**: the load-ratio proxy directly.
///
/// All paths fail safe: any missing/unreadable sensor degrades to the
/// proxy rather than throttling blindly. GPU/power telemetry remains out
/// of scope for this pass.
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

#[cfg(target_os = "linux")]
pub fn detect_thermal(resources: &SystemResources) -> ThermalResult {
    if let Some(celsius) = thermal_zone::max_celsius() {
        return ThermalResult {
            level: classify_celsius(celsius),
            source: "sys_thermal_zone".into(),
        };
    }
    // No readable thermal zone — fall back to the portable proxy.
    detect_from_load_ratio(resources)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
            ["xcpm_thermal_level", "sys_thermal_zone", "load_ratio", "unavailable"]
                .contains(&result.source.as_str()),
            "unexpected thermal source: {}",
            result.source
        );
    }

    #[test]
    fn test_classify_celsius() {
        // Below 60 °C is comfortable.
        assert_eq!(classify_celsius(20.0), ThermalPressureLevel::Nominal);
        assert_eq!(classify_celsius(59.9), ThermalPressureLevel::Nominal);
        // 60..80 → Fair (boundary is inclusive lower).
        assert_eq!(classify_celsius(60.0), ThermalPressureLevel::Fair);
        assert_eq!(classify_celsius(79.9), ThermalPressureLevel::Fair);
        // 80..95 → Serious.
        assert_eq!(classify_celsius(80.0), ThermalPressureLevel::Serious);
        assert_eq!(classify_celsius(94.9), ThermalPressureLevel::Serious);
        // >= 95 → Critical.
        assert_eq!(classify_celsius(95.0), ThermalPressureLevel::Critical);
        assert_eq!(classify_celsius(110.0), ThermalPressureLevel::Critical);
    }

    #[test]
    fn test_parse_zone_millidegrees() {
        // Millidegrees with trailing newline, as sysfs emits it.
        assert_eq!(parse_zone_millidegrees("52000\n"), Some(52.0));
        assert_eq!(parse_zone_millidegrees("  48500 "), Some(48.5));
        assert_eq!(parse_zone_millidegrees("0"), Some(0.0));
        // Junk / empty zones are dropped, not panicked on.
        assert_eq!(parse_zone_millidegrees(""), None);
        assert_eq!(parse_zone_millidegrees("N/A"), None);
        assert_eq!(parse_zone_millidegrees("52.0"), None);
    }

    #[test]
    fn test_max_zone_celsius_picks_hottest() {
        // Several zones → hottest wins (GPU at 71 °C here).
        let zones = vec!["45000\n", "71000\n", "60000\n"];
        assert_eq!(max_zone_celsius(zones), Some(71.0));
    }

    #[test]
    fn test_max_zone_celsius_skips_unreadable() {
        // Non-numeric zones are skipped; the lone valid reading wins.
        let zones = vec!["error", "", "63000\n", "garbage"];
        assert_eq!(max_zone_celsius(zones), Some(63.0));
    }

    #[test]
    fn test_max_zone_celsius_none_when_empty() {
        // No zones at all → None so the caller falls back to the proxy.
        let empty: Vec<&str> = vec![];
        assert_eq!(max_zone_celsius(empty), None);
        // All-invalid zones also yield None.
        assert_eq!(max_zone_celsius(vec!["x", "y"]), None);
    }

    #[test]
    fn test_thermal_result_serializes() {
        let res = detect_from_load_ratio(&mock_resources(10, 5.0));
        let json = serde_json::to_string(&res).expect("ThermalResult must serialize");
        assert!(json.contains("level"));
        assert!(json.contains("source"));
    }
}
