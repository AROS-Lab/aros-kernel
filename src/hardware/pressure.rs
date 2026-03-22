use serde::{Deserialize, Serialize};

use super::probe::SystemResources;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryPressureLevel {
    Normal,
    Warn,
    Critical,
}

impl MemoryPressureLevel {
    /// Return the worse (higher severity) of two levels.
    fn worse(self, other: Self) -> Self {
        match (self, other) {
            (Self::Critical, _) | (_, Self::Critical) => Self::Critical,
            (Self::Warn, _) | (_, Self::Warn) => Self::Warn,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PressureResult {
    pub level: MemoryPressureLevel,
    pub ram_available_conservative_mb: u64,
    pub ram_compressor_mb: u64,
    pub source: String,
}

// ── macOS sysctl helpers ────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod sysctl {
    use std::ffi::CString;
    use std::mem;

    pub unsafe fn sysctl_u64(name: &str) -> Option<u64> {
        let c_name = CString::new(name).ok()?;
        let mut value: u64 = 0;
        let mut size = mem::size_of::<u64>();
        let ret = unsafe {
            libc::sysctlbyname(
                c_name.as_ptr(),
                &mut value as *mut u64 as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret == 0 { Some(value) } else { None }
    }

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

/// Derive pressure level from a simple RAM usage ratio (non-macOS path).
pub fn detect_from_ratio(ram_total_mb: u64, ram_available_mb: u64) -> PressureResult {
    if ram_total_mb == 0 {
        return PressureResult {
            level: MemoryPressureLevel::Critical,
            ram_available_conservative_mb: 0,
            ram_compressor_mb: 0,
            source: "ram_ratio_zero_total".into(),
        };
    }

    let used = ram_total_mb.saturating_sub(ram_available_mb);
    let ratio = used as f64 / ram_total_mb as f64;

    let level = if ratio >= 0.85 {
        MemoryPressureLevel::Critical
    } else if ratio >= 0.60 {
        MemoryPressureLevel::Warn
    } else {
        MemoryPressureLevel::Normal
    };

    PressureResult {
        level,
        ram_available_conservative_mb: ram_available_mb,
        ram_compressor_mb: 0,
        source: "ram_ratio".into(),
    }
}

/// Detect memory pressure from system resources.
///
/// On macOS this combines the Mach kernel memory-status signal with
/// the compressor-bytes ratio. On other platforms it falls back to a
/// simple RAM usage ratio.
#[cfg(target_os = "macos")]
pub fn detect_pressure(resources: &SystemResources) -> PressureResult {
    // 1. Kernel memory-status level
    let kernel_level = unsafe { sysctl::sysctl_i32("kern.memorystatus_level") }
        .map(|v| match v {
            1 => MemoryPressureLevel::Critical,
            2 => MemoryPressureLevel::Warn,
            4 => MemoryPressureLevel::Normal,
            _ => MemoryPressureLevel::Normal,
        })
        .unwrap_or(MemoryPressureLevel::Normal);

    // 2. Compressor bytes
    let compressor_bytes =
        unsafe { sysctl::sysctl_u64("vm.compressor_bytes_used") }.unwrap_or(0);
    let compressor_mb = compressor_bytes / (1024 * 1024);

    // 3. Compressor ratio
    let compressor_level = if resources.ram_total_mb > 0 {
        let ratio = compressor_mb as f64 / resources.ram_total_mb as f64;
        if ratio > 0.50 {
            MemoryPressureLevel::Critical
        } else if ratio > 0.30 {
            MemoryPressureLevel::Warn
        } else {
            MemoryPressureLevel::Normal
        }
    } else {
        MemoryPressureLevel::Normal
    };

    // 4. Worst of the two signals
    let level = kernel_level.worse(compressor_level);

    // 5. Conservative available
    let conservative = resources.ram_available_mb.saturating_sub(compressor_mb);

    let source = match (kernel_level, compressor_level) {
        (MemoryPressureLevel::Critical, _) => "kernel_signal",
        (_, MemoryPressureLevel::Critical) => "compressor_ratio",
        (MemoryPressureLevel::Warn, _) => "kernel_signal",
        (_, MemoryPressureLevel::Warn) => "compressor_ratio",
        _ => "kernel_signal",
    };

    PressureResult {
        level,
        ram_available_conservative_mb: conservative,
        ram_compressor_mb: compressor_mb,
        source: source.into(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn detect_pressure(resources: &SystemResources) -> PressureResult {
    detect_from_ratio(resources.ram_total_mb, resources.ram_available_mb)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_resources(total: u64, available: u64) -> SystemResources {
        SystemResources {
            cpu_count: 4,
            ram_total_mb: total,
            ram_available_mb: available,
            load_avg_1: 1.0,
            load_avg_5: 1.0,
            load_avg_15: 1.0,
        }
    }

    #[test]
    fn test_pressure_normal() {
        // 30% used → Normal
        let res = detect_from_ratio(16000, 11200);
        assert_eq!(res.level, MemoryPressureLevel::Normal);
        assert_eq!(res.source, "ram_ratio");
    }

    #[test]
    fn test_pressure_warn() {
        // ~69% used → Warn  (60%..85%)
        let res = detect_from_ratio(16000, 5000);
        assert_eq!(res.level, MemoryPressureLevel::Warn);
    }

    #[test]
    fn test_pressure_critical() {
        // ~94% used → Critical
        let res = detect_from_ratio(16000, 1000);
        assert_eq!(res.level, MemoryPressureLevel::Critical);
    }

    #[test]
    fn test_conservative_ram() {
        let _r = mock_resources(16000, 8000); // exercise mock_resources
        let res = detect_from_ratio(16000, 8000);
        assert!(
            res.ram_available_conservative_mb <= 8000,
            "conservative ({}) must be <= available (8000)",
            res.ram_available_conservative_mb
        );
    }

    #[test]
    fn test_real_pressure_detection() {
        let resources = super::super::probe::probe_system();
        let result = detect_pressure(&resources);
        // Just verify it returns a valid variant
        match result.level {
            MemoryPressureLevel::Normal
            | MemoryPressureLevel::Warn
            | MemoryPressureLevel::Critical => {}
        }
        assert!(
            result.ram_available_conservative_mb <= resources.ram_available_mb,
            "conservative ({}) must be <= available ({})",
            result.ram_available_conservative_mb,
            resources.ram_available_mb
        );
    }

    #[test]
    fn test_worse_level() {
        assert_eq!(
            MemoryPressureLevel::Normal.worse(MemoryPressureLevel::Warn),
            MemoryPressureLevel::Warn
        );
        assert_eq!(
            MemoryPressureLevel::Warn.worse(MemoryPressureLevel::Critical),
            MemoryPressureLevel::Critical
        );
        assert_eq!(
            MemoryPressureLevel::Critical.worse(MemoryPressureLevel::Normal),
            MemoryPressureLevel::Critical
        );
    }
}
