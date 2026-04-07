//! RpcClient — Unix domain socket JSON-RPC client for kernel → Loop 1 dispatch.
//!
//! The kernel uses this to send TaskEnvelopes to Loop 1 subprocesses and to
//! issue commands (cancel, ping) over the per-task socket.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::UnixStream;

use super::rpc::{JsonRpcRequest, JsonRpcResponse};

/// Client for sending JSON-RPC requests over a Unix domain socket.
pub struct RpcClient {
    stream: BufReader<OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next_id: AtomicU64,
}

impl RpcClient {
    /// Connect to a JSON-RPC server at the given socket path.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let stream = UnixStream::connect(path).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            stream: BufReader::new(reader),
            writer,
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a request and wait for the response.
    pub async fn call(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse, ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(method, params, id);

        let mut buf = serde_json::to_vec(&request).map_err(ClientError::Serialize)?;
        buf.push(b'\n');

        self.writer
            .write_all(&buf)
            .await
            .map_err(ClientError::Io)?;
        self.writer.flush().await.map_err(ClientError::Io)?;

        let mut line = String::new();
        self.stream
            .read_line(&mut line)
            .await
            .map_err(ClientError::Io)?;

        if line.is_empty() {
            return Err(ClientError::ConnectionClosed);
        }

        serde_json::from_str(&line).map_err(ClientError::Deserialize)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("IO error: {0}")]
    Io(std::io::Error),
    #[error("Serialization error: {0}")]
    Serialize(serde_json::Error),
    #[error("Deserialization error: {0}")]
    Deserialize(serde_json::Error),
    #[error("Connection closed by server")]
    ConnectionClosed,
}
