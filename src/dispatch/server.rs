//! RpcServer — Unix domain socket JSON-RPC server for kernel-side dispatch.
//!
//! Each Loop 1 subprocess connects back to the kernel via its dedicated socket.
//! The server handles task progress reports, completion signals, and tool requests.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::broadcast;

use super::rpc::{
    JsonRpcRequest, JsonRpcResponse, RpcMethod, ERROR_INTERNAL, ERROR_METHOD_NOT_FOUND,
    ERROR_PARSE,
};

/// Handler trait for processing incoming JSON-RPC requests.
///
/// Kernel modules implement this to handle specific RPC methods.
pub trait RequestHandler: Send + Sync + 'static {
    fn handle(
        &self,
        method: RpcMethod,
        params: Option<Value>,
    ) -> impl std::future::Future<Output = Result<Value, (i32, String)>> + Send;
}

/// Unix domain socket JSON-RPC server.
pub struct RpcServer {
    socket_path: PathBuf,
    shutdown_tx: broadcast::Sender<()>,
}

impl RpcServer {
    /// Create a new server bound to the given socket path.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            socket_path: socket_path.into(),
            shutdown_tx,
        }
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Signal all connections to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    /// Start accepting connections. Blocks until shutdown is signaled.
    ///
    /// Each connection is handled on a separate tokio task.
    /// Protocol: newline-delimited JSON-RPC 2.0 messages.
    pub async fn serve<H: RequestHandler + Clone + 'static>(
        &self,
        handler: H,
    ) -> Result<(), std::io::Error> {
        // Remove stale socket file if it exists.
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tracing::info!(path = %self.socket_path.display(), "RPC server listening");

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    let (stream, _addr) = accept?;
                    let handler = handler.clone();
                    let mut conn_shutdown = self.shutdown_tx.subscribe();

                    tokio::spawn(async move {
                        let (reader, mut writer) = stream.into_split();
                        let mut lines = BufReader::new(reader).lines();

                        loop {
                            tokio::select! {
                                line = lines.next_line() => {
                                    match line {
                                        Ok(Some(text)) => {
                                            let response = Self::process_line(&handler, &text).await;
                                            let mut buf = serde_json::to_vec(&response)
                                                .unwrap_or_default();
                                            buf.push(b'\n');
                                            if writer.write_all(&buf).await.is_err() {
                                                break;
                                            }
                                        }
                                        Ok(None) => break, // EOF
                                        Err(_) => break,
                                    }
                                }
                                _ = conn_shutdown.recv() => break,
                            }
                        }
                    });
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("RPC server shutting down");
                    break;
                }
            }
        }

        // Clean up socket file.
        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }

    async fn process_line<H: RequestHandler>(handler: &H, line: &str) -> JsonRpcResponse {
        let request: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse::error(
                    Value::Null,
                    ERROR_PARSE,
                    &format!("Parse error: {e}"),
                    None,
                );
            }
        };

        let id = request.id.clone();

        let Some(method) = RpcMethod::from_str(&request.method) else {
            return JsonRpcResponse::error(
                id,
                ERROR_METHOD_NOT_FOUND,
                &format!("Unknown method: {}", request.method),
                None,
            );
        };

        match handler.handle(method, request.params).await {
            Ok(result) => JsonRpcResponse::success(id, result),
            Err((code, msg)) => JsonRpcResponse::error(id, code, &msg, None),
        }
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// A simple ping handler for health checks and testing.
#[derive(Clone)]
pub struct PingHandler;

impl RequestHandler for PingHandler {
    async fn handle(
        &self,
        method: RpcMethod,
        _params: Option<Value>,
    ) -> Result<Value, (i32, String)> {
        match method {
            RpcMethod::Ping => Ok(Value::String("pong".to_string())),
            _ => Err((ERROR_INTERNAL, "Not implemented".to_string())),
        }
    }
}
