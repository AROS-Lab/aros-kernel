use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Permission denied: {identity} cannot {operation} on key '{key}'")]
    PermissionDenied {
        identity: String,
        operation: String,
        key: String,
    },

    #[error("Append-only key: delete not allowed on '{0}'")]
    AppendOnly(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}
