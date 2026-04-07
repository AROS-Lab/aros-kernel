use thiserror::Error;

#[derive(Error, Debug)]
pub enum GovernorError {
    #[error("unknown priority tier")]
    UnknownTier,
    #[error("system RSS ceiling exceeded")]
    SystemCeilingExceeded,
    #[error("governor not initialized")]
    NotInitialized,
}
