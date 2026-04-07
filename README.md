# AROS Kernel

The core runtime kernel for AROS (Agent Runtime OS). Manages agent lifecycle, task orchestration, resource governance, model adapter routing, and inter-loop communication via a supervised process tree.

## Architecture

```
                    ┌─────────────────────────────────────────────┐
                    │              AROS KERNEL                     │
                    │                                              │
  ┌──────────┐     │  ┌────────────────────────────────────────┐  │
  │  Loop 0  │◄────┤  │         Supervisor Daemon               │  │
  │  (Meta)  │     │  │  init -> kernel -> loops -> adapters    │  │
  └──────────┘     │  └──────────────┬─────────────────────────┘  │
                    │                 │                             │
  ┌──────────┐     │  ┌──────────────▼─────────────────────────┐  │
  │  Loop 2  │◄────┤  │       JSON-RPC Dispatch (UDS)           │  │
  │ (Harness)│     │  │  kernel.sock | loop*.sock | adapter*.sock│  │
  └──────────┘     │  └──────────────┬─────────────────────────┘  │
                    │                 │                             │
  ┌──────────┐     │  ┌──────┬───────┼───────┬──────┬──────────┐  │
  │  Loop 1  │◄────┤  │State │Resource│  DAG  │Model │ Hardware │  │
  │(Agentic) │     │  │Store │Governor│Engine │Adapt.│ Monitor  │  │
  └──────────┘     │  └──────┴───────┴───────┴──────┴──────────┘  │
                    └─────────────────────────────────────────────┘
```

## Modules

### Supervisor (`src/supervisor/`)
Two-level supervision tree with process lifecycle management.

- `ProcessId` — Init, Kernel, Loop0–2, ModelAdapter, EmbeddingAdapter
- `ProcessState` — Starting, Running, Stopping, Stopped, Failed, Restarting
- `RestartPolicy` — exponential backoff with configurable max restarts
- `KernelSupervisor` — health aggregation (Healthy/Degraded/Recovering)
- `KernelRequestHandler` — JSON-RPC router for inter-loop trigger dispatch

### Task Envelope (`src/envelope/`)
Versioned task schema with security and resource constraints.

- `SecurityZone` — Green (any provider), Yellow (approved only), Red (local only)
- `Priority` — P0Critical (always admitted), P1Normal (standard), P2Background (shed first)
- `ResourceBudget` — max RSS, wall time, token budget, warning threshold
- `TaskEnvelope` v1 — task spec, checkpoint policy, tool endpoints

### Resource Governor (`src/governor/`)
Two-phase resource management: admission control + runtime budget enforcement.

- **Admission**: queue, throttle, shed based on priority and system pressure
- **Budget**: per-tier token tracking (P0 reserved, P1 pool, P2 spare capacity)
- System-wide RSS ceiling with headroom reserve

### Model Adapter (`src/adapter/`)
Unified interface for all LLM interactions with capability-based provider resolution.

- `ModelAdapter` trait — `complete()`, `health()`, `budget()`
- **Circuit breaker** per provider (Closed → Open → HalfOpen)
- **Provider resolver** — capabilities + zone + health + adversarial constraint → best available
- **Capability matching** — context window, tool use, vision, streaming, quality tier
- **Degradation tracking** — None/Mild/Significant based on fallback position
- Request/response schemas with context attribution (L1–L4 memory tiers)

### State Store (`src/store/`)
SQLite/WAL-backed key-value store with ACL enforcement.

- `StateStore` trait — get/put/delete/list/exists on namespaced keys
- `SqliteStateStore` — WAL mode, configurable checkpoint policy
- `ProcessIdentity` ACL — per-process write permissions on key prefixes

### JSON-RPC Dispatch (`src/dispatch/`)
Unix domain socket communication between kernel and loop processes.

- **Protocol** — JSON-RPC 2.0 over newline-delimited UDS
- **Methods** — task.submit, task.progress, task.complete, task.cancel, loop.trigger, ping
- **Loop trigger contracts** — TaskDispatch, TaskProgress, TaskComplete, TaskFailed, TaskCancel, MetaCycleRequest, MetaCycleAuthorized, MetaCycleComplete
- **Socket convention** — `{state_dir}/sockets/kernel.sock`, `loop0.sock`, `loop1-{task_id}.sock`, `loop2.sock`, `adapter-model.sock`

### DAG Engine (`src/dag/`)
Directed acyclic graph executor with parallel task dispatch and crash recovery.

- Cycle detection (DFS), topological sort (Kahn's algorithm)
- Async parallel execution via Tokio with `max_parallel` limit
- JSON checkpoint/resume with crash recovery (InProgress → Pending on reload)

### Hardware Monitor (`src/hardware/`)
System resource probing with memory pressure detection.

- CPU count, total/available RAM, load averages (2s cached snapshots)
- macOS-specific pressure detection via `sysctl`
- Pressure levels: Normal, Warn, Critical

### Scheduler (`src/scheduler/`)
Legacy admission controller and resource allocator (being superseded by governor).

### Agent (`src/agent/`)
Agent type abstraction with subprocess management.

- `AgentType` trait — `execute(task, timeout)` + `resource_requirements()`
- `ClaudeCliAgent` — Claude CLI subprocess with stdin/stdout piping
- `ShellAgent` — `/bin/sh -c` execution

## Usage

```bash
# Build
cargo build --release

# Run the kernel daemon
cargo run -- run --state-dir ./aros-state

# Run all tests
cargo test

# Run specific module tests
cargo test adapter
cargo test store
cargo test supervisor

# Clippy (allow pre-existing module_inception in governor)
cargo clippy -- -D warnings -A clippy::module_inception
```

## Integration with aros-sie

The kernel's Loop 0 orchestrator calls into the SIE's trait-based abstractions:

| SIE Trait | Kernel Usage |
|-----------|-------------|
| `SelfModel` | SELF-MODEL UPDATE step |
| `Critic` | CRITIQUE step |
| `PolicyStore` | POLICY REVISION step |
| `IdentityChecker` | IDENTITY CHECK step |
| `StateStore` | SIE persistence (kernel provides SQLite/WAL impl) |

State store keys for SIE data:
- `sie/identity/last_drift` — latest drift score (UI drift gauge)
- `sie/policy/head` — current policy snapshot ID (Evolution Timeline)
- `sie/meta/last_cycle` — latest meta-cycle ID

## Tech Stack

- Rust (edition 2024)
- tokio, serde, serde_json, tracing, tracing-subscriber, thiserror
- rusqlite (bundled SQLite), sysinfo, clap, libc
- uuid, chrono

## License

Private — AROS-Lab
