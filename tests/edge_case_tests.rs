use std::thread;
use std::time::Duration;

use aros_kernel::agent::lifecycle::AgentState;
use aros_kernel::dag::graph::{AgentLevel, DagError, DagGraph, Node, NodeStatus};
use aros_kernel::hardware::pressure::{detect_from_ratio, MemoryPressureLevel};
use aros_kernel::hardware::probe::{probe_system, SystemResources};
use aros_kernel::scheduler::admission::{
    AdmissionController, MemoryPressureLevel as AdmissionPressureLevel, ResourceRequirements,
};
use aros_kernel::scheduler::allocator::ResourceAllocator;
use aros_kernel::scheduler::recommender::Recommender;

// ── Helpers ──────────────────────────────────────────────────────────

fn make_node(id: &str, deps: Vec<&str>) -> Node {
    Node {
        id: id.to_string(),
        title: format!("Task {id}"),
        description: String::new(),
        depends_on: deps.into_iter().map(String::from).collect(),
        status: NodeStatus::Pending,
        agent_level: AgentLevel::Agent,
        output_files: vec![],
        retry_count: 0,
        result: None,
    }
}

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

// ── Tests ────────────────────────────────────────────────────────────

/// 1. Probe cache expiration: after sleeping past CACHE_DURATION (2s),
///    a fresh probe should still return valid data.
#[test]
fn test_probe_cache_expiration() {
    let first = probe_system();
    assert!(first.cpu_count > 0, "First probe: cpu_count must be > 0");

    thread::sleep(Duration::from_millis(2100));

    let second = probe_system();
    assert!(second.cpu_count > 0, "Second probe: cpu_count must be > 0");
    assert!(
        second.ram_total_mb > 0,
        "Second probe: ram_total_mb must be > 0"
    );
}

/// 2. Pressure boundary: 60% usage → Warn.
///    10000 total, 4000 available → 6000 used → 60% ratio.
#[test]
fn test_pressure_warn_boundary() {
    let result = detect_from_ratio(10000, 4000);
    assert_eq!(
        result.level,
        MemoryPressureLevel::Warn,
        "60% usage should be Warn, got {:?}",
        result.level
    );
}

/// 3. Pressure boundary: 85% usage → Critical.
///    10000 total, 1500 available → 8500 used → 85% ratio.
#[test]
fn test_pressure_critical_boundary() {
    let result = detect_from_ratio(10000, 1500);
    assert_eq!(
        result.level,
        MemoryPressureLevel::Critical,
        "85% usage should be Critical, got {:?}",
        result.level
    );
}

/// 4. An empty DAG is not considered complete.
#[test]
fn test_empty_dag_is_not_complete() {
    let g = DagGraph::new();
    assert!(
        !g.is_complete(),
        "Empty DAG should not be considered complete"
    );
}

/// 5. Self-loop should be detected as a cycle.
#[test]
fn test_cycle_detection_self_loop() {
    let mut g = DagGraph::new();
    let result = g.add_node(make_node("x", vec!["x"]));
    assert!(result.is_err(), "Self-loop should be rejected");
    assert!(
        matches!(result.unwrap_err(), DagError::CycleDetected(_)),
        "Error should be CycleDetected"
    );
}

/// 6. Invalid lifecycle transitions should all fail.
#[test]
fn test_invalid_lifecycle_transitions() {
    // Idle → Done (must be Busy first)
    let mut state = AgentState::Idle;
    assert!(
        state.transition_to_done().is_err(),
        "Idle → Done should fail"
    );

    // Idle → Failed (must be Busy first)
    let mut state = AgentState::Idle;
    assert!(
        state.transition_to_failed().is_err(),
        "Idle → Failed should fail"
    );

    // Done → Busy (terminal state)
    let mut state = AgentState::Idle;
    state.transition_to_busy().unwrap();
    state.transition_to_done().unwrap();
    assert!(
        state.transition_to_busy().is_err(),
        "Done → Busy should fail"
    );

    // Failed → Done (terminal state)
    let mut state = AgentState::Idle;
    state.transition_to_busy().unwrap();
    state.transition_to_failed().unwrap();
    assert!(
        state.transition_to_done().is_err(),
        "Failed → Done should fail"
    );
}

/// 7. Warn pressure should still allow scheduling when resources are plentiful.
#[test]
fn test_can_schedule_warn_pressure() {
    let ctrl = AdmissionController::new(10);
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::claude_cli();

    assert!(
        ctrl.can_schedule(&req, &alloc, AdmissionPressureLevel::Warn, 4000, 8000),
        "Warn pressure with plenty of resources should allow scheduling"
    );
}

/// 8. Exact resource boundary: available == required should still allow scheduling.
#[test]
fn test_exact_resource_boundary() {
    let ctrl = AdmissionController::new(10);
    let alloc = ResourceAllocator::new();
    let req = ResourceRequirements::claude_cli(); // 500 millicores, 250 MB

    assert!(
        ctrl.can_schedule(&req, &alloc, AdmissionPressureLevel::Normal, 500, 250),
        "Exact match of available == required should allow scheduling"
    );
}

/// 9. Warn pressure increases headroom to 1.75x base (2048 * 1.75 = 3584 MB),
///    which reduces the recommendation compared to Normal.
#[test]
fn test_recommender_warn_pressure() {
    let r = Recommender::with_defaults(); // base headroom = 2048
    let res = make_resources(10, 16384, 12000);

    let _normal = r.recommend_max_agents(&res, AdmissionPressureLevel::Normal, 250);
    let _warn = r.recommend_max_agents(&res, AdmissionPressureLevel::Warn, 250);

    // Normal: headroom = 2048, (12000 - 2048) / 250 = 39, min(20, 39) = 20
    // Warn:   headroom = 3584, (12000 - 3584) / 250 = 33, min(20, 33) = 20
    // Both are CPU-limited at 20 with 10 CPUs. Use fewer CPUs to see the difference.
    let res_small = make_resources(100, 16384, 12000);
    let normal_small = r.recommend_max_agents(&res_small, AdmissionPressureLevel::Normal, 250);
    let warn_small = r.recommend_max_agents(&res_small, AdmissionPressureLevel::Warn, 250);

    // Normal: (12000 - 2048) / 250 = 39, min(200, 39) = 39
    // Warn:   (12000 - 3584) / 250 = 33, min(200, 33) = 33
    assert_eq!(normal_small, 39, "Normal pressure recommendation");
    assert_eq!(warn_small, 33, "Warn pressure recommendation");
    assert!(
        warn_small < normal_small,
        "Warn should reduce recommendation vs Normal"
    );
}

/// 10. Single CPU system: cpu_limit = 2, so recommendation is min(2, ram_limit).
#[test]
fn test_recommender_single_cpu() {
    let r = Recommender::with_defaults(); // base headroom = 2048
    let res = make_resources(1, 16384, 16384);

    let max = r.recommend_max_agents(&res, AdmissionPressureLevel::Normal, 250);

    // CPU limit = 1 * 2 = 2
    // RAM limit = (16384 - 2048) / 250 = 57
    // min(2, 57) = 2
    assert_eq!(max, 2, "Single CPU should cap at cpu_count * 2 = 2");
}
