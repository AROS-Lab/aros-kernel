use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use aros_kernel::dag::graph::{AgentLevel, DagGraph, Node, NodeResult, NodeStatus};
use aros_kernel::dag::persistence::DagPersistence;
use aros_kernel::dag::runtime::RuntimeDag;
use aros_kernel::hardware::pressure::{detect_from_ratio, MemoryPressureLevel};
use aros_kernel::hardware::probe::probe_system;
use aros_kernel::scheduler::admission::{AdmissionController, MemoryPressureLevel as AdmPressure, ResourceRequirements};
use aros_kernel::scheduler::allocator::ResourceAllocator;
use aros_kernel::agent::lifecycle::AgentState;

use tempfile::TempDir;

// ── Helpers ─────────────────────────────────────────────────────────

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

fn mock_executor(delay_ms: u64) -> aros_kernel::dag::executor::TaskExecutor {
    Arc::new(move |node: Node| {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            NodeResult {
                success: true,
                output: format!("Completed {}", node.id),
                error: None,
                duration_secs: delay_ms as f64 / 1000.0,
            }
        })
    })
}

// ── Test 1: Concurrent probe calls ──────────────────────────────────

#[test]
fn test_concurrent_probe_calls() {
    let handles: Vec<_> = (0..20)
        .map(|_| {
            thread::spawn(|| {
                let res = probe_system();
                res.cpu_count
            })
        })
        .collect();

    for h in handles {
        let cpu_count = h.join().expect("thread panicked during probe_system()");
        assert!(cpu_count > 0, "cpu_count must be > 0, got {cpu_count}");
    }
}

// ── Test 2: Concurrent allocator stress ─────────────────────────────

#[test]
fn test_concurrent_allocator_stress() {
    let allocator = ResourceAllocator::new();

    let handles: Vec<_> = (0..50)
        .map(|_| {
            let alloc = allocator.clone();
            thread::spawn(move || {
                let req = ResourceRequirements::shell();
                for _ in 0..10 {
                    alloc.allocate(&req);
                    alloc.release(&req);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked during allocator stress");
    }

    assert_eq!(
        allocator.active_agents(),
        0,
        "active_agents should be 0 after balanced allocate/release"
    );
    assert_eq!(
        allocator.allocated_cpu(),
        0,
        "allocated_cpu should be 0 after balanced allocate/release"
    );
}

// ── Test 3: Concurrent admission decisions ──────────────────────────

#[test]
fn test_concurrent_admission_decisions() {
    let allocator = ResourceAllocator::new();
    let controller = AdmissionController::new(100);
    let req = ResourceRequirements::shell();

    // Spawn 10 threads that repeatedly call can_schedule
    let controller = Arc::new(controller);
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let alloc = allocator.clone();
            let r = req.clone();
            let ctrl = Arc::clone(&controller);
            thread::spawn(move || {
                for _ in 0..50 {
                    let _ = ctrl.can_schedule(
                        &r,
                        &alloc,
                        AdmPressure::Normal,
                        4000,
                        8000,
                    );
                }
            })
        })
        .collect();

    // Main thread does allocate/release concurrently
    for _ in 0..50 {
        allocator.allocate(&req);
        allocator.release(&req);
    }

    for h in handles {
        h.join().expect("thread panicked during concurrent admission");
    }
}

// ── Test 4: Concurrent lifecycle transitions ────────────────────────

#[test]
fn test_concurrent_lifecycle_transitions() {
    let state = Arc::new(Mutex::new(AgentState::Idle));
    let success_count = Arc::new(Mutex::new(0u32));

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let s = Arc::clone(&state);
            let count = Arc::clone(&success_count);
            thread::spawn(move || {
                let mut guard = s.lock().unwrap();
                if guard.transition_to_busy().is_ok() {
                    let mut c = count.lock().unwrap();
                    *c += 1;
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked during lifecycle transition");
    }

    let final_count = *success_count.lock().unwrap();
    assert_eq!(
        final_count, 1,
        "exactly 1 thread should succeed in transition_to_busy, got {final_count}"
    );

    let final_state = *state.lock().unwrap();
    assert_eq!(final_state, AgentState::Busy);
}

// ── Test 5: Concurrent DAG mutations ────────────────────────────────

#[tokio::test]
async fn test_concurrent_dag_mutations() {
    let graph_arc = Arc::new(tokio::sync::RwLock::new(DagGraph::new()));

    // Add 2 initial nodes
    {
        let mut g = graph_arc.write().await;
        g.add_node(make_node("n1", vec![])).unwrap();
        g.add_node(make_node("n2", vec![])).unwrap();
    }

    let rt = Arc::new(RuntimeDag::new(graph_arc.clone()));

    // Task 1: add 3 more nodes via RuntimeDag
    let rt_clone = Arc::clone(&rt);
    let add_handle = tokio::spawn(async move {
        for i in 3..=5 {
            rt_clone
                .add_node(make_node(&format!("n{i}"), vec![]))
                .await
                .unwrap();
        }
    });

    // Task 2: run executor concurrently
    let graph_for_exec = graph_arc.clone();
    let exec_handle = tokio::spawn(async move {
        use aros_kernel::dag::executor::DagExecutor;
        let executor = DagExecutor::new(graph_for_exec, 10);
        let task_fn = mock_executor(5);
        executor.run(task_fn).await
    });

    // Wait for the add task to complete first
    add_handle.await.unwrap();

    // Now wait for the executor — it should eventually complete all 5 nodes
    let exec_result = exec_handle.await.unwrap();
    assert!(
        exec_result.is_ok(),
        "executor should complete: {:?}",
        exec_result
    );

    let g = graph_arc.read().await;
    assert_eq!(g.node_count(), 5, "should have 5 nodes total");
    assert!(g.is_complete(), "all 5 nodes should be Done");
}

// ── Test 6: Concurrent pressure detection ───────────────────────────

#[test]
fn test_concurrent_pressure_detection() {
    let handles: Vec<_> = (0..10)
        .map(|_| {
            thread::spawn(|| {
                let result = detect_from_ratio(16384, 8192);
                result.level
            })
        })
        .collect();

    let mut levels = Vec::new();
    for h in handles {
        let level = h.join().expect("thread panicked during pressure detection");
        levels.push(level);
    }

    // All should return the same level (deterministic inputs)
    let first = levels[0];
    for (i, level) in levels.iter().enumerate() {
        assert_eq!(
            *level, first,
            "thread {i} returned {:?} but expected {:?}",
            level, first
        );
    }

    // With 50% usage (8192/16384), should be Normal (< 60% threshold)
    assert_eq!(first, MemoryPressureLevel::Normal);
}

// ── Test 7: Concurrent save/load ────────────────────────────────────

#[test]
fn test_concurrent_save_load() {
    let tmp = TempDir::new().unwrap();
    let base_path = tmp.path().join(".aros");

    // Create and save an initial checkpoint so loads don't fail on missing file
    let mut graph = DagGraph::new();
    graph.add_node(make_node("a", vec![])).unwrap();
    graph.add_node(make_node("b", vec!["a"])).unwrap();
    graph.add_node(make_node("c", vec!["a"])).unwrap();

    let persistence = DagPersistence::new(base_path.clone());
    persistence.save_checkpoint(&graph).unwrap();

    let graph = Arc::new(graph);
    let base = Arc::new(base_path);

    let mut handles = Vec::new();

    // 5 threads saving
    for _ in 0..5 {
        let g = Arc::clone(&graph);
        let b = Arc::clone(&base);
        handles.push(thread::spawn(move || {
            let p = DagPersistence::new(b.as_ref().clone());
            for _ in 0..5 {
                p.save_checkpoint(&g).unwrap();
            }
        }));
    }

    // 5 threads loading
    for _ in 0..5 {
        let b = Arc::clone(&base);
        handles.push(thread::spawn(move || {
            let p = DagPersistence::new(b.as_ref().clone());
            for _ in 0..5 {
                // Load may transiently fail if save is mid-write; that's OK
                // for this test — we just verify no panics.
                let _ = p.load_checkpoint();
            }
        }));
    }

    for h in handles {
        h.join().expect("thread panicked during concurrent save/load");
    }
}

// ── Test 8: Allocator interleaved (deadlock detection) ──────────────

#[test]
fn test_allocator_interleaved() {
    let allocator = ResourceAllocator::new();
    let req = ResourceRequirements::shell();

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let alloc = allocator.clone();
            let r = req.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    if i % 2 == 1 {
                        // Odd threads allocate
                        alloc.allocate(&r);
                    } else {
                        // Even threads release (saturating_sub handles underflow)
                        alloc.release(&r);
                    }
                }
            })
        })
        .collect();

    // Use a watchdog thread with 2s timeout to detect deadlocks
    let (tx, rx) = std::sync::mpsc::channel();
    let watchdog = thread::spawn(move || {
        for h in handles {
            h.join().expect("thread panicked in interleaved test");
        }
        tx.send(()).unwrap();
    });

    match rx.recv_timeout(Duration::from_secs(2)) {
        Ok(()) => {
            watchdog.join().unwrap();
        }
        Err(_) => {
            panic!("DEADLOCK DETECTED: interleaved allocate/release did not complete within 2s");
        }
    }
}
