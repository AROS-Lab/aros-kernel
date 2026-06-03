use serde::{Deserialize, Serialize};

use super::probe::SystemResources;

/// Power-draw pressure severity, mirroring `ThermalPressureLevel` /
/// `GpuPressureLevel`.
///
/// Like thermal and GPU, package power draw degrades gracefully rather than
/// cliff-edging, so consumers should treat these as a multiplier for
/// parallelism budgets rather than a hard stop. Scheduler integration is a
/// separate slice (the same staging gpu.rs used — no `Recommender` wiring in
/// this pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerPressureLevel {
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerResult {
    pub level: PowerPressureLevel,
    /// Per-package power draw in watts when a reading is available, `None`
    /// when the probe fails or the platform is unsupported.
    pub package_watts: Option<f64>,
    /// Where the reading came from: `iokit_smc_property` or `unavailable`
    /// (fail-safe — treated as Nominal, no throttling).
    pub source: String,
}

// ── macOS IOKit helper ──────────────────────────────────────────────
//
// We intentionally reuse only the *read-only* IOKit registry-property surface
// that gpu.rs already proved out (IOServiceMatching → IOIteratorNext →
// IORegistryEntryCreateCFProperty). The heavyweight AppleSMC user-client
// protocol (IOServiceOpen + IOConnectCallStructMethod + SMC opcode/wire-format
// decode) is deliberately out of scope for this slice: it is materially more
// fragile and pointer-lifetime-heavy than any existing probe, and the
// fail-safe contract below means a missing reading is harmless. If a host does
// not surface package power as a read-only property, the probe degrades to
// `unavailable` (Nominal, no throttling) — escalating to the user-client path
// is a documented follow-up.

#[cfg(target_os = "macos")]
mod iokit {
    use std::os::raw::{c_char, c_int, c_void};

    pub type IOReturn = c_int;
    pub type IOOptionBits = u32;
    pub type CFAllocatorRef = *const c_void;
    pub type CFDictionaryRef = *const c_void;
    pub type CFMutableDictionaryRef = *mut c_void;
    pub type CFStringRef = *const c_void;
    pub type CFTypeRef = *const c_void;
    pub type CFNumberRef = *const c_void;
    pub type CFNumberType = c_int;
    pub type CFTypeID = usize;
    pub type CFStringEncoding = u32;
    pub type Boolean = u8;
    pub type IoIteratorT = u32;
    pub type IoObjectT = u32;
    pub type IoRegistryEntryT = u32;
    pub type MachPortT = u32;

    /// kCFNumberDoubleType — decode CFNumbers as IEEE-754 f64.
    pub const K_CF_NUMBER_DOUBLE_TYPE: CFNumberType = 13;
    pub const K_CF_STRING_ENCODING_UTF8: CFStringEncoding = 0x0800_0100;
    pub const KERN_SUCCESS: IOReturn = 0;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
        pub fn IOServiceGetMatchingServices(
            main_port: MachPortT,
            matching: CFDictionaryRef,
            existing: *mut IoIteratorT,
        ) -> IOReturn;
        pub fn IOIteratorNext(iterator: IoIteratorT) -> IoObjectT;
        pub fn IOObjectRelease(object: IoObjectT) -> IOReturn;
        pub fn IORegistryEntryCreateCFProperty(
            entry: IoRegistryEntryT,
            key: CFStringRef,
            allocator: CFAllocatorRef,
            options: IOOptionBits,
        ) -> CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const c_char,
            encoding: CFStringEncoding,
        ) -> CFStringRef;
        pub fn CFNumberGetValue(
            number: CFNumberRef,
            the_type: CFNumberType,
            value_ptr: *mut c_void,
        ) -> Boolean;
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        pub fn CFNumberGetTypeID() -> CFTypeID;
    }
}

// ── Detection logic ─────────────────────────────────────────────────

/// Candidate read-only IORegistry property keys that may carry a package
/// power-draw figure in watts. These mirror the conventional SMC power-sensor
/// key names; the first one that resolves to a finite, non-negative CFNumber
/// wins. The set is first-pass and may be widened as more host shapes are
/// observed.
#[cfg(target_os = "macos")]
const POWER_KEYS: [&str; 4] = ["PCPC", "PSTR", "PHPC", "PSYS"];

/// Classify a per-package power draw (watts) into a pressure bucket.
///
/// Buckets mirror the four-level shape used by `thermal.rs` / `gpu.rs`. The
/// cutoffs are sized for a headless Apple-Silicon / small-form-factor package
/// envelope (idle single-digit watts; sustained load tens of watts) and are
/// explicitly first-pass — they will be tuned once the scheduler starts
/// consuming the signal (separate slice).
fn classify_watts(watts: f64) -> PowerPressureLevel {
    if watts < 15.0 {
        PowerPressureLevel::Nominal
    } else if watts < 35.0 {
        PowerPressureLevel::Fair
    } else if watts < 60.0 {
        PowerPressureLevel::Serious
    } else {
        PowerPressureLevel::Critical
    }
}

/// Fail-safe result: no reading, no throttling.
fn unavailable() -> PowerResult {
    PowerResult {
        level: PowerPressureLevel::Nominal,
        package_watts: None,
        source: "unavailable".into(),
    }
}

#[cfg(target_os = "macos")]
unsafe fn read_smc_package_watts() -> Option<f64> {
    use iokit::*;
    use std::ffi::CString;
    use std::os::raw::c_void;
    use std::ptr;

    let c_name = CString::new("AppleSMC").ok()?;
    // SAFETY: c_name is a valid NUL-terminated string for the duration of the call.
    let matching = unsafe { IOServiceMatching(c_name.as_ptr()) };
    if matching.is_null() {
        return None;
    }

    let mut iterator: IoIteratorT = 0;
    // SAFETY: matching is non-null and is consumed by this call regardless of
    // return value, so we do not release it ourselves.
    let kr =
        unsafe { IOServiceGetMatchingServices(0, matching as CFDictionaryRef, &mut iterator) };
    if kr != KERN_SUCCESS || iterator == 0 {
        return None;
    }

    let mut found: Option<f64> = None;
    'services: loop {
        // SAFETY: iterator is a live io_iterator_t from the call above.
        let service = unsafe { IOIteratorNext(iterator) };
        if service == 0 {
            break;
        }

        for key_name in POWER_KEYS {
            let key_cstr = match CString::new(key_name) {
                Ok(s) => s,
                Err(_) => continue,
            };
            // SAFETY: key_cstr outlives the CFString creation call.
            let key = unsafe {
                CFStringCreateWithCString(
                    ptr::null(),
                    key_cstr.as_ptr(),
                    K_CF_STRING_ENCODING_UTF8,
                )
            };
            if key.is_null() {
                continue;
            }

            // SAFETY: service is a live io_registry_entry_t, key is a valid CFStringRef.
            let value = unsafe { IORegistryEntryCreateCFProperty(service, key, ptr::null(), 0) };
            // We own `key` (Create rule) — release unconditionally.
            unsafe { CFRelease(key) };

            if value.is_null() {
                continue;
            }

            // Guard against an unexpected non-number payload.
            // SAFETY: value is a non-null CFTypeRef we own.
            let value_type = unsafe { CFGetTypeID(value as CFTypeRef) };
            let num_type = unsafe { CFNumberGetTypeID() };
            if value_type != num_type {
                unsafe { CFRelease(value as CFTypeRef) };
                continue;
            }

            let mut raw: f64 = 0.0;
            let ok = unsafe {
                CFNumberGetValue(
                    value as CFNumberRef,
                    K_CF_NUMBER_DOUBLE_TYPE,
                    &mut raw as *mut f64 as *mut c_void,
                )
            };
            unsafe { CFRelease(value as CFTypeRef) };

            // Only accept a finite, non-negative reading — anything else is
            // treated as no signal so the caller fail-safes to Nominal.
            if ok != 0 && raw.is_finite() && raw >= 0.0 {
                found = Some(raw);
                // Release the current service before leaving the walk.
                unsafe { IOObjectRelease(service) };
                break 'services;
            }
        }

        // SAFETY: service is a live io_object_t we have not yet released.
        unsafe { IOObjectRelease(service) };
    }
    // SAFETY: iterator is a live io_iterator_t from IOServiceGetMatchingServices.
    unsafe { IOObjectRelease(iterator) };

    found
}

/// Detect package power-draw pressure from system resources.
///
/// On macOS this attempts a read-only IOKit registry-property read against the
/// `AppleSMC` service for a known per-package power key (see `POWER_KEYS`). Any
/// FFI failure, missing key, non-numeric payload, or non-macOS host falls back
/// to `unavailable` — Nominal, no throttling. `resources` is unused at this
/// slice but accepted to keep the probe shape consistent with `thermal.rs` /
/// `gpu.rs` for future fallback proxies.
#[cfg(target_os = "macos")]
pub fn detect_power(_resources: &SystemResources) -> PowerResult {
    match unsafe { read_smc_package_watts() } {
        Some(watts) => PowerResult {
            level: classify_watts(watts),
            package_watts: Some(watts),
            source: "iokit_smc_property".into(),
        },
        None => unavailable(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn detect_power(_resources: &SystemResources) -> PowerResult {
    unavailable()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_resources() -> SystemResources {
        SystemResources {
            cpu_count: 10,
            ram_total_mb: 16000,
            ram_available_mb: 8000,
            load_avg_1: 1.0,
            load_avg_5: 1.0,
            load_avg_15: 1.0,
        }
    }

    #[test]
    fn test_classify_watts() {
        assert_eq!(classify_watts(0.0), PowerPressureLevel::Nominal);
        assert_eq!(classify_watts(14.9), PowerPressureLevel::Nominal);
        assert_eq!(classify_watts(15.0), PowerPressureLevel::Fair);
        assert_eq!(classify_watts(34.9), PowerPressureLevel::Fair);
        assert_eq!(classify_watts(35.0), PowerPressureLevel::Serious);
        assert_eq!(classify_watts(59.9), PowerPressureLevel::Serious);
        assert_eq!(classify_watts(60.0), PowerPressureLevel::Critical);
        assert_eq!(classify_watts(250.0), PowerPressureLevel::Critical);
    }

    #[test]
    fn test_unavailable_is_nominal() {
        // Fail-safe contract: no reading must never request throttling.
        let r = unavailable();
        assert_eq!(r.level, PowerPressureLevel::Nominal);
        assert_eq!(r.package_watts, None);
        assert_eq!(r.source, "unavailable");
    }

    #[test]
    fn test_detect_power_real_hardware() {
        // Whatever platform the suite runs on, detect_power must return a valid
        // variant with a recognized source and a self-consistent shape: an
        // `unavailable` source implies no reading and Nominal; a real reading
        // is finite and non-negative.
        let result = detect_power(&mock_resources());
        match result.level {
            PowerPressureLevel::Nominal
            | PowerPressureLevel::Fair
            | PowerPressureLevel::Serious
            | PowerPressureLevel::Critical => {}
        }
        assert!(
            ["iokit_smc_property", "unavailable"].contains(&result.source.as_str()),
            "unexpected power source: {}",
            result.source
        );
        if result.source == "unavailable" {
            assert_eq!(result.package_watts, None);
            assert_eq!(result.level, PowerPressureLevel::Nominal);
        } else if let Some(watts) = result.package_watts {
            assert!(
                watts.is_finite() && watts >= 0.0,
                "package_watts out of range: {watts}"
            );
            // A present reading must classify consistently with classify_watts.
            assert_eq!(result.level, classify_watts(watts));
        }
    }

    #[test]
    fn test_power_result_serializes() {
        // Round-trip every level + both source variants to catch field renames.
        let cases = [
            PowerResult {
                level: PowerPressureLevel::Nominal,
                package_watts: None,
                source: "unavailable".into(),
            },
            PowerResult {
                level: PowerPressureLevel::Fair,
                package_watts: Some(22.5),
                source: "iokit_smc_property".into(),
            },
            PowerResult {
                level: PowerPressureLevel::Serious,
                package_watts: Some(48.0),
                source: "iokit_smc_property".into(),
            },
            PowerResult {
                level: PowerPressureLevel::Critical,
                package_watts: Some(95.0),
                source: "iokit_smc_property".into(),
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).expect("PowerResult must serialize");
            assert!(json.contains("level"));
            assert!(json.contains("package_watts"));
            assert!(json.contains("source"));
            let back: PowerResult =
                serde_json::from_str(&json).expect("PowerResult must round-trip");
            assert_eq!(back.level, case.level);
            assert_eq!(back.package_watts, case.package_watts);
            assert_eq!(back.source, case.source);
        }
    }
}
