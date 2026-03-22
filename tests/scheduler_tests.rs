use aros_kernel::hardware::probe::probe_system;
use aros_kernel::hardware::pressure::detect_pressure;
use aros_kernel::scheduler::admission::{
    AdmissionController, MemoryPressureLevel, ResourceRequirements,
};
use aros_kernel::scheduler::allocator::ResourceAllocator;
use aros_kernel::scheduler::recommender::Recommender;

#[test]
fn test_admission_with_real_probe() {
    let resources = probe_system();
    let ctrl = AdmissionController::new(10);
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::shell(); // lightweight: 200mc, 50MB

    // With real hardware, a shell task should be schedulable if we have any resources
    let can = ctrl.can_schedule(
        &req,
        &alloc,
        MemoryPressureLevel::Normal,
        (resources.cpu_count as u32) * 1000, // total CPU millicores
        resources.ram_available_mb as u32,
    );

    // On any real machine with > 200mc CPU and > 50MB RAM, this should succeed
    assert!(
        can,
        "Should be able to schedule a shell agent on real hardware (cpu={}, ram={}MB)",
        resources.cpu_count,
        resources.ram_available_mb
    );
}

#[test]
fn test_allocator_multiple_agents() {
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::claude_cli(); // 500mc, 250MB

    // Allocate 5 agents
    for _ in 0..5 {
        alloc.allocate(&req);
    }
    assert_eq!(alloc.active_agents(), 5);
    assert_eq!(alloc.allocated_cpu(), 2500); // 5 * 500
    assert_eq!(alloc.allocated_memory(), 1250); // 5 * 250

    // Release 3
    for _ in 0..3 {
        alloc.release(&req);
    }
    assert_eq!(alloc.active_agents(), 2);
    assert_eq!(alloc.allocated_cpu(), 1000); // 2 * 500
    assert_eq!(alloc.allocated_memory(), 500); // 2 * 250

    // Release remaining 2
    for _ in 0..2 {
        alloc.release(&req);
    }
    assert_eq!(alloc.active_agents(), 0);
    assert_eq!(alloc.allocated_cpu(), 0);
    assert_eq!(alloc.allocated_memory(), 0);
}

#[test]
fn test_admission_rejects_at_limit() {
    let ctrl = AdmissionController::new(2); // hard limit = 2
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::shell();

    // Allocate up to the limit
    alloc.allocate(&req);
    alloc.allocate(&req);
    assert_eq!(alloc.active_agents(), 2);

    // Third allocation should be rejected by admission
    let can = ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Normal, 4000, 8000);
    assert!(
        !can,
        "Should reject scheduling when at hard limit of 2 agents"
    );

    // Release one, should allow scheduling again
    alloc.release(&req);
    let can = ctrl.can_schedule(&req, &alloc, MemoryPressureLevel::Normal, 4000, 8000);
    assert!(
        can,
        "Should allow scheduling after releasing one agent"
    );
}

#[test]
fn test_recommender_with_real_hardware() {
    let resources = probe_system();
    let recommender = Recommender::with_defaults(); // headroom=2048MB, no hard limit

    let max = recommender.recommend_max_agents(
        &resources,
        MemoryPressureLevel::Normal,
        250, // claude_cli RAM
    );

    // On any real machine, recommendation should be >= 0
    // On a machine with at least 4GB RAM and 2+ CPUs, we expect > 0
    if resources.ram_available_mb > 2048 + 250 && resources.cpu_count >= 1 {
        assert!(
            max > 0,
            "With {}MB available RAM and {} CPUs, should recommend > 0 agents",
            resources.ram_available_mb,
            resources.cpu_count
        );
    }

    // Should never exceed CPU limit (cpu_count * 2)
    let cpu_limit = (resources.cpu_count * 2) as u32;
    assert!(
        max <= cpu_limit,
        "Recommendation ({}) should not exceed CPU limit ({})",
        max,
        cpu_limit
    );

    // Verify warn pressure reduces recommendation
    let max_warn = recommender.recommend_max_agents(
        &resources,
        MemoryPressureLevel::Warn,
        250,
    );
    assert!(
        max_warn <= max,
        "Warn pressure ({}) should produce <= Normal recommendation ({})",
        max_warn,
        max
    );

    // Verify critical pressure reduces even further
    let max_critical = recommender.recommend_max_agents(
        &resources,
        MemoryPressureLevel::Critical,
        250,
    );
    assert!(
        max_critical <= max_warn,
        "Critical pressure ({}) should produce <= Warn recommendation ({})",
        max_critical,
        max_warn
    );
}

#[test]
fn test_recommender_zero_ram_per_agent() {
    let resources = probe_system();
    let recommender = Recommender::with_defaults();

    // 0 ram_per_agent should not panic (no division by zero)
    let max = recommender.recommend_max_agents(
        &resources,
        MemoryPressureLevel::Normal,
        0, // edge case
    );

    // With 0 ram_per_agent, RAM limit becomes u32::MAX, so CPU limit dominates
    let cpu_limit = (resources.cpu_count * 2) as u32;
    assert_eq!(
        max, cpu_limit,
        "With 0 ram_per_agent, recommendation should equal CPU limit ({})",
        cpu_limit
    );
}

#[test]
fn test_full_scheduling_flow() {
    // End-to-end flow: probe -> pressure -> admission -> allocate -> release

    // Step 1: Probe real hardware
    let resources = probe_system();
    assert!(resources.cpu_count > 0);
    assert!(resources.ram_total_mb > 0);

    // Step 2: Detect pressure
    let pressure = detect_pressure(&resources);
    // Map hardware pressure level to scheduler pressure level
    let sched_pressure = match pressure.level {
        aros_kernel::hardware::pressure::MemoryPressureLevel::Normal => {
            MemoryPressureLevel::Normal
        }
        aros_kernel::hardware::pressure::MemoryPressureLevel::Warn => {
            MemoryPressureLevel::Warn
        }
        aros_kernel::hardware::pressure::MemoryPressureLevel::Critical => {
            MemoryPressureLevel::Critical
        }
    };

    // Step 3: Set up admission controller and allocator
    let ctrl = AdmissionController::new(5);
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::shell(); // 200mc, 50MB

    // Step 4: Check admission (only if not critical pressure)
    let available_cpu = (resources.cpu_count as u32) * 1000;
    let available_mem = resources.ram_available_mb as u32;

    if sched_pressure != MemoryPressureLevel::Critical {
        let can = ctrl.can_schedule(&req, &alloc, sched_pressure, available_cpu, available_mem);
        assert!(can, "Should be able to schedule under non-critical pressure");

        // Step 5: Allocate
        alloc.allocate(&req);
        assert_eq!(alloc.active_agents(), 1);
        assert_eq!(alloc.allocated_cpu(), 200);
        assert_eq!(alloc.allocated_memory(), 50);

        // Step 6: Release
        alloc.release(&req);
        assert_eq!(alloc.active_agents(), 0);
        assert_eq!(alloc.allocated_cpu(), 0);
        assert_eq!(alloc.allocated_memory(), 0);
    }

    // Step 7: Verify recommender works with real data
    let recommender = Recommender::with_defaults();
    let recommended = recommender.recommend_max_agents(&resources, sched_pressure, 250);
    // Just verify it doesn't panic and returns a reasonable value
    assert!(
        recommended <= (resources.cpu_count as u32) * 2,
        "Recommended ({}) should be bounded by CPU limit",
        recommended
    );
}

#[test]
fn test_allocator_release_underflow_safety() {
    // Verify that releasing more than allocated doesn't underflow (uses saturating_sub)
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::claude_cli();

    // Release without allocating - should not panic
    alloc.release(&req);
    assert_eq!(alloc.active_agents(), 0);
    assert_eq!(alloc.allocated_cpu(), 0);
    assert_eq!(alloc.allocated_memory(), 0);
}

#[test]
fn test_available_slots_tracks_allocations() {
    let ctrl = AdmissionController::new(5);
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::claude_cli(); // 500mc, 250MB

    let initial_slots =
        ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Normal, 2000, 2000);
    // cpu: 2000/500 = 4, mem: 2000/250 = 8, agent_headroom: 5 -> min = 4
    assert_eq!(initial_slots, 4);

    // Allocate 2 agents
    alloc.allocate(&req);
    alloc.allocate(&req);

    let remaining_slots =
        ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Normal, 2000, 2000);
    // agent_headroom: 5-2 = 3, cpu: 4, mem: 8 -> min = 3
    assert_eq!(remaining_slots, 3);

    // Under warn pressure, same slots (warn doesn't block, only critical does)
    let warn_slots =
        ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Warn, 2000, 2000);
    assert_eq!(warn_slots, 3);

    // Under critical pressure, 0 slots
    let critical_slots =
        ctrl.available_slots(&req, &alloc, MemoryPressureLevel::Critical, 2000, 2000);
    assert_eq!(critical_slots, 0);
}
