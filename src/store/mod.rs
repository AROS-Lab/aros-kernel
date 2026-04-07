pub mod acl;
pub mod engine;
pub mod error;

pub use acl::{AclGuard, ProcessIdentity};
pub use engine::{CheckpointPolicy, SqliteStateStore};
pub use error::StoreError;

/// The core key-value state store trait.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// async tasks and threads.
pub trait StateStore: Send + Sync {
    fn put(&mut self, key: &str, value: Vec<u8>) -> Result<(), StoreError>;
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn delete(&mut self, key: &str) -> Result<(), StoreError>;
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StoreError>;
    fn exists(&self, key: &str) -> Result<bool, StoreError>;
}
