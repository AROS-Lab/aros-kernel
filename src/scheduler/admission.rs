use serde::{Deserialize, Serialize};

use super::allocator::ResourceAllocator;
// Re-exported so downstream code can still import from scheduler::admission
pub use crate::hardware::pressure::MemoryPressureLevel;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequirements {
    pub cpu_millicores: u32,
    pub memory_mb: u32,
}

impl ResourceRequirements {
    /// Claude CLI agent: moderate CPU, significant memory
    pub fn claude_cli() -> Self {
        Self {
            cpu_millicores: 500,
            memory_mb: 250,
        }
    }

    /// Shell command: lightweight
    pub fn shell() -> Self {
        Self {
            cpu_millicores: 200,
            memory_mb: 50,
        }
    }
}

pub struct AdmissionController {
    max_agents_hard_limit: u32,
}

impl AdmissionController {
    pub fn new(max_agents: u32) -> Self {
        Self {
            max_agents_hard_limit: max_agents,
        }
    }

    /// Check if a new agent can be scheduled given current state.
    pub fn can_schedule(
        &self,
        req: &ResourceRequirements,
        allocator: &ResourceAllocator,
        pressure: MemoryPressureLevel,
        available_cpu_millicores: u32,
        available_memory_mb: u32,
    ) -> bool {
        if pressure == MemoryPressureLevel::Critical {
            return false;
        }
        if available_cpu_millicores < req.cpu_millicores {
            return false;
        }
        if available_memory_mb < req.memory_mb {
            return false;
        }
        if allocator.active_agents() >= self.max_agents_hard_limit {
            return false;
        }
        true
    }

    /// How many more agents of this type can fit given current resources and limits.
    pub fn available_slots(
        &self,
        req: &ResourceRequirements,
        allocator: &ResourceAllocator,
        pressure: MemoryPressureLevel,
        available_cpu_millicores: u32,
        available_memory_mb: u32,
    ) -> u32 {
        if pressure == MemoryPressureLevel::Critical {
            return 0;
        }

        let agent_headroom = self
            .max_agents_hard_limit
            .saturating_sub(allocator.active_agents());

        let cpu_slots = if req.cpu_millicores > 0 {
            available_cpu_millicores / req.cpu_millicores
        } else {
            u32::MAX
        };

        let mem_slots = if req.memory_mb > 0 {
            available_memory_mb / req.memory_mb
        } else {
            u32::MAX
        };

        agent_headroom.min(cpu_slots).min(mem_slots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_controller() -> AdmissionController {
        AdmissionController::new(5)
    }

    fn make_allocator() -> ResourceAllocator {
        ResourceAllocator::new()
    }

    #[test]
    fn test_can_schedule_normal_pressure() {
        let ctrl = make_controller();
        let alloc = make_allocator();
        let req = ResourceRequirements::claude_cli();

        assert!(ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Normal, 4000, 8000));
    }

    #[test]
    fn test_cannot_schedule_critical_pressure() {
        let ctrl = make_controller();
        let alloc = make_allocator();
        let req = ResourceRequirements::shell();

        // Even with plenty of resources, Critical pressure blocks scheduling
        assert!(!ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Critical, 4000, 8000));
    }

    #[test]
    fn test_cannot_schedule_exhausted_cpu() {
        let ctrl = make_controller();
        let alloc = make_allocator();
        let req = ResourceRequirements::claude_cli(); // needs 500 millicores

        assert!(!ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Normal, 100, 8000));
    }

    #[test]
    fn test_cannot_schedule_exhausted_ram() {
        let ctrl = make_controller();
        let alloc = make_allocator();
        let req = ResourceRequirements::claude_cli(); // needs 250 MB

        assert!(!ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Normal, 4000, 100));
    }

    #[test]
    fn test_cannot_schedule_max_agents() {
        let ctrl = make_controller(); // limit = 5
        let alloc = make_allocator();
        let req = ResourceRequirements::shell();

        // Fill to the limit
        for _ in 0..5 {
            alloc.allocate(&req);
        }

        assert!(!ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Normal, 4000, 8000));
    }

    #[test]
    fn test_available_slots() {
        let ctrl = make_controller(); // limit = 5
        let alloc = make_allocator();
        let req = ResourceRequirements::claude_cli(); // 500mc, 250MB

        // With 2000mc CPU and 1000MB RAM: cpu allows 4, ram allows 4, agent limit 5 → 4
        assert_eq!(
            ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Normal, 2000, 1000),
            4
        );

        // Allocate 2 agents
        alloc.allocate(&req);
        alloc.allocate(&req);
        // agent headroom = 3, cpu still allows 4, ram allows 4 → 3
        assert_eq!(
            ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Normal, 2000, 1000),
            3
        );

        // Critical pressure → 0
        assert_eq!(
            ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Critical, 2000, 1000),
            0
        );
    }
}
