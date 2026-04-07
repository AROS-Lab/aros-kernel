# AROS Kernel

The kernel layer of the **Agent Runtime Operating System (AROS)** — a constellation-model runtime for AI agents with hardware-aware scheduling, self-improvement coordination, and safety-enforced execution.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          AROS Kernel                                │
│                                                                     │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │  Supervisor  │  │   Resource   │  │     JSON-RPC Dispatch    │  │
│  │    Tree      │  │   Governor   │  │   (Unix Domain Sockets)  │  │
│  │             │  │              │  │                          │  │
│  │  Init       │  │  Admission   │  │  kernel.sock  (hub)     │  │
│  │  └─ Kernel  │  │  + Runtime   │  │  loop0.sock   (meta)    │  │
│  │     └─ Loops│  │  + Budget    │  │  loop1-*.sock (agentic) │  │
│  │     └─ Adpt │  │              │  │  loop2.sock   (harness) │  │
│  └─────────────┘  └──────────────┘  └──────────────────────────┘  │
│                                                                     │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │ State Store  │  │    Task      │  │    Model Adapter         │  │
│  │ SQLite/WAL   │  │  Envelope    │  │    (Sidecar)             │  │
│  │ + ACL Guard  │  │  + Security  │  │                          │  │
│  │              │  │    Zones     │  │  Circuit breaker          │  │
│  │  Per-process │  │  + Priority  │  │  Provider resolution     │  │
│  │  write perms │  │    Tiers     │  │  Zone-aware routing      │  │
│  └─────────────┘  └──────────────┘  └──────────────────────────┘  │
│                                                                     │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
│  │  Hardware    │  │  Scheduler   │  │     DAG Engine           │  │
│  │  Monitor     │  │  Admission   │  │  Async parallel executor │  │
│  │  + Pressure  │  │  Control     │  │  Checkpoint/resume       │  │
│  └─────────────┘  └──────────────┘  └──────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
```

## Modules

### Supervisor Tree (`src/supervisor/`)

Two-level supervisor: Init (OS-managed, immortal) → Kernel (checkpoint-recoverable) → Loops (ephemeral).

| Component | Description |
|-----------|-------------|
| **ProcessId** | Registry of supervised processes (Init, Kernel, Loop0–2, ModelAdapter, EmbeddingAdapter) |
| **ProcessState** | State machine: Starting → Running → Stopping → Stopped → Failed → Restarting |
| **RestartPolicy** | Exponential backoff with configurable max restarts and window |
| **KernelSupervisor** | Health aggregation (Healthy/Degraded/Recovering), process lifecycle management |
| **KernelRequestHandler** | JSON-RPC router: task admission, progress tracking, trigger routing, budget enforcement |
| **TaskRegistry** | Tracks active Loop 1 instances (task_id → socket_path) |

### State Store (`src/store/`)

SQLite with WAL mode for single-node persistence. Designed for future distributed backends (etcd/FoundationDB).

| Component | Description |
|-----------|-------------|
| **StateStore** trait | Key-value contract: `put`, `get`, `delete`, `list_keys`, `exists`, `transaction` |
| **SqliteStateStore** | WAL mode, dual-trigger checkpointing (write count OR elapsed seconds), hard WAL ceiling |
| **AclGuard** | Kernel-enforced per-process write permissions by key prefix |
| **ProcessIdentity** | Caller identity for ACL enforcement (Kernel, MetaLoop, HarnessLoop, AgenticLoop, ModelAdapter, Human) |

**ACL Rules:**

| Key Prefix | Write Access | Notes |
|------------|-------------|-------|
| `/policy/*` | MetaLoop | Two-step commit: staging → kernel validation → promote |
| `/meta-goals/*` | Human only | Identity constraints are human-owned |
| `/audit/*` | Append-only | All identities can append, none can delete |
| `/evolution-log/*` | MetaLoop (append-only) | Branching policy archive |
| `/circuit-breaker/*` | ModelAdapter | Provider health state |
| `/dag/*` | HarnessLoop | DAG execution state |
| `/task/*` | AgenticLoop | Task execution state |
| `/self-model/*` | MetaLoop | Bayesian capability model |
| `/security/redzone/*` | Read-only | Kernel invariants, no writes permitted |

### JSON-RPC Dispatch (`src/dispatch/`)

Inter-loop communication over Unix domain sockets using JSON-RPC 2.0.

| Component | Description |
|-----------|-------------|
| **RpcServer** | Unix socket server with newline-delimited JSON-RPC, graceful shutdown, per-connection isolation |
| **RpcClient** | Unix socket client with auto-incrementing request IDs |
| **RpcMethod** | `task.submit`, `task.progress`, `task.complete`, `task.cancel`, `loop.trigger`, `ping` |
| **LoopTrigger** | Typed trigger envelope with ProcessId routing, sequence numbers, W3C trace context |
| **TriggerKind** | All inter-loop flows: TaskDispatch, TaskProgress/Complete/Failed, TaskCancel, MetaCycle lifecycle |
| **TriggerSink** | Trait for kernel-side trigger routing |

**Socket Convention:**
```
{state_dir}/sockets/kernel.sock          — Kernel event bus (all loops connect here)
{state_dir}/sockets/loop0.sock           — Loop 0 (Meta), long-lived sidecar
{state_dir}/sockets/loop1-{task_id}.sock — Loop 1 (Agentic), per-task ephemeral
{state_dir}/sockets/loop2.sock           — Loop 2 (Harness), long-lived
{state_dir}/sockets/adapter-model.sock   — Model adapter sidecar
{state_dir}/sockets/adapter-embed.sock   — Embedding adapter sidecar
```

### Task Envelope (`src/envelope/`)

Versioned contract for Loop 2 → Loop 1 task dispatch.

| Component | Description |
|-----------|-------------|
| **TaskEnvelope** | task_id, dag_id, task_spec, security_zone, priority, resource_budget, tool_endpoints, checkpoint_policy |
| **SecurityZone** | Green (any provider), Yellow (approved only), Red (local only, airgapped) |
| **Priority** | P0Critical (Loop 0/health, always admitted), P1Normal (task execution), P2Background (SIE, first to shed) |
| **ResourceBudget** | max_rss_mb, max_wall_time, max_tokens, budget_warning_threshold |

### Resource Governor (`src/governor/`)

Two-phase resource management: admission control (can I start?) + runtime monitoring (are you within budget?).

| Component | Description |
|-----------|-------------|
| **ResourceGovernor** | Admission (admit/queue/throttle/shed) + runtime budget enforcement |
| **TierBudget** | Per-priority resource allocation (P0: never shed, P2: shed first) |
| **TierUsage** | Real-time atomic tracking of active tasks, tokens, RSS |
| **AdmissionDecision** | Admit / Queue / Throttle / Shed based on pressure + priority |

Ordering under pressure: queue → throttle → shed. P0 has reserved non-lendable budget.

### Model Adapter (`src/adapter/`)

Supervised sidecar for LLM provider abstraction.

| Component | Description |
|-----------|-------------|
| **ModelAdapter** trait | `complete()`, `health()`, `budget()` — capability-based, callers never name a model directly |
| **ProviderResolver** | Zone filter → capability filter → health filter → adversarial constraint → fallback rank sort |
| **CircuitBreaker** | Per-provider: Closed → HalfOpen → Open. Recovers to HalfOpen on kernel restart |
| **ProviderRegistry** | Provider tracking with health aggregation (Healthy/Degraded/AllProvidersDown) |
| **QualityTier** | Haiku / Sonnet / Opus capability levels |

### Hardware Monitor (`src/hardware/`)

| Component | Description |
|-----------|-------------|
| **SystemResources** | CPU count, total/available RAM, load averages (2s cache) |
| **MemoryPressureLevel** | Normal / Warn / Critical via macOS `sysctl` |
| **HardwareSnapshot** | Timestamped system state, serializable to JSON |

### Scheduler (`src/scheduler/`)

| Component | Description |
|-----------|-------------|
| **AdmissionController** | Enforces CPU/RAM/pressure limits before scheduling |
| **ResourceAllocator** | Thread-safe resource tracking (`Arc<Mutex<>>`) |
| **Recommender** | Max agent calculation based on hardware + pressure headroom |

### DAG Engine (`src/dag/`)

| Component | Description |
|-----------|-------------|
| **DagGraph** | Directed acyclic graph with DFS cycle detection and Kahn's topological sort |
| **DagExecutor** | Async parallel task execution via Tokio, respects max_parallel |
| **RuntimeDag** | `Arc<RwLock<>>` wrapper for safe concurrent mutation |
| **DagPersistence** | JSON checkpoint/resume with crash recovery (InProgress → Pending) |

### Agent (`src/agent/`)

| Component | Description |
|-----------|-------------|
| **AgentType** trait | `execute(task, timeout)` + `resource_requirements()` |
| **AgentState** | FSM: Idle → Busy → Done/Failed → Reset |
| **ClaudeCliAgent** | Subprocess management with stdin/stdout piping, timeout |
| **ShellAgent** | `/bin/sh -c` execution with working directory support |

## Usage

```bash
# Build
cargo build --release

# Run the kernel daemon
cargo run -- run --state-dir /tmp/aros-state

# Run tests
cargo test

# Options
cargo run -- run --state-dir ./data --health-interval 10 --log-level debug
```

## Test Suite

167 tests across 10 modules covering:
- SQLite/WAL operations, checkpointing, transactions
- ACL enforcement for every key prefix, append-only rules
- JSON-RPC client/server round-trips, concurrent connections
- Loop trigger serialization and routing
- Task envelope validation and serde
- Supervisor state machine transitions, restart backoff
- Governor admission under pressure, budget enforcement
- Circuit breaker state transitions
- DAG parallel dispatch, dependency resolution, crash recovery
- Hardware pressure detection, agent lifecycle

## Tech Stack

- **Language:** Rust (Edition 2024)
- **Async runtime:** Tokio
- **Persistence:** SQLite with WAL mode (rusqlite)
- **IPC:** JSON-RPC 2.0 over Unix domain sockets
- **Serialization:** serde + serde_json
- **Telemetry:** tracing + tracing-subscriber
- **Hardware:** sysinfo + libc (macOS sysctl)

## License

Private — AROS-Lab
