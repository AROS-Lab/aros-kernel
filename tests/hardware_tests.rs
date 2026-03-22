use aros_kernel::hardware::pressure::{detect_from_ratio, detect_pressure, MemoryPressureLevel};
use aros_kernel::hardware::probe::probe_system;
use aros_kernel::hardware::snapshot::take_snapshot;

#[test]
fn test_real_probe_returns_valid_values() {
    let res = probe_system();
    assert!(res.cpu_count > 0, "CPU count must be > 0");
    assert!(res.ram_total_mb > 0, "Total RAM must be > 0");
    assert!(res.ram_available_mb > 0, "Available RAM must be > 0");
    assert!(
        res.ram_available_mb <= res.ram_total_mb,
        "Available RAM ({}) must be <= total ({})",
        res.ram_available_mb,
        res.ram_total_mb
    );
    // Load averages should be non-negative
    assert!(res.load_avg_1 >= 0.0, "Load avg 1m must be >= 0");
    assert!(res.load_avg_5 >= 0.0, "Load avg 5m must be >= 0");
    assert!(res.load_avg_15 >= 0.0, "Load avg 15m must be >= 0");
}

#[test]
fn test_probe_caching_consistency() {
    // Rapid successive calls should return identical results (2s cache)
    let first = probe_system();
    let second = probe_system();
    let third = probe_system();

    assert_eq!(first.cpu_count, second.cpu_count);
    assert_eq!(second.cpu_count, third.cpu_count);
    assert_eq!(first.ram_total_mb, second.ram_total_mb);
    assert_eq!(second.ram_total_mb, third.ram_total_mb);
    // Available RAM should be identical when cached
    assert_eq!(first.ram_available_mb, second.ram_available_mb);
    // Load averages should be bit-identical from cache
    assert_eq!(first.load_avg_1.to_bits(), second.load_avg_1.to_bits());
    assert_eq!(first.load_avg_5.to_bits(), third.load_avg_5.to_bits());
}

#[test]
fn test_snapshot_has_platform() {
    let snap = take_snapshot();
    assert!(!snap.platform.is_empty(), "Platform field must be non-empty");
    // On macOS it should be "macos", on Linux "linux"
    let valid_platforms = ["macos", "linux", "windows"];
    assert!(
        valid_platforms.contains(&snap.platform.as_str()),
        "Platform '{}' should be a recognized OS",
        snap.platform
    );
}

#[test]
fn test_snapshot_json_roundtrip() {
    let snap = take_snapshot();
    let json = serde_json::to_string(&snap).expect("Snapshot must serialize to JSON");

    // Deserialize back
    let deserialized: serde_json::Value =
        serde_json::from_str(&json).expect("JSON must be valid");

    // Verify key fields survived roundtrip
    assert_eq!(
        deserialized["resources"]["cpu_count"].as_u64().unwrap(),
        snap.resources.cpu_count as u64
    );
    assert_eq!(
        deserialized["resources"]["ram_total_mb"].as_u64().unwrap(),
        snap.resources.ram_total_mb
    );
    assert_eq!(
        deserialized["platform"].as_str().unwrap(),
        snap.platform
    );
    assert_eq!(
        deserialized["timestamp"].as_str().unwrap(),
        snap.timestamp
    );

    // Verify timestamp looks like ISO 8601
    let ts = snap.timestamp;
    assert!(ts.contains('T'), "Timestamp should contain 'T' separator: {}", ts);
    assert!(ts.ends_with('Z'), "Timestamp should end with 'Z' (UTC): {}", ts);
}

#[test]
fn test_pressure_detection_real() {
    let resources = probe_system();
    let result = detect_pressure(&resources);

    // Must return a valid level
    match result.level {
        MemoryPressureLevel::Normal
        | MemoryPressureLevel::Warn
        | MemoryPressureLevel::Critical => {}
    }

    // Conservative RAM should be <= available RAM
    assert!(
        result.ram_available_conservative_mb <= resources.ram_available_mb,
        "Conservative RAM ({}) must be <= available ({})",
        result.ram_available_conservative_mb,
        resources.ram_available_mb
    );

    // Source should be non-empty
    assert!(!result.source.is_empty(), "Pressure source must be non-empty");
}

#[test]
fn test_pressure_level_ordering() {
    // Test that severity increases: Normal < Warn < Critical
    // We verify this via detect_from_ratio boundaries

    // Normal: < 60% used
    let normal = detect_from_ratio(10000, 5000); // 50% used
    assert_eq!(normal.level, MemoryPressureLevel::Normal);

    // Warn: 60%..85% used
    let warn = detect_from_ratio(10000, 3000); // 70% used
    assert_eq!(warn.level, MemoryPressureLevel::Warn);

    // Critical: >= 85% used
    let critical = detect_from_ratio(10000, 1000); // 90% used
    assert_eq!(critical.level, MemoryPressureLevel::Critical);

    // Edge case: 0 total RAM -> Critical
    let zero_total = detect_from_ratio(0, 0);
    assert_eq!(zero_total.level, MemoryPressureLevel::Critical);

    // Edge case: available > total (shouldn't happen but shouldn't panic)
    let over = detect_from_ratio(1000, 2000);
    // saturating_sub means used=0, ratio=0 -> Normal
    assert_eq!(over.level, MemoryPressureLevel::Normal);

    // Boundary: exactly 60% used -> Warn
    let boundary_warn = detect_from_ratio(10000, 4000);
    assert_eq!(boundary_warn.level, MemoryPressureLevel::Warn);

    // Boundary: exactly 85% used -> Critical
    let boundary_critical = detect_from_ratio(10000, 1500);
    assert_eq!(boundary_critical.level, MemoryPressureLevel::Critical);
}
