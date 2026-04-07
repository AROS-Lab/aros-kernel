# AROS Kernel

Hardware-aware agent runtime engine for the AROS (Agent Runtime OS) ecosystem. The kernel manages the full lifecycle of AI agent execution — from hardware probing and admission control through DAG orchestration to supervised process management — on resource-constrained systems.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         AROS Kernel                             │
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────┐ │
│  │  Supervisor  │    │  Resource   │    │   JSON-RPC          │ │
│  │  Tree        │───▶│  Governor   │───▶│   Dispatch          │ │
│  │  (init→loop) │    │  (2-phase)  │    │   (UDS server)      │ │
│  └─────────────┘    └─────────────┘    └─────────────────────┘ │
│        │                   │                     │              │
│        ▼                   ▼                     ▼              │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────┐ │
│  │  Task        │    │  Hardware   │    │   Loop Contracts    │ │
│  │  Envelope    │    │  Monitor    │    │   (triggers)        │ │
│  │  (zones)     │    │  (pressure) │    │                     │ │
│  └─────────────┘    └─────────────┘    └─────────────────────┘ │
│        │                   │                     │              │
│        ▼                   ▼                     ▼              │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │                    State Store (SQLite/WAL)               │  │
│  │                    + ACL Enforcement                      │  │
│  └──────────────────────────────────────────────────────────┘  │
│        │                                                        │
│        ▼                                                        │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  DAG Executor  │  Agent Lifecycle  │  Model Adapter      │  │
│  │  (parallel)    │  (Shell/Claude)   │  (circuit breaker)  │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

## Three-Loop Model

The kernel orchestrates three execution loops:

| Loop | Name | Purpose |
|------|------|---------|
| **Loop 0** | Meta | Self-improvement cycle (PERCEIVE → CRITIQUE → REVISE → CHECK → PERSIST) |
| **Loop 1** | Agentic | Single-task agent execution (one subprocess per task) |
| **Loop 2** | Harness | DAG orchestration — dispatches tasks to Loop 1 subprocesses |

Inter-loop communication uses JSON-RPC 2.0 over Unix domain sockets, with the kernel as the routing hub.

## Modules

### Supervisor (`src/supervisor/`)

Process lifecycle management with ordered startup and graceful shutdown.

| Component | Description |
|-----------|-------------|
| **KernelSupervisor** | Registers and tracks all processes (Init, Kernel, Loops 0-2, Adapters) |
| **ProcessState** | State machine: Starting → Running → Stopping → Stopped → Failed → Restarting |
| **RestartPolicy** | Exponential backoff with configurable max restarts and window |
| **HealthStatus** | Aggregated health: Healthy / Degraded / Recovering |
| **KernelRequestHandler** | Routes JSON-RPC triggers between loops with process validation |

### Task Envelope (`src/envelope/`)

Versioned task schema with security and resource constraints.

| Component | Description |
|-----------|-------------|
| **TaskEnvelope** | Wraps every task with metadata: security zone, priority, budget, tools |
| **SecurityZone** | Green (any provider) / Yellow (approved only) / Red (local only) |
| **Priority** | P0 Critical (never shed) / P1 Normal / P2 Background (shed first) |
| **ResourceBudget** | Per-task limits: max RSS, wall time, tokens, warning threshold |

### Resource Governor (`src/governor/`)

Two-phase resource control with priority-aware enforcement.

| Component | Description |
|-----------|-------------|
| **ResourceGovernor** | Phase 1: admission (queue → throttle → shed). Phase 2: runtime budget |
| **TierBudget** | Per-priority budgets: max concurrent, RSS ceiling, hourly tokens |
| **AdmissionDecision** | Admitted / Queued / Throttled / Shed |
| **RuntimeDecision** | Continue / Warning / Exceeded |

### JSON-RPC Dispatch (`src/dispatch/`)

Inter-loop communication over Unix domain sockets.

| Component | Description |
|-----------|-------------|
| **RpcServer** | Newline-delimited JSON-RPC server with per-connection isolation |
| **RpcClient** | Client with auto-incrementing request IDs |
| **LoopTrigger** | Routable trigger with source/target `ProcessId` and W3C trace context |
| **TriggerKind** | TaskDispatch, TaskProgress, TaskComplete, TaskFailed, TaskCancel, MetaCycle* |

### State Store (`src/store/`)

SQLite/WAL-backed key-value persistence with ACL enforcement.

| Component | Description |
|-----------|-------------|
| **SqliteStateStore** | WAL mode, dual-trigger checkpointing (write count + elapsed time) |
| **AclGuard** | Per-process write permissions via key-prefix ACL rules |
| **StateStore** trait | Generic interface for storage backends |

### Model Adapter (`src/adapter/`)

Supervised sidecar for LLM provider routing with fault tolerance.

| Component | Description |
|-----------|-------------|
| **ModelAdapter** trait | `complete()`, `health()`, `budget()` for provider interaction |
| **ProviderResolver** | Zone filter → capability filter → health filter → adversarial constraint → fallback rank |
| **CircuitBreaker** | Per-provider: Closed → Open → HalfOpen with configurable thresholds |
| **ProviderRegistry** | Tracks all providers with health aggregation |

### Hardware (`src/hardware/`)

| Component | Description |
|-----------|-------------|
| **SystemResources** | CPU count, total/available RAM, load averages (2s cache) |
| **MemoryPressureLevel** | Normal / Warn / Critical via macOS `sysctl` or RAM ratio fallback |
| **HardwareSnapshot** | Timestamped system state, serializable to JSON |

### Scheduler (`src/scheduler/`)

| Component | Description |
|-----------|-------------|
| **AdmissionController** | CPU/RAM/pressure gate before scheduling |
| **ResourceAllocator** | Thread-safe resource tracking |
| **Recommender** | Max agent capacity based on hardware + pressure headroom |

### DAG Engine (`src/dag/`)

| Component | Description |
|-----------|-------------|
| **DagGraph** | Directed acyclic graph with cycle detection and topological sort |
| **DagExecutor** | Async parallel execution via Tokio |
| **DagPersistence** | JSON checkpoint/resume with crash recovery |

### Agent (`src/agent/`)

| Component | Description |
|-----------|-------------|
| **AgentType** trait | `execute(task, timeout)` + `resource_requirements()` |
| **AgentState** | FSM: Idle → Busy → Done/Failed → Reset |
| **ClaudeCliAgent** | Subprocess management with stdin/stdout piping |
| **ShellAgent** | `/bin/sh -c` execution with working directory support |

## Daemon

The kernel runs as an async daemon (`aros-kernel run`):

```bash
# Run the kernel daemon (default)
aros-kernel run --state-dir ~/.aros/state --health-interval 5

# Show recommended agent capacity
aros-kernel recommend

# Show system status
aros-kernel status
```

Boot sequence: Init → Kernel → Loop 0/1/2 → Model Adapter → Embedding Adapter

Shutdown: reverse order with cooperative signal handling (SIGTERM/SIGINT).

## Socket Layout

```
{state_dir}/sockets/
├── kernel.sock              # Kernel event bus (hub)
├── loop0.sock               # Loop 0 (Meta) — long-lived
├── loop1-{task_id}.sock     # Loop 1 (Agentic) — per-task, ephemeral
├── loop2.sock               # Loop 2 (Harness) — long-lived
├── adapter-model.sock       # Model adapter sidecar
└── adapter-embed.sock       # Embedding adapter sidecar
```

## Building

```bash
cargo build --release
cargo test
```

## Test Suite

166 tests across 10 modules covering:
- Supervisor process lifecycle and restart policies
- Task envelope validation and serialization
- Governor admission/runtime decisions under pressure
- JSON-RPC protocol, client/server, concurrent connections
- State store CRUD, ACL enforcement, WAL checkpointing
- Model adapter circuit breaker and provider resolution
- DAG parallel dispatch and dependency resolution
- Hardware pressure detection and admission control
- Agent lifecycle state transitions
- MetaCycleComplete state persistence

## Tech Stack

- **Language**: Rust (Edition 2024)
- **Async runtime**: Tokio
- **State store**: SQLite with WAL mode (via rusqlite)
- **IPC**: JSON-RPC 2.0 over Unix domain sockets
- **Serialization**: serde + serde_json
- **Instrumentation**: tracing
- **Target**: macOS (Apple Silicon), with Linux fallbacks

## License

Apache License 2.0 — see [LICENSE](LICENSE)
