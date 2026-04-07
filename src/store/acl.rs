use super::error::StoreError;
use super::StateStore;

/// Identity of the calling process, used for ACL enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessIdentity {
    Kernel,
    MetaLoop,
    HarnessLoop,
    AgenticLoop,
    ModelAdapter,
    Human,
}

impl std::fmt::Display for ProcessIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// A wrapper around any `StateStore` that enforces per-process write
/// permissions based on key prefix.
pub struct AclGuard<S: StateStore> {
    inner: S,
    identity: ProcessIdentity,
}

impl<S: StateStore> AclGuard<S> {
    pub fn new(inner: S, identity: ProcessIdentity) -> Self {
        Self { inner, identity }
    }

    /// Return a reference to the underlying store (bypasses ACL — use with care).
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Consume the guard and return the underlying store.
    pub fn into_inner(self) -> S {
        self.inner
    }

    // ── ACL logic ────────────────────────────────────────────────────

    /// Check whether `self.identity` may perform `operation` on `key`.
    fn check_write(&self, key: &str, operation: &str) -> Result<(), StoreError> {
        // Append-only prefixes: delete is ALWAYS denied regardless of identity.
        if operation == "delete" && is_append_only(key) {
            return Err(StoreError::AppendOnly(key.to_owned()));
        }

        // Per-prefix write rules.
        if !may_write(self.identity, key) {
            return Err(StoreError::PermissionDenied {
                identity: self.identity.to_string(),
                operation: operation.to_owned(),
                key: key.to_owned(),
            });
        }

        Ok(())
    }
}

/// Returns `true` if the key belongs to an append-only namespace.
fn is_append_only(key: &str) -> bool {
    key.starts_with("/audit/") || key.starts_with("/evolution-log/")
}

/// Returns `true` if `identity` is allowed to write (put) to `key`.
fn may_write(identity: ProcessIdentity, key: &str) -> bool {
    use ProcessIdentity::*;

    // The Kernel has full access — it is the enforcement layer itself.
    if identity == Kernel {
        return true;
    }

    if key.starts_with("/policy/") {
        return identity == MetaLoop;
    }
    if key.starts_with("/meta-goals/") {
        return identity == Human;
    }
    if key.starts_with("/security/redzone/") {
        // Read-only for all (compiled into kernel). No runtime writes.
        return false;
    }
    if key.starts_with("/audit/") {
        // Append-only: put allowed for any identity (delete handled separately).
        return true;
    }
    if key.starts_with("/circuit-breaker/") {
        return identity == ModelAdapter;
    }
    if key.starts_with("/dag/") {
        return identity == HarnessLoop;
    }
    if key.starts_with("/task/") {
        return identity == AgenticLoop;
    }
    if key.starts_with("/evolution-log/") {
        return identity == MetaLoop;
    }
    if key.starts_with("/self-model/") {
        return identity == MetaLoop;
    }

    // Keys outside any defined prefix: allow writes (no restriction).
    true
}

impl<S: StateStore> StateStore for AclGuard<S> {
    fn put(&mut self, key: &str, value: Vec<u8>) -> Result<(), StoreError> {
        self.check_write(key, "put")?;
        self.inner.put(key, value)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        // Reads are unrestricted.
        self.inner.get(key)
    }

    fn delete(&mut self, key: &str) -> Result<(), StoreError> {
        self.check_write(key, "delete")?;
        self.inner.delete(key)
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.inner.list_keys(prefix)
    }

    fn exists(&self, key: &str) -> Result<bool, StoreError> {
        self.inner.exists(key)
    }
}
