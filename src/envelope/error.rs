use thiserror::Error;

#[derive(Error, Debug)]
pub enum EnvelopeError {
    #[error("empty task_id")]
    EmptyTaskId,
    #[error("empty parent_dag_id")]
    EmptyDagId,
    #[error("unsupported envelope version: {0}")]
    UnsupportedVersion(u32),
    #[error("invalid resource budget: {0}")]
    InvalidBudget(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
