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
