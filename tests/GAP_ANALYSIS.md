# Test Coverage Gap Analysis

**Baseline (2026-04-17):** 279 tests across 13 files, all passing on `cargo test --release`.

This document catalogs test coverage gaps identified by module criticality ×
coverage deficit. Gaps are ranked by *production impact if exercised* —
not count.

## Method

1. Ran `cargo test --release --no-fail-fast` for baseline (all pass).
2. Walked each module's source looking for error enum variants, panics, and
   documented failure modes, then cross-referenced against test names.
3. Weighted gaps by: (a) kernel-critical modules (supervisor, governor,
   dispatch, store) > adapters, (b) error/failure paths > happy paths,
   (c) cross-module integration > unit-level.

## Top gaps (ranked)

### 1. `dispatch` — JSON-RPC server error paths (ADDRESSED in this commit)

- **ERROR_PARSE (malformed JSON input):** `server::process_line` line 117-127
  returns ERROR_PARSE for unparseable input. No test exercised this code path.
  Added `test_malformed_json_returns_parse_error`.
- **Handler-emitted explicit errors:** `server::process_line` line 142 propagates
  `(code, msg)` tuples from the handler as JsonRpcResponse::error. Previously
  only `ERROR_METHOD_NOT_FOUND` was tested; custom error codes from the handler
  were not. Added `test_handler_error_propagates_to_client`.
- **Connection EOF mid-session:** `server.rs` line 95 breaks cleanly on EOF.
  Reconnection after EOF was implicit but not asserted. Added
  `test_server_handles_client_disconnect_cleanly`.
- **Server-side shutdown with connected client:** The shutdown path (line 104)
  was untested. Attempted but deferred — `RpcServer::new` creates a fresh
  broadcast channel per instance, and the existing test pattern moves the
  server into the spawned task, making external shutdown signaling awkward
  without an API change. Recommended follow-up: change `serve()` to take a
  shutdown token or return a handle.

### 2. `governor` — `GovernorError::UnknownTier` and `NotInitialized`

- `GovernorError` defines three variants but no test constructs or asserts
  `UnknownTier` or `NotInitialized`. These are narrow (internal invariants),
  low-risk in production. Deferred.

### 3. `supervisor` — restart policy exhaustion + handler routing failure

- Existing tests cover the happy path and exponential backoff shape, but the
  terminal `Failed` state (max restarts exceeded within window) and
  `KernelRequestHandler` routing to an unknown `ProcessId` have only indirect
  coverage. Recommended follow-up: 2–3 tests in `tests/supervisor_tests.rs`
  (currently inline-only in `src/supervisor/`).

### 4. `store` — WAL checkpoint failure / disk-full simulation

- The SQLite WAL checkpoint path has happy-path tests but no failure simulation
  (e.g., read-only filesystem, permission error on the WAL file). Would
  require injecting a faulty backend — deferred as it needs a trait rework.

### 5. `adapter` — provider circuit breaker under concurrent calls

- Circuit breaker state transitions are tested single-threaded. Concurrent
  failures hitting the threshold simultaneously (racy half-open) not asserted.
  Low severity since `CircuitBreaker` uses atomics, but worth a concurrency
  test. Deferred.

### 6. `envelope` — envelope version negotiation downgrade path

- `ERROR_ENVELOPE_VERSION` exists but the client-side downgrade or rejection
  logic isn't exercised in an integration test. Deferred until version
  negotiation is actually used (currently v1 only).

## What was added in this pass

Three new tests in `tests/dispatch_tests.rs` targeting Gap #1 (highest-impact,
lowest-effort):

- `test_malformed_json_returns_parse_error` — asserts ERROR_PARSE with null id
- `test_handler_error_propagates_to_client` — asserts handler-emitted `(code, msg)` tuples reach the client with id preserved
- `test_server_handles_client_disconnect_cleanly` — asserts server keeps accepting after a dropped client

## What was deliberately NOT added

Per Eddie's principle *"stability > intelligence, simplicity > complexity"*,
this pass adds **4 high-value tests** rather than 30+ shallow ones. The
remaining gaps are documented here so the next hardening pass has a clear
starting point without having to re-audit.

---

## Pass 2 (2026-05-04, MetaLoop cycle #338)

### Closed gaps

- [x] **§2 governor — `GovernorError` variant coverage.** Added Display
  assertions for all three variants and an `std::error::Error` trait
  bound check in `tests/error_handling_tests.rs`. Guards against
  accidental message changes that telemetry/log alerts depend on.
- [x] **§3 supervisor — terminal `Failed` state persistence.** New
  `tests/supervisor_tests.rs` (8 tests) asserts that after
  `MaxRestartsExceeded` the process *remains* in `Failed`, that
  repeated failure calls keep erroring, and that `Running → Running`
  resets the restart tracker so post-recovery failures get a fresh
  budget.
- [x] **§3 supervisor — restart tracker boundary cases.** Covers
  `max_restarts == 0` (must immediately fail), restart-window
  eviction allowing new restarts, and the invariant that
  `consecutive_failures` persists across window eviction (so backoff
  cannot be reset by wall-clock time alone).
- [x] **§5 adapter — circuit breaker under concurrency.** Three new
  tests in `tests/concurrency_tests.rs` exercise the breaker through
  a `Mutex<CircuitBreaker>` under 32× contention, mixed
  success/failure interleaving, and concurrent reader/writer
  watchdog (2s deadlock budget). Confirms the state machine reaches
  `Open` deterministically and never panics or deadlocks.

### Still deferred

- §3 — `KernelRequestHandler` routing to an unknown `ProcessId`: the
  existing `test_loop_trigger_from_stopped_process_rejected` covers
  the source-side check; target-side routing still has TODO markers
  in `handler.rs` and isn't worth testing until implemented.
- §4 — store WAL checkpoint failure: still requires a trait rework
  to inject a faulty backend.
- §6 — envelope version negotiation: still a v1-only protocol.

### Pass 2 test count

**+17 tests** (8 supervisor + 6 governor/supervisor error + 3 adapter
concurrency). All passing on `cargo test --release` baseline 296 tests
(279 baseline + 17).

---

## Pass 3 (2026-05-07, MetaLoop cycle #340)

### Scope decision

The MetaLoop dispatched this pass with a stale framing — *"complete 30+
missing test cases."* Eddie-Nirmana review rejected the framing per the
Pass 1 principle (*"4 high-value tests rather than 30+ shallow ones"*)
and refined the scope to: the highest-impact remaining gaps that do **not**
require trait rework. Padding the count to 30 was explicitly out of scope.

### Closed gaps

- [x] **§3 supervisor — `KernelRequestHandler` target-side routing.**
  Previously, `handle_loop_trigger` only verified the *source* process
  was Running; the target was passed through without validation, so a
  trigger targeting a stopped, restarting, or unregistered process
  would silently return `status: routed` and the caller had no
  signal that no listener existed. Implemented a target-side check
  in `src/supervisor/handler.rs`: if `trigger.target != ProcessId::Kernel`,
  the target must be in `active_processes` with state `Running`,
  otherwise reject with `ERROR_PERMISSION_DENIED`. Kernel is exempted
  because the handler *is* the kernel — applying the check would
  reject every Loop-0/Loop-1/Loop-2 → Kernel trigger including the
  meta-cycle path. Three new tests in
  `tests/../src/supervisor/handler.rs` (cfg(test) module):
  - `test_loop_trigger_to_stopped_target_rejected` — target registered but in `Starting` state → reject
  - `test_loop_trigger_to_unregistered_target_rejected` — target absent from supervisor → reject
  - `test_loop_trigger_to_running_target_routes` — both Running → routes successfully
  - `test_loop_trigger_kernel_target_skips_target_running_check` —
    regression guard: future refactors must not apply target-running
    check to Kernel.
  Implementation diff: ~17 LOC added to `handle_loop_trigger`. Within
  Nirmana's 30-LOC bound for GREEN-authority change.

- [x] **`envelope` — round-trip and validation invariants across the
  full `SecurityZone × Priority × ResourceBudget` matrix.** The
  existing `task_envelope.rs` inline tests covered single instances;
  this pass adds *parametrized* coverage across 9 zone × priority
  combinations and 4 budget shapes (min-nonzero, default, high-threshold,
  low-threshold), so a regression on any one combination cannot
  hide behind the others. New file `tests/envelope_property_tests.rs`
  with 5 tests:
  - `envelope_round_trip_all_zone_priority_combinations` — all 9
    zone×priority pairs serialize→deserialize losslessly with all
    fields preserved (env_vars, tool_endpoints, checkpoint_policy)
  - `envelope_round_trip_zone_priority_budget_matrix` — 9 × 4 = 36
    combinations round-trip and uphold token-budget invariants
    (`exceeded` is strict `>`, `warning` triggers at `floor(max × threshold)`)
  - `envelope_validation_rejects_zero_budget_fields_across_matrix` —
    each zero-field rejection (`max_tokens=0`, `max_rss_mb=0`,
    `max_wall_time=0`, threshold out-of-range) holds for every
    zone × priority pair, guarding against accidental priority-class
    bypass logic
  - `priority_total_order_and_serialized_form_are_stable` — `Ord`
    invariant + JSON shape (`"P0Critical"`/`"P1Normal"`/`"P2Background"`)
    pinned, so log/telemetry joins on the string form don't silently break
  - `security_zone_serialized_form_is_stable` — same guarantee for
    `"Green"`/`"Yellow"`/`"Red"`

### Still deferred

- §1 (Pass 1) — dispatch server-side shutdown with a connected client:
  still requires changing `RpcServer::serve()` to take a shutdown token
  or return a handle. **YELLOW** — defer until the API change is
  motivated by an actual call site, not by test coverage alone.
- §4 — store WAL checkpoint failure: still requires injecting a
  faulty backend via a trait rework. **YELLOW**.
- §6 — envelope version negotiation: still a v1-only protocol.

### What was deliberately NOT added

- No padding tests to hit a "30+" count. Per Eddie's principle
  (*"stability > intelligence, simplicity > complexity"*), shallow
  tests dilute the signal of a green suite — every test added in
  this pass exercises a production-impact path or a regression
  guard for a behavior that telemetry/policy systems join on.
- No git tag, no Cargo.toml version bump, no GitHub release. Tagging
  is a one-way door touching public release timing — that decision
  is **YELLOW** authority and stays with Eddie.

### Pass 3 test count

**+9 tests** (4 supervisor handler routing + 5 envelope property tests).
All passing on `cargo test --release` — 305 tests total
(296 from Pass 2 baseline + 9). Library inline tests: 171 (was 167).
