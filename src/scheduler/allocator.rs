use std::sync::{Arc, Mutex};

use super::admission::ResourceRequirements;

#[derive(Debug, Clone)]
pub struct ResourceAllocator {
    inner: Arc<Mutex<AllocatorState>>,
}

#[derive(Debug)]
struct AllocatorState {
    allocated_cpu: u32,
    allocated_memory: u32,
    active_agents: u32,
}

impl ResourceAllocator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AllocatorState {
                allocated_cpu: 0,
                allocated_memory: 0,
                active_agents: 0,
            })),
        }
    }

    pub fn allocate(&self, req: &ResourceRequirements) {
        let mut state = self.inner.lock().unwrap();
        state.allocated_cpu += req.cpu_millicores;
        state.allocated_memory += req.memory_mb;
        state.active_agents += 1;
    }

    pub fn release(&self, req: &ResourceRequirements) {
        let mut state = self.inner.lock().unwrap();
        state.allocated_cpu = state.allocated_cpu.saturating_sub(req.cpu_millicores);
        state.allocated_memory = state.allocated_memory.saturating_sub(req.memory_mb);
        state.active_agents = state.active_agents.saturating_sub(1);
    }

    pub fn active_agents(&self) -> u32 {
        self.inner.lock().unwrap().active_agents
    }

    pub fn allocated_cpu(&self) -> u32 {
        self.inner.lock().unwrap().allocated_cpu
    }

    pub fn allocated_memory(&self) -> u32 {
        self.inner.lock().unwrap().allocated_memory
    }
}

impl Default for ResourceAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_and_release() {
        let allocator = ResourceAllocator::new();
        let req = ResourceRequirements::claude_cli();

        allocator.allocate(&req);
        assert_eq!(allocator.allocated_cpu(), 500);
        assert_eq!(allocator.allocated_memory(), 250);
        assert_eq!(allocator.active_agents(), 1);

        allocator.release(&req);
        assert_eq!(allocator.allocated_cpu(), 0);
        assert_eq!(allocator.allocated_memory(), 0);
        assert_eq!(allocator.active_agents(), 0);
    }

    #[test]
    fn test_active_agents_tracking() {
        let allocator = ResourceAllocator::new();
        let cli = ResourceRequirements::claude_cli();
        let shell = ResourceRequirements::shell();

        allocator.allocate(&cli);
        allocator.allocate(&shell);
        assert_eq!(allocator.active_agents(), 2);
        assert_eq!(allocator.allocated_cpu(), 700);
        assert_eq!(allocator.allocated_memory(), 300);

        allocator.release(&cli);
        assert_eq!(allocator.active_agents(), 1);
        assert_eq!(allocator.allocated_cpu(), 200);
        assert_eq!(allocator.allocated_memory(), 50);

        allocator.release(&shell);
        assert_eq!(allocator.active_agents(), 0);
    }

    #[test]
    fn test_thread_safety() {
        let allocator = ResourceAllocator::new();
        let mut handles = vec![];

        for _ in 0..10 {
            let alloc = allocator.clone();
            handles.push(std::thread::spawn(move || {
                let req = ResourceRequirements::shell();
                alloc.allocate(&req);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(allocator.active_agents(), 10);
        assert_eq!(allocator.allocated_cpu(), 2000);
        assert_eq!(allocator.allocated_memory(), 500);

        // Release all
        let mut handles = vec![];
        for _ in 0..10 {
            let alloc = allocator.clone();
            handles.push(std::thread::spawn(move || {
                let req = ResourceRequirements::shell();
                alloc.release(&req);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(allocator.active_agents(), 0);
        assert_eq!(allocator.allocated_cpu(), 0);
        assert_eq!(allocator.allocated_memory(), 0);
    }
}
