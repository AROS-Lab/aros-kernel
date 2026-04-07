//! JSON-RPC 2.0 protocol types for inter-loop communication.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    pub id: Value,
}

impl JsonRpcRequest {
    pub fn new(method: &str, params: Option<Value>, id: u64) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: Value::Number(id.into()),
        }
    }
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Value,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    pub fn error(id: Value, code: i32, message: &str, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data,
            }),
            id,
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// Standard JSON-RPC error codes.
pub const ERROR_PARSE: i32 = -32700;
pub const ERROR_INVALID_REQUEST: i32 = -32600;
pub const ERROR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERROR_INVALID_PARAMS: i32 = -32602;
pub const ERROR_INTERNAL: i32 = -32603;

// AROS-specific error codes (application range: -32000 to -32099).
pub const ERROR_PERMISSION_DENIED: i32 = -32000;
pub const ERROR_BUDGET_EXCEEDED: i32 = -32001;
pub const ERROR_SECURITY_ZONE: i32 = -32002;
pub const ERROR_ENVELOPE_VERSION: i32 = -32003;

/// Known RPC methods for dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcMethod {
    /// Loop 2 → Loop 1: submit a task.
    TaskSubmit,
    /// Loop 1 → Kernel: report progress.
    TaskProgress,
    /// Loop 1 → Kernel: report completion.
    TaskComplete,
    /// Kernel → Loop 1: request cancellation.
    TaskCancel,
    /// Loop 0 → Kernel: trigger a loop event.
    LoopTrigger,
    /// Health check.
    Ping,
}

impl RpcMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TaskSubmit => "task.submit",
            Self::TaskProgress => "task.progress",
            Self::TaskComplete => "task.complete",
            Self::TaskCancel => "task.cancel",
            Self::LoopTrigger => "loop.trigger",
            Self::Ping => "ping",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "task.submit" => Some(Self::TaskSubmit),
            "task.progress" => Some(Self::TaskProgress),
            "task.complete" => Some(Self::TaskComplete),
            "task.cancel" => Some(Self::TaskCancel),
            "loop.trigger" => Some(Self::LoopTrigger),
            "ping" => Some(Self::Ping),
            _ => None,
        }
    }
}
