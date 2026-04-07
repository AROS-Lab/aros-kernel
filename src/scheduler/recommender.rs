use crate::hardware::probe::SystemResources;
use crate::hardware::pressure::MemoryPressureLevel;

pub struct Recommender {
    ram_headroom_base_mb: u32,
    max_agents_hard_limit: u32,
}

impl Recommender {
    pub fn new(headroom_mb: u32, max_agents: u32) -> Self {
        Self {
            ram_headroom_base_mb: headroom_mb,
            max_agents_hard_limit: max_agents,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(2048, 0)
    }

    /// Calculate dynamic headroom based on pressure level.
    /// Normal = 1x, Warn = 1.75x, Critical = 2.5x base headroom.
    fn dynamic_headroom(&self, pressure: MemoryPressureLevel) -> u32 {
        let multiplier = match pressure {
            MemoryPressureLevel::Normal => 1.0,
            MemoryPressureLevel::Warn => 1.75,
            MemoryPressureLevel::Critical => 2.5,
        };
        (self.ram_headroom_base_mb as f64 * multiplier) as u32
    }

    /// Recommend max agents for given hardware state.
    ///
    /// Takes the minimum of:
    /// - CPU limit: `cpu_count * 2`
    /// - RAM limit: `(available_ram - dynamic_headroom) / ram_per_agent`
    /// - Hard limit (if set)
    pub fn recommend_max_agents(
        &self,
        resources: &SystemResources,
        pressure: MemoryPressureLevel,
        ram_per_agent_mb: u32,
    ) -> u32 {
        let cpu_limit = (resources.cpu_count * 2) as u32;
        let headroom = self.dynamic_headroom(pressure);
        let available = resources.ram_available_mb.saturating_sub(headroom as u64);
        let ram_limit = if ram_per_agent_mb > 0 {
            (available / ram_per_agent_mb as u64) as u32
        } else {
            u32::MAX
        };
        let recommended = cpu_limit.min(ram_limit);
        if self.max_agents_hard_limit > 0 {
            recommended.min(self.max_agents_hard_limit)
        } else {
            recommended
        }
    }

    /// Generate a formatted capacity summary for the given hardware state.
    pub fn recommend_config(
        &self,
        resources: &SystemResources,
        pressure: MemoryPressureLevel,
    ) -> String {
        let claude_cli_ram = 250_u32;
        let shell_ram = 50_u32;
        let max_claude = self.recommend_max_agents(resources, pressure, claude_cli_ram);
        let max_shell = self.recommend_max_agents(resources, pressure, shell_ram);
        let headroom = self.dynamic_headroom(pressure);

        let platform = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        format!(
            "\
┌─────────────────────────────────────────┐
│          HARDWARE SUMMARY               │
├─────────────────────────────────────────┤
│ Platform: {platform}-{arch}
│ CPUs:     {cpus}
│ RAM:      {total} MB total, {avail} MB available
│ Pressure: {pressure:?}
├─────────────────────────────────────────┤
│       AGENT CAPACITY BY TYPE            │
├─────────────────────────────────────────┤
│ claude_cli ({claude_ram} MB/agent): max {max_claude}
│ shell      ({shell_ram} MB/agent):  max {max_shell}
├─────────────────────────────────────────┤
│     RECOMMENDED CONFIGURATION           │
├─────────────────────────────────────────┤
│ Type:     claude_cli
│ Max:      {max_claude}
│ Headroom: {headroom} MB
└─────────────────────────────────────────┘",
            platform = platform,
            arch = arch,
            cpus = resources.cpu_count,
            total = resources.ram_total_mb,
            avail = resources.ram_available_mb,
            pressure = pressure,
            claude_ram = claude_cli_ram,
            shell_ram = shell_ram,
            max_claude = max_claude,
            max_shell = max_shell,
            headroom = headroom,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_resources(cpu_count: usize, ram_total_mb: u64, ram_available_mb: u64) -> SystemResources {
        SystemResources {
            cpu_count,
            ram_total_mb,
            ram_available_mb,
            load_avg_1: 1.0,
            load_avg_5: 1.0,
            load_avg_15: 1.0,
        }
    }

    #[test]
    fn test_recommend_ample_resources() {
        let r = Recommender::with_defaults();
        let res = make_resources(10, 16384, 12000);
        let max = r.recommend_max_agents(&res, MemoryPressureLevel::Normal, 250);
        // CPU limit = 20, RAM limit = (12000 - 2048) / 250 = 39 → min = 20
        assert_eq!(max, 20);
    }

    #[test]
    fn test_recommend_low_ram() {
        let r = Recommender::with_defaults();
        let res = make_resources(10, 4096, 3072);
        let max = r.recommend_max_agents(&res, MemoryPressureLevel::Normal, 250);
        // CPU limit = 20, RAM limit = (3072 - 2048) / 250 = 4 → min = 4
        assert_eq!(max, 4);
    }

    #[test]
    fn test_recommend_high_cpu() {
        let r = Recommender::with_defaults();
        let res = make_resources(2, 32768, 28000);
        let max = r.recommend_max_agents(&res, MemoryPressureLevel::Normal, 250);
        // CPU limit = 4, RAM limit = (28000 - 2048) / 250 = 103 → min = 4
        assert_eq!(max, 4);
    }

    #[test]
    fn test_recommend_critical_pressure() {
        let r = Recommender::with_defaults();
        let res = make_resources(10, 16384, 8000);
        let normal = r.recommend_max_agents(&res, MemoryPressureLevel::Normal, 250);
        let critical = r.recommend_max_agents(&res, MemoryPressureLevel::Critical, 250);
        // Normal headroom = 2048, Critical headroom = 5120
        // Normal: (8000 - 2048) / 250 = 23, min(20, 23) = 20
        // Critical: (8000 - 5120) / 250 = 11, min(20, 11) = 11
        assert_eq!(normal, 20);
        assert_eq!(critical, 11);
        assert!(critical < normal, "Critical pressure should reduce capacity");
    }

    #[test]
    fn test_recommend_hard_limit() {
        let r = Recommender::new(2048, 3);
        let res = make_resources(10, 16384, 12000);
        let max = r.recommend_max_agents(&res, MemoryPressureLevel::Normal, 250);
        // Without limit would be 20, but hard limit is 3
        assert_eq!(max, 3);
    }

    #[test]
    fn test_recommend_config_format() {
        let r = Recommender::with_defaults();
        let res = make_resources(8, 16384, 10000);
        let output = r.recommend_config(&res, MemoryPressureLevel::Normal);
        assert!(output.contains("HARDWARE SUMMARY"), "Should contain hardware summary section");
        assert!(output.contains("AGENT CAPACITY BY TYPE"), "Should contain agent capacity section");
        assert!(output.contains("RECOMMENDED CONFIGURATION"), "Should contain recommended config section");
        assert!(output.contains("claude_cli"), "Should mention claude_cli agent type");
        assert!(output.contains("shell"), "Should mention shell agent type");
        assert!(output.contains("16384"), "Should show total RAM");
    }
}
