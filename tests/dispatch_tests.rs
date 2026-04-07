//! Integration tests for the dispatch module: JSON-RPC over Unix domain sockets,
//! loop trigger contracts, and client/server round-trips.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use aros_kernel::dispatch::contracts::{LoopTrigger, TriggerKind, TriggerError};
use aros_kernel::dispatch::rpc::{
    JsonRpcRequest, JsonRpcResponse, RpcMethod,
    ERROR_INTERNAL, ERROR_METHOD_NOT_FOUND, ERROR_PARSE,
    ERROR_PERMISSION_DENIED, ERROR_BUDGET_EXCEEDED, ERROR_SECURITY_ZONE, ERROR_ENVELOPE_VERSION,
};
use aros_kernel::dispatch::server::{PingHandler, RequestHandler, RpcServer};
use aros_kernel::dispatch::client::RpcClient;
use aros_kernel::envelope::task_envelope::{
    Priority, SecurityZone, TaskEnvelope, TaskSpec,
};
use aros_kernel::supervisor::process::ProcessId;

// ---------------------------------------------------------------------------
// JSON-RPC protocol types
// ---------------------------------------------------------------------------

#[test]
fn test_request_construction() {
    let req = JsonRpcRequest::new("ping", None, 1);
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.method, "ping");
    assert_eq!(req.id, Value::Number(1.into()));
    assert!(req.params.is_none());
}

#[test]
fn test_request_with_params() {
    let params = serde_json::json!({"task_id": "t-001"});
    let req = JsonRpcRequest::new("task.submit", Some(params.clone()), 42);
    assert_eq!(req.params.unwrap(), params);
}

#[test]
fn test_request_serialization_roundtrip() {
    let req = JsonRpcRequest::new("task.progress", Some(serde_json::json!({"phase": "running"})), 7);
    let json = serde_json::to_string(&req).unwrap();
    let back: JsonRpcRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.method, "task.progress");
    assert_eq!(back.id, Value::Number(7.into()));
}

#[test]
fn test_response_success() {
    let resp = JsonRpcResponse::success(Value::Number(1.into()), serde_json::json!("pong"));
    assert!(resp.result.is_some());
    assert!(resp.error.is_none());
    assert_eq!(resp.result.unwrap(), "pong");
}

#[test]
fn test_response_error() {
    let resp = JsonRpcResponse::error(
        Value::Number(1.into()),
        ERROR_METHOD_NOT_FOUND,
        "Unknown method: foo",
        None,
    );
    assert!(resp.result.is_none());
    let err = resp.error.unwrap();
    assert_eq!(err.code, ERROR_METHOD_NOT_FOUND);
    assert!(err.message.contains("foo"));
}

#[test]
fn test_response_serialization_roundtrip() {
    let resp = JsonRpcResponse::success(Value::Number(5.into()), serde_json::json!({"ok": true}));
    let json = serde_json::to_string(&resp).unwrap();
    let back: JsonRpcResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, Value::Number(5.into()));
    assert!(back.result.is_some());
}

#[test]
fn test_error_codes_are_negative() {
    assert!(ERROR_PARSE < 0);
    assert!(ERROR_INTERNAL < 0);
    assert!(ERROR_METHOD_NOT_FOUND < 0);
    assert!(ERROR_PERMISSION_DENIED < 0);
    assert!(ERROR_BUDGET_EXCEEDED < 0);
    assert!(ERROR_SECURITY_ZONE < 0);
    assert!(ERROR_ENVELOPE_VERSION < 0);
}

// ---------------------------------------------------------------------------
// RPC method resolution
// ---------------------------------------------------------------------------

#[test]
fn test_rpc_method_roundtrip() {
    let methods = [
        RpcMethod::TaskSubmit,
        RpcMethod::TaskProgress,
        RpcMethod::TaskComplete,
        RpcMethod::TaskCancel,
        RpcMethod::LoopTrigger,
        RpcMethod::Ping,
    ];
    for m in methods {
        let s = m.as_str();
        let back = RpcMethod::from_str(s).unwrap();
        assert_eq!(back, m);
    }
}

#[test]
fn test_rpc_method_unknown() {
    assert!(RpcMethod::from_str("nonexistent.method").is_none());
}

// ---------------------------------------------------------------------------
// Loop trigger contracts
// ---------------------------------------------------------------------------

fn sample_task_spec() -> TaskSpec {
    TaskSpec {
        title: "Test task".into(),
        description: "Integration test".into(),
        working_dir: Some("/tmp/test".into()),
        env_vars: HashMap::new(),
        max_retries: 2,
    }
}

fn sample_envelope() -> TaskEnvelope {
    TaskEnvelope::new(
        "task-001",
        "dag-abc",
        sample_task_spec(),
        SecurityZone::Green,
        Priority::P1Normal,
    )
}

#[test]
fn test_trigger_task_dispatch() {
    let envelope = sample_envelope();
    let trigger = LoopTrigger::new(
        1,
        ProcessId::Loop2Harness,
        ProcessId::Loop1Agentic,
        TriggerKind::TaskDispatch(envelope),
    );
    assert_eq!(trigger.seq, 1);
    assert_eq!(trigger.source, ProcessId::Loop2Harness);
    assert_eq!(trigger.target, ProcessId::Loop1Agentic);
    assert!(trigger.trace_parent.is_none());
}

#[test]
fn test_trigger_with_trace() {
    let trigger = LoopTrigger::new(
        1,
        ProcessId::Kernel,
        ProcessId::Loop0Meta,
        TriggerKind::Ping,
    )
    .with_trace("00-abc123-def456-01".to_string());

    assert_eq!(
        trigger.trace_parent.as_deref(),
        Some("00-abc123-def456-01")
    );
}

#[test]
fn test_trigger_task_complete() {
    let trigger = LoopTrigger::new(
        5,
        ProcessId::Loop1Agentic,
        ProcessId::Kernel,
        TriggerKind::TaskComplete {
            task_id: "task-001".to_string(),
            dag_id: "dag-abc".to_string(),
            output: "Done".to_string(),
            tokens_used: 5000,
            duration_secs: 12.5,
        },
    );

    match &trigger.kind {
        TriggerKind::TaskComplete { tokens_used, .. } => {
            assert_eq!(*tokens_used, 5000);
        }
        _ => panic!("Expected TaskComplete"),
    }
}

#[test]
fn test_trigger_task_failed() {
    let trigger = LoopTrigger::new(
        6,
        ProcessId::Loop1Agentic,
        ProcessId::Kernel,
        TriggerKind::TaskFailed {
            task_id: "task-002".to_string(),
            dag_id: "dag-abc".to_string(),
            error: "timeout".to_string(),
            retryable: true,
        },
    );

    match &trigger.kind {
        TriggerKind::TaskFailed { retryable, .. } => assert!(*retryable),
        _ => panic!("Expected TaskFailed"),
    }
}

#[test]
fn test_trigger_meta_cycle_request() {
    let trigger = LoopTrigger::new(
        10,
        ProcessId::Loop0Meta,
        ProcessId::Kernel,
        TriggerKind::MetaCycleRequest {
            trigger_source: "schedule".to_string(),
        },
    );
    assert_eq!(trigger.source, ProcessId::Loop0Meta);
}

#[test]
fn test_trigger_meta_cycle_complete() {
    let trigger = LoopTrigger::new(
        12,
        ProcessId::Loop0Meta,
        ProcessId::Kernel,
        TriggerKind::MetaCycleComplete {
            cycle_id: "cycle-001".to_string(),
            policy_changed: true,
            drift_score: 0.15,
        },
    );

    match &trigger.kind {
        TriggerKind::MetaCycleComplete {
            policy_changed,
            drift_score,
            ..
        } => {
            assert!(*policy_changed);
            assert!(*drift_score < 0.2);
        }
        _ => panic!("Expected MetaCycleComplete"),
    }
}

#[test]
fn test_trigger_serialization_roundtrip() {
    let envelope = sample_envelope();
    let trigger = LoopTrigger::new(
        1,
        ProcessId::Loop2Harness,
        ProcessId::Loop1Agentic,
        TriggerKind::TaskDispatch(envelope),
    )
    .with_trace("00-trace-id-01".to_string());

    let json = serde_json::to_string(&trigger).unwrap();
    let back: LoopTrigger = serde_json::from_str(&json).unwrap();
    assert_eq!(back.seq, 1);
    assert_eq!(back.source, ProcessId::Loop2Harness);
    assert_eq!(back.target, ProcessId::Loop1Agentic);
    assert_eq!(back.trace_parent.as_deref(), Some("00-trace-id-01"));

    match back.kind {
        TriggerKind::TaskDispatch(env) => {
            assert_eq!(env.task_id, "task-001");
        }
        _ => panic!("Expected TaskDispatch"),
    }
}

#[test]
fn test_trigger_cancel() {
    let trigger = LoopTrigger::new(
        20,
        ProcessId::Kernel,
        ProcessId::Loop1Agentic,
        TriggerKind::TaskCancel {
            task_id: "task-003".to_string(),
            reason: "budget exceeded".to_string(),
        },
    );

    match &trigger.kind {
        TriggerKind::TaskCancel { reason, .. } => {
            assert!(reason.contains("budget"));
        }
        _ => panic!("Expected TaskCancel"),
    }
}

// ---------------------------------------------------------------------------
// Client/Server round-trip over Unix domain sockets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_ping_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test.sock");

    let server = RpcServer::new(sock_path.clone());

    // Spawn server in background.
    let server_shutdown = {
        let s = RpcServer::new(sock_path.clone());
        tokio::spawn(async move {
            s.serve(PingHandler).await.unwrap();
        });
        server
    };

    // Give server a moment to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect client.
    let mut client = RpcClient::connect(&sock_path).await.unwrap();

    // Send ping.
    let resp = client.call("ping", None).await.unwrap();
    assert!(resp.error.is_none());
    assert_eq!(resp.result.unwrap(), "pong");

    // Shutdown.
    server_shutdown.shutdown();
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn test_unknown_method_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test2.sock");

    let server = RpcServer::new(sock_path.clone());

    tokio::spawn(async move {
        server.serve(PingHandler).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = RpcClient::connect(&sock_path).await.unwrap();
    let resp = client.call("nonexistent.method", None).await.unwrap();

    let err = resp.error.unwrap();
    assert_eq!(err.code, ERROR_METHOD_NOT_FOUND);
    assert!(err.message.contains("nonexistent.method"));
}

#[tokio::test]
async fn test_multiple_requests_on_same_connection() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("test3.sock");

    let server = RpcServer::new(sock_path.clone());

    tokio::spawn(async move {
        server.serve(PingHandler).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = RpcClient::connect(&sock_path).await.unwrap();

    // Send multiple requests on the same connection.
    for _ in 0..5 {
        let resp = client.call("ping", None).await.unwrap();
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap(), "pong");
    }
}

/// Custom handler that echoes back task.submit params and handles task.progress.
#[derive(Clone)]
struct EchoHandler;

impl RequestHandler for EchoHandler {
    async fn handle(
        &self,
        method: RpcMethod,
        params: Option<Value>,
    ) -> Result<Value, (i32, String)> {
        match method {
            RpcMethod::TaskSubmit => {
                Ok(serde_json::json!({
                    "status": "accepted",
                    "params": params,
                }))
            }
            RpcMethod::TaskProgress => {
                Ok(serde_json::json!({"ack": true}))
            }
            RpcMethod::Ping => Ok(Value::String("pong".to_string())),
            _ => Err((ERROR_INTERNAL, format!("Unhandled: {:?}", method))),
        }
    }
}

#[tokio::test]
async fn test_task_submit_echo() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("echo.sock");

    let server = RpcServer::new(sock_path.clone());

    tokio::spawn(async move {
        server.serve(EchoHandler).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = RpcClient::connect(&sock_path).await.unwrap();

    let envelope = sample_envelope();
    let params = serde_json::to_value(&envelope).unwrap();
    let resp = client.call("task.submit", Some(params)).await.unwrap();

    let result = resp.result.unwrap();
    assert_eq!(result["status"], "accepted");
    assert!(result["params"].is_object());
}

#[tokio::test]
async fn test_concurrent_clients() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("concurrent.sock");

    let server = RpcServer::new(sock_path.clone());

    tokio::spawn(async move {
        server.serve(PingHandler).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Spawn 5 concurrent clients.
    let mut handles = Vec::new();
    for i in 0..5 {
        let path = sock_path.clone();
        handles.push(tokio::spawn(async move {
            let mut client = RpcClient::connect(&path).await.unwrap();
            let resp = client.call("ping", None).await.unwrap();
            assert!(resp.error.is_none(), "Client {i} got error");
            assert_eq!(resp.result.unwrap(), "pong");
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// TriggerError display
// ---------------------------------------------------------------------------

#[test]
fn test_trigger_error_display() {
    let err = TriggerError::TargetNotRunning(ProcessId::Loop1Agentic);
    assert!(err.to_string().contains("Loop1Agentic"));

    let err = TriggerError::Rejected("budget exceeded".to_string());
    assert!(err.to_string().contains("budget exceeded"));

    let err = TriggerError::Dispatch("socket closed".to_string());
    assert!(err.to_string().contains("socket closed"));
}
