# AROS Kernel

Hardware-aware agent runtime engine for the AROS ecosystem. Manages execution of AI agents (Claude CLI, shell) on resource-constrained systems with memory pressure detection, admission control, and parallel DAG execution.

## Architecture

```
┌─────────────────────────────────────────────────┐
│                  AROS Kernel                     │
│                                                  │
│  ┌──────────┐  ┌───────────┐  ┌──────────────┐ │
│  │ Hardware  │  │ Scheduler │  │     DAG      │ │
│  │ Monitor   │→│ Admission │→│   Executor    │ │
│  │           │  │ Control   │  │              │ │
│  └──────────┘  └───────────┘  └──────────────┘ │
│       ↓              ↓              ↓            │
│  ┌──────────────────────────────────────────┐   │
│  │            Agent Lifecycle                │   │
│  │  ┌──────────┐  ┌─────────────────────┐   │   │
│  │  │  Shell   │  │   Claude CLI Agent  │   │   │
│  │  │  Agent   │  │  (subprocess mgmt)  │   │   │
│  │  └──────────┘  └─────────────────────┘   │   │
│  └──────────────────────────────────────────┘   │
└─────────────────────────────────────────────────┘
```

## Modules

### Hardware (`src/hardware/`)

| Component | Description |
|-----------|-------------|
| **SystemResources** | CPU count, total/available RAM, load averages (2s cache) |
| **MemoryPressureLevel** | Normal / Warn / Critical detection via macOS `sysctl` |
| **HardwareSnapshot** | Timestamped system state, serializable to JSON |

macOS-specific: reads `kern.memorystatus_level` and `vm.compressor_bytes_used` for accurate pressure detection. Falls back to RAM ratio on other platforms.

### Scheduler (`src/scheduler/`)

| Component | Description |
|-----------|-------------|
| **AdmissionController** | Enforces CPU/RAM/pressure limits before scheduling |
| **ResourceAllocator** | Thread-safe tracking of allocated resources (`Arc<Mutex<>>`) |
| **Recommender** | Calculates max agents based on hardware + pressure headroom |

Resource requirements per agent type:
- **Claude CLI**: 500mc CPU, 250MB RAM
- **Shell**: 200mc CPU, 50MB RAM

Pressure-aware headroom multiplier: Normal=1x, Warn=1.75x, Critical=2.5x.

### DAG Engine (`src/dag/`)

| Component | Description |
|-----------|-------------|
| **DagGraph** | Directed acyclic graph with cycle detection (DFS) and topological sort (Kahn's) |
| **DagExecutor** | Async parallel task execution via Tokio, respects max_parallel |
| **RuntimeDag** | `Arc<RwLock<>>` wrapper for safe concurrent mutation with rollback |
| **DagPersistence** | JSON checkpoint/resume with crash recovery (InProgress → Pending) |

Node states: `Pending → InProgress → Done | Failed | Blocked`

### Agent (`src/agent/`)

| Component | Description |
|-----------|-------------|
| **AgentType** trait | `execute(task, timeout)` + `resource_requirements()` |
| **AgentState** | Finite state machine: Idle → Busy → Done/Failed → Reset |
| **ClaudeCliAgent** | Subprocess management with stdin/stdout piping, timeout |
| **ShellAgent** | `/bin/sh -c` execution with working directory support |

## Usage

```bash
# Build
cargo build --release

# Run tests
cargo test

# Show recommended agent capacity
cargo run -- recommend

# Show system status
cargo run -- status
```

## Key Design Decisions

- **Zero external runtime deps** beyond tokio, sysinfo, serde — minimal footprint for headless Mac Mini
- **Pressure-aware scheduling** — rejects all work at Critical pressure, scales headroom dynamically
- **Crash recovery** — DAG checkpoints reset InProgress nodes to Pending on reload
- **Thread-safe by default** — `Arc<Mutex<>>` / `Arc<RwLock<>>` throughout
- **Trait-based agents** — `AgentType` trait enables custom agent backends

## Test Suite

9 test files covering:
- DAG parallel dispatch and dependency resolution
- Admission control under memory pressure
- Agent lifecycle state transitions
- Thread-safety with concurrent allocations
- Stress tests and edge cases
- Checkpoint save/load and crash recovery

## License

Private — AROS-Lab
