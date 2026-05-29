use serde::{Deserialize, Serialize};

use super::probe::SystemResources;

/// GPU pressure severity, mirroring `ThermalPressureLevel`.
///
/// Like thermal, GPU contention degrades gracefully rather than cliff-edging,
/// so consumers should treat these as a multiplier for parallelism budgets
/// rather than a hard stop. Scheduler integration is a separate slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuPressureLevel {
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuResult {
    pub level: GpuPressureLevel,
    /// Raw device utilization percentage (0..=100) when a reading is
    /// available, `None` when the probe fails or the platform is unsupported.
    pub utilization_pct: Option<u8>,
    /// Where the reading came from: `iokit_accelerator` or `unavailable`
    /// (fail-safe — treated as Nominal, no throttling).
    pub source: String,
}

// ── macOS IOKit helper ──────────────────────────────────────────────

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

    pub const K_CF_NUMBER_SINT64_TYPE: CFNumberType = 4;
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
        pub fn CFDictionaryGetValue(
            the_dict: CFDictionaryRef,
            key: *const c_void,
        ) -> *const c_void;
        pub fn CFNumberGetValue(
            number: CFNumberRef,
            the_type: CFNumberType,
            value_ptr: *mut c_void,
        ) -> Boolean;
        pub fn CFRelease(cf: CFTypeRef);
        pub fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        pub fn CFDictionaryGetTypeID() -> CFTypeID;
        pub fn CFNumberGetTypeID() -> CFTypeID;
    }
}

// ── Detection logic ─────────────────────────────────────────────────

/// Classify a 0..=100 device utilization percentage into a pressure bucket.
///
/// Buckets mirror the xcpm-style four-level shape used by `thermal.rs`; the
/// exact cutoffs are first-pass and will be tuned when the scheduler starts
/// consuming the signal (separate slice).
fn classify_utilization(pct: u8) -> GpuPressureLevel {
    match pct {
        0..=29 => GpuPressureLevel::Nominal,
        30..=59 => GpuPressureLevel::Fair,
        60..=84 => GpuPressureLevel::Serious,
        _ => GpuPressureLevel::Critical,
    }
}

/// Fail-safe result: no reading, no throttling.
fn unavailable() -> GpuResult {
    GpuResult {
        level: GpuPressureLevel::Nominal,
        utilization_pct: None,
        source: "unavailable".into(),
    }
}

#[cfg(target_os = "macos")]
unsafe fn read_iokit_utilization_pct() -> Option<u8> {
    use iokit::*;
    use std::ffi::CString;
    use std::os::raw::c_void;
    use std::ptr;

    // Apple Silicon exposes `AGXAccelerator`; Intel discrete/integrated GPUs
    // typically show up under the generic `IOAccelerator` class. Try both and
    // accept the first one that has a usable PerformanceStatistics dict.
    for service_name in ["AGXAccelerator", "IOAccelerator"] {
        let c_name = CString::new(service_name).ok()?;
        // SAFETY: c_name is a valid NUL-terminated string for the duration of the call.
        let matching = unsafe { IOServiceMatching(c_name.as_ptr()) };
        if matching.is_null() {
            continue;
        }

        let mut iterator: IoIteratorT = 0;
        // SAFETY: matching is non-null and is consumed by this call regardless
        // of return value, so we do not release it ourselves.
        let kr = unsafe {
            IOServiceGetMatchingServices(0, matching as CFDictionaryRef, &mut iterator)
        };
        if kr != KERN_SUCCESS || iterator == 0 {
            continue;
        }

        // Walk all matching services; first one with a numeric utilization wins.
        let mut found: Option<u8> = None;
        loop {
            // SAFETY: iterator is a live io_iterator_t from the call above.
            let service = unsafe { IOIteratorNext(iterator) };
            if service == 0 {
                break;
            }

            let key_cstr = match CString::new("PerformanceStatistics") {
                Ok(s) => s,
                Err(_) => {
                    unsafe { IOObjectRelease(service) };
                    break;
                }
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
                unsafe { IOObjectRelease(service) };
                continue;
            }

            // SAFETY: service is a live io_registry_entry_t, key is a valid CFStringRef.
            let props = unsafe {
                IORegistryEntryCreateCFProperty(service, key, ptr::null(), 0)
            };
            // We own `key` (Create rule) and `service` — release both unconditionally.
            unsafe { CFRelease(key) };
            unsafe { IOObjectRelease(service) };

            if props.is_null() {
                continue;
            }

            // Guard against an unexpected non-dictionary payload.
            // SAFETY: props is a non-null CFTypeRef we own.
            let props_type = unsafe { CFGetTypeID(props) };
            let dict_type = unsafe { CFDictionaryGetTypeID() };
            if props_type != dict_type {
                unsafe { CFRelease(props) };
                continue;
            }

            let util_key_cstr = match CString::new("Device Utilization %") {
                Ok(s) => s,
                Err(_) => {
                    unsafe { CFRelease(props) };
                    continue;
                }
            };
            let util_key = unsafe {
                CFStringCreateWithCString(
                    ptr::null(),
                    util_key_cstr.as_ptr(),
                    K_CF_STRING_ENCODING_UTF8,
                )
            };
            if util_key.is_null() {
                unsafe { CFRelease(props) };
                continue;
            }

            // CFDictionaryGetValue is a "Get" — we do not own the returned value.
            let value =
                unsafe { CFDictionaryGetValue(props, util_key as *const c_void) };
            unsafe { CFRelease(util_key) };

            if value.is_null() {
                unsafe { CFRelease(props) };
                continue;
            }

            let value_type = unsafe { CFGetTypeID(value as CFTypeRef) };
            let num_type = unsafe { CFNumberGetTypeID() };
            if value_type != num_type {
                unsafe { CFRelease(props) };
                continue;
            }

            let mut raw: i64 = 0;
            let ok = unsafe {
                CFNumberGetValue(
                    value as CFNumberRef,
                    K_CF_NUMBER_SINT64_TYPE,
                    &mut raw as *mut i64 as *mut c_void,
                )
            };
            unsafe { CFRelease(props) };

            if ok != 0 {
                found = Some(raw.clamp(0, 100) as u8);
                break;
            }
        }
        unsafe { IOObjectRelease(iterator) };

        if found.is_some() {
            return found;
        }
    }

    None
}

/// Detect GPU pressure from system resources.
///
/// On macOS this queries IOKit's `IOAccelerator`/`AGXAccelerator` services
/// for a `Device Utilization %` reading. Any FFI failure, missing key, or
/// non-macOS host falls back to `unavailable` — Nominal, no throttling.
/// `resources` is unused at this slice but accepted to keep the probe shape
/// consistent with `thermal.rs` for future fallback proxies.
#[cfg(target_os = "macos")]
pub fn detect_gpu(_resources: &SystemResources) -> GpuResult {
    match unsafe { read_iokit_utilization_pct() } {
        Some(pct) => GpuResult {
            level: classify_utilization(pct),
            utilization_pct: Some(pct),
            source: "iokit_accelerator".into(),
        },
        None => unavailable(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn detect_gpu(_resources: &SystemResources) -> GpuResult {
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
    fn test_classify_utilization() {
        assert_eq!(classify_utilization(0), GpuPressureLevel::Nominal);
        assert_eq!(classify_utilization(29), GpuPressureLevel::Nominal);
        assert_eq!(classify_utilization(30), GpuPressureLevel::Fair);
        assert_eq!(classify_utilization(59), GpuPressureLevel::Fair);
        assert_eq!(classify_utilization(60), GpuPressureLevel::Serious);
        assert_eq!(classify_utilization(84), GpuPressureLevel::Serious);
        assert_eq!(classify_utilization(85), GpuPressureLevel::Critical);
        assert_eq!(classify_utilization(100), GpuPressureLevel::Critical);
    }

    #[test]
    fn test_unavailable_is_nominal() {
        let r = unavailable();
        assert_eq!(r.level, GpuPressureLevel::Nominal);
        assert_eq!(r.utilization_pct, None);
        assert_eq!(r.source, "unavailable");
    }

    #[test]
    fn test_detect_gpu_real_hardware() {
        // Whatever platform the suite runs on, detect_gpu must return a valid
        // variant with a recognized source and a self-consistent shape: an
        // `unavailable` source implies no utilization reading.
        let result = detect_gpu(&mock_resources());
        match result.level {
            GpuPressureLevel::Nominal
            | GpuPressureLevel::Fair
            | GpuPressureLevel::Serious
            | GpuPressureLevel::Critical => {}
        }
        assert!(
            ["iokit_accelerator", "unavailable"].contains(&result.source.as_str()),
            "unexpected gpu source: {}",
            result.source
        );
        if result.source == "unavailable" {
            assert_eq!(result.utilization_pct, None);
            assert_eq!(result.level, GpuPressureLevel::Nominal);
        } else if let Some(pct) = result.utilization_pct {
            assert!(pct <= 100, "utilization_pct out of range: {pct}");
        }
    }

    #[test]
    fn test_gpu_result_serializes() {
        let r = unavailable();
        let json = serde_json::to_string(&r).expect("GpuResult must serialize");
        assert!(json.contains("level"));
        assert!(json.contains("utilization_pct"));
        assert!(json.contains("source"));
        let back: GpuResult =
            serde_json::from_str(&json).expect("GpuResult must round-trip");
        assert_eq!(back.level, r.level);
        assert_eq!(back.utilization_pct, r.utilization_pct);
        assert_eq!(back.source, r.source);
    }
}
