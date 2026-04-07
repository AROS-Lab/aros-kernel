//! Dispatch layer — JSON-RPC over Unix domain sockets for inter-loop communication.
//!
//! Loop 2 (Harness) spawns Loop 1 (Agentic) subprocesses, each with a dedicated
//! temp directory and JSON-RPC socket. The kernel mediates all cross-loop dispatch.
//!
//! Canonical types come from sibling modules:
//! - `envelope::task_envelope` — TaskEnvelope, SecurityZone, Priority, ResourceBudget
//! - `supervisor::process` — ProcessId for loop identification

pub mod client;
pub mod contracts;
pub mod rpc;
pub mod server;

pub use client::RpcClient;
pub use contracts::{LoopTrigger, TriggerKind};
pub use rpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, RpcMethod, ERROR_INTERNAL};
pub use server::RpcServer;
