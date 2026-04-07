use std::sync::Mutex;
use std::time::Instant;

use rusqlite::{Connection, params};

use super::error::StoreError;
use super::StateStore;

/// Configuration for the dual-trigger WAL checkpoint policy.
#[derive(Debug, Clone)]
pub struct CheckpointPolicy {
    /// Checkpoint after this many writes.
    pub write_threshold: u64,
    /// Checkpoint after this many seconds since the last checkpoint.
    pub seconds_threshold: u64,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            write_threshold: 1000,
            seconds_threshold: 60,
        }
    }
}

/// A SQLite-backed key-value state store using WAL mode.
///
/// Thread-safe via an internal `Mutex<Connection>`. Supports configurable
/// dual-trigger WAL checkpoint policy (write count OR elapsed time).
pub struct SqliteStateStore {
    conn: Mutex<Connection>,
    policy: CheckpointPolicy,
    write_count: Mutex<u64>,
    last_checkpoint: Mutex<Instant>,
}

impl SqliteStateStore {
    /// Open (or create) a state store at `path`.
    ///
    /// Pass `":memory:"` for an in-memory database suitable for testing.
    pub fn open(path: &str) -> Result<Self, StoreError> {
        Self::open_with_policy(path, CheckpointPolicy::default())
    }

    /// Open with an explicit checkpoint policy.
    pub fn open_with_policy(path: &str, policy: CheckpointPolicy) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;

        // Enable WAL mode and foreign keys.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;",
        )?;

        // Create the KV table if it doesn't exist.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kv_store (
                 key        TEXT PRIMARY KEY,
                 value      BLOB NOT NULL,
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            policy,
            write_count: Mutex::new(0),
            last_checkpoint: Mutex::new(Instant::now()),
        })
    }

    /// Force a WAL checkpoint (TRUNCATE mode — reclaims WAL file space).
    pub fn checkpoint(&self) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("mutex poisoned");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        *self.write_count.lock().expect("mutex poisoned") = 0;
        *self.last_checkpoint.lock().expect("mutex poisoned") = Instant::now();
        Ok(())
    }

    /// Begin a scoped transaction for atomic multi-key writes.
    ///
    /// The caller receives a `Transaction` that implements `StateStore`.
    /// Call `commit()` to persist, or let it drop to roll back.
    pub fn transaction(&self) -> Result<Transaction<'_>, StoreError> {
        let guard = self.conn.lock().expect("mutex poisoned");
        // We must start the SQL transaction while holding the mutex.
        // Safety: the `Transaction` borrows the `MutexGuard`, keeping the
        // lock held for its entire lifetime.
        guard.execute_batch("BEGIN IMMEDIATE;")?;
        Ok(Transaction { guard, committed: false })
    }

    /// Check the dual-trigger policy and checkpoint if a threshold is hit.
    fn maybe_checkpoint(&self) {
        let count = *self.write_count.lock().expect("mutex poisoned");
        let elapsed = self.last_checkpoint.lock().expect("mutex poisoned").elapsed().as_secs();

        if count >= self.policy.write_threshold || elapsed >= self.policy.seconds_threshold {
            // Best-effort: ignore checkpoint errors during normal writes.
            let _ = self.checkpoint();
        }
    }

    /// Increment the internal write counter.
    fn record_write(&self) {
        *self.write_count.lock().expect("mutex poisoned") += 1;
    }

    /// Return the current write count (for testing).
    pub fn write_count(&self) -> u64 {
        *self.write_count.lock().expect("mutex poisoned")
    }
}

impl StateStore for SqliteStateStore {
    fn put(&mut self, key: &str, value: Vec<u8>) -> Result<(), StoreError> {
        // Also allow calling through a shared reference internally.
        state_store_put(&self.conn, key, value)?;
        self.record_write();
        self.maybe_checkpoint();
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        state_store_get(&self.conn, key)
    }

    fn delete(&mut self, key: &str) -> Result<(), StoreError> {
        state_store_delete(&self.conn, key)?;
        self.record_write();
        self.maybe_checkpoint();
        Ok(())
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        state_store_list_keys(&self.conn, prefix)
    }

    fn exists(&self, key: &str) -> Result<bool, StoreError> {
        state_store_exists(&self.conn, key)
    }
}

// ── Shared helpers (used by both SqliteStateStore and Transaction) ───────────

fn state_store_put(conn: &Mutex<Connection>, key: &str, value: Vec<u8>) -> Result<(), StoreError> {
    let conn = conn.lock().expect("mutex poisoned");
    conn.execute(
        "INSERT INTO kv_store (key, value, updated_at)
         VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value],
    )?;
    Ok(())
}

fn state_store_get(conn: &Mutex<Connection>, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    let conn = conn.lock().expect("mutex poisoned");
    let mut stmt = conn.prepare("SELECT value FROM kv_store WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn state_store_delete(conn: &Mutex<Connection>, key: &str) -> Result<(), StoreError> {
    let conn = conn.lock().expect("mutex poisoned");
    conn.execute("DELETE FROM kv_store WHERE key = ?1", params![key])?;
    Ok(())
}

fn state_store_list_keys(conn: &Mutex<Connection>, prefix: &str) -> Result<Vec<String>, StoreError> {
    let conn = conn.lock().expect("mutex poisoned");
    let pattern = format!("{prefix}%");
    let mut stmt = conn.prepare("SELECT key FROM kv_store WHERE key LIKE ?1 ORDER BY key")?;
    let keys = stmt
        .query_map(params![pattern], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(keys)
}

fn state_store_exists(conn: &Mutex<Connection>, key: &str) -> Result<bool, StoreError> {
    let conn = conn.lock().expect("mutex poisoned");
    let mut stmt = conn.prepare("SELECT 1 FROM kv_store WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    Ok(rows.next()?.is_some())
}

// ── Transaction ─────────────────────────────────────────────────────────────

/// A scoped transaction that holds the connection mutex for its lifetime.
///
/// Provides the same `put`/`get`/`delete`/`list_keys`/`exists` API as
/// `StateStore`, but as inherent methods rather than a trait impl (because
/// `MutexGuard` is neither `Send` nor `Sync`).
///
/// Call `commit()` to persist; dropping without committing will roll back.
pub struct Transaction<'a> {
    guard: std::sync::MutexGuard<'a, Connection>,
    committed: bool,
}

impl Transaction<'_> {
    /// Commit the transaction.
    pub fn commit(mut self) -> Result<(), StoreError> {
        self.guard.execute_batch("COMMIT;")?;
        self.committed = true;
        Ok(())
    }

    pub fn put(&mut self, key: &str, value: Vec<u8>) -> Result<(), StoreError> {
        self.guard.execute(
            "INSERT INTO kv_store (key, value, updated_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let mut stmt = self.guard.prepare("SELECT value FROM kv_store WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn delete(&mut self, key: &str) -> Result<(), StoreError> {
        self.guard
            .execute("DELETE FROM kv_store WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub fn list_keys(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let pattern = format!("{prefix}%");
        let mut stmt = self
            .guard
            .prepare("SELECT key FROM kv_store WHERE key LIKE ?1 ORDER BY key")?;
        let keys = stmt
            .query_map(params![pattern], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }

    pub fn exists(&self, key: &str) -> Result<bool, StoreError> {
        let mut stmt = self
            .guard
            .prepare("SELECT 1 FROM kv_store WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        Ok(rows.next()?.is_some())
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.guard.execute_batch("ROLLBACK;");
        }
    }
}
