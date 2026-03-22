use serde::{Deserialize, Serialize};

use super::probe::{probe_system, SystemResources};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareSnapshot {
    pub resources: SystemResources,
    pub timestamp: String,
    pub platform: String,
}

/// Take a hardware snapshot with current timestamp and platform info.
pub fn take_snapshot() -> HardwareSnapshot {
    HardwareSnapshot {
        resources: probe_system(),
        timestamp: {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            // ISO 8601 without pulling in chrono — use simple formatting
            let secs = now.as_secs();
            // Format as RFC 3339 manually isn't ideal; use a simpler approach
            format_iso8601(secs)
        },
        platform: std::env::consts::OS.to_string(),
    }
}

/// Format unix timestamp as ISO 8601 (UTC).
fn format_iso8601(epoch_secs: u64) -> String {
    // Basic UTC datetime calculation
    let secs_per_min = 60u64;
    let secs_per_hour = 3600u64;
    let secs_per_day = 86400u64;

    let mut days = epoch_secs / secs_per_day;
    let day_secs = epoch_secs % secs_per_day;
    let hours = day_secs / secs_per_hour;
    let minutes = (day_secs % secs_per_hour) / secs_per_min;
    let seconds = day_secs % secs_per_min;

    // Calculate year/month/day from days since 1970-01-01
    let mut year = 1970i32;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let month_days: [u64; 12] = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if days < md {
            month = i;
            break;
        }
        days -= md;
    }

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month + 1,
        days + 1,
        hours,
        minutes,
        seconds
    )
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_serializes() {
        let snap = take_snapshot();
        let json = serde_json::to_string(&snap);
        assert!(json.is_ok(), "Snapshot must serialize to JSON");
        let json_str = json.unwrap();
        assert!(json_str.contains("cpu_count"));
        assert!(json_str.contains("ram_total_mb"));
        assert!(json_str.contains("timestamp"));
        assert!(json_str.contains("platform"));
    }
}
