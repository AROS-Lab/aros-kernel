use std::thread;

use aros_kernel::store::{
    AclGuard, CheckpointPolicy, ProcessIdentity, SqliteStateStore, StateStore, StoreError,
};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn mem_store() -> SqliteStateStore {
    SqliteStateStore::open(":memory:").expect("open in-memory store")
}

// ── Basic CRUD ───────────────────────────────────────────────────────────────

#[test]
fn put_and_get() {
    let mut store = mem_store();
    store.put("/foo", b"bar".to_vec()).unwrap();
    assert_eq!(store.get("/foo").unwrap(), Some(b"bar".to_vec()));
}

#[test]
fn get_missing_key_returns_none() {
    let store = mem_store();
    assert_eq!(store.get("/nonexistent").unwrap(), None);
}

#[test]
fn put_overwrites() {
    let mut store = mem_store();
    store.put("/k", b"v1".to_vec()).unwrap();
    store.put("/k", b"v2".to_vec()).unwrap();
    assert_eq!(store.get("/k").unwrap(), Some(b"v2".to_vec()));
}

#[test]
fn delete_removes_key() {
    let mut store = mem_store();
    store.put("/k", b"v".to_vec()).unwrap();
    store.delete("/k").unwrap();
    assert_eq!(store.get("/k").unwrap(), None);
}

#[test]
fn delete_nonexistent_is_ok() {
    let mut store = mem_store();
    store.delete("/nope").unwrap();
}

#[test]
fn exists_returns_correct_value() {
    let mut store = mem_store();
    assert!(!store.exists("/k").unwrap());
    store.put("/k", b"v".to_vec()).unwrap();
    assert!(store.exists("/k").unwrap());
}

// ── list_keys ────────────────────────────────────────────────────────────────

#[test]
fn list_keys_by_prefix() {
    let mut store = mem_store();
    store.put("/dag/a", b"1".to_vec()).unwrap();
    store.put("/dag/b", b"2".to_vec()).unwrap();
    store.put("/task/x", b"3".to_vec()).unwrap();

    let keys = store.list_keys("/dag/").unwrap();
    assert_eq!(keys, vec!["/dag/a", "/dag/b"]);
}

#[test]
fn list_keys_empty_prefix_returns_all() {
    let mut store = mem_store();
    store.put("/a", b"1".to_vec()).unwrap();
    store.put("/b", b"2".to_vec()).unwrap();

    let keys = store.list_keys("/").unwrap();
    assert_eq!(keys.len(), 2);
}

// ── WAL mode verification ────────────────────────────────────────────────────

#[test]
fn wal_mode_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let store = SqliteStateStore::open(path.to_str().unwrap()).unwrap();

    // After opening, the WAL file should exist (created by PRAGMA journal_mode=WAL).
    // We can also verify by querying the pragma through the store's internals,
    // but the simplest check for a file-backed DB is that the -wal file appears
    // after the first write.
    let mut store = store;
    store.put("/wal-check", b"x".to_vec()).unwrap();

    let wal_path = dir.path().join("test.db-wal");
    assert!(wal_path.exists(), "WAL file should exist after a write");
}

// ── Checkpoint ───────────────────────────────────────────────────────────────

#[test]
fn manual_checkpoint() {
    let mut store = mem_store();
    store.put("/a", b"1".to_vec()).unwrap();
    store.checkpoint().unwrap(); // should not panic
}

#[test]
fn checkpoint_triggers_on_write_threshold() {
    let policy = CheckpointPolicy {
        write_threshold: 3,
        seconds_threshold: 9999, // effectively disabled
    };
    let mut store = SqliteStateStore::open_with_policy(":memory:", policy).unwrap();

    store.put("/a", b"1".to_vec()).unwrap();
    store.put("/b", b"2".to_vec()).unwrap();
    assert_eq!(store.write_count(), 2);

    // Third write should trigger checkpoint, resetting count to 0.
    store.put("/c", b"3".to_vec()).unwrap();
    assert_eq!(store.write_count(), 0, "checkpoint should have reset write_count");
}

// ── Transaction ──────────────────────────────────────────────────────────────

#[test]
fn transaction_commit() {
    let store = mem_store();
    {
        let mut tx = store.transaction().unwrap();
        tx.put("/t1", b"a".to_vec()).unwrap();
        tx.put("/t2", b"b".to_vec()).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(store.get("/t1").unwrap(), Some(b"a".to_vec()));
    assert_eq!(store.get("/t2").unwrap(), Some(b"b".to_vec()));
}

#[test]
fn transaction_rollback_on_drop() {
    let mut store = mem_store();
    store.put("/pre", b"exists".to_vec()).unwrap();
    {
        let mut tx = store.transaction().unwrap();
        tx.put("/t1", b"a".to_vec()).unwrap();
        tx.delete("/pre").unwrap();
        // drop without commit
    }
    // Nothing should have changed.
    assert_eq!(store.get("/pre").unwrap(), Some(b"exists".to_vec()));
    assert_eq!(store.get("/t1").unwrap(), None);
}

// ── ACL enforcement ──────────────────────────────────────────────────────────

#[test]
fn acl_allows_authorized_write() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::HarnessLoop);
    guarded.put("/dag/step1", b"data".to_vec()).unwrap();
    assert_eq!(guarded.get("/dag/step1").unwrap(), Some(b"data".to_vec()));
}

#[test]
fn acl_denies_unauthorized_write() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::AgenticLoop);
    let err = guarded
        .put("/dag/step1", b"data".to_vec())
        .expect_err("should be denied");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));
}

#[test]
fn acl_policy_write_only_by_metaloop() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::HarnessLoop);
    let err = guarded
        .put("/policy/foo", b"x".to_vec())
        .expect_err("HarnessLoop cannot write /policy/");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));

    // MetaLoop should succeed.
    let store2 = mem_store();
    let mut ml = AclGuard::new(store2, ProcessIdentity::MetaLoop);
    ml.put("/policy/foo", b"x".to_vec()).unwrap();
}

#[test]
fn acl_meta_goals_human_only() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::MetaLoop);
    let err = guarded
        .put("/meta-goals/north-star", b"x".to_vec())
        .expect_err("MetaLoop cannot write /meta-goals/");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));

    let store2 = mem_store();
    let mut human = AclGuard::new(store2, ProcessIdentity::Human);
    human.put("/meta-goals/north-star", b"x".to_vec()).unwrap();
}

#[test]
fn acl_security_redzone_read_only() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::Kernel);
    // Even Kernel cannot write to redzone at the ACL layer.
    // Wait — actually Kernel has full access per may_write(). Let's test a non-Kernel.
    let store2 = mem_store();
    let mut ml = AclGuard::new(store2, ProcessIdentity::MetaLoop);
    let err = ml
        .put("/security/redzone/flag", b"x".to_vec())
        .expect_err("redzone is read-only");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));

    // Kernel CAN write (it's the enforcement layer itself).
    guarded.put("/security/redzone/flag", b"x".to_vec()).unwrap();
}

#[test]
fn acl_circuit_breaker_model_adapter_only() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::ModelAdapter);
    guarded.put("/circuit-breaker/llm", b"open".to_vec()).unwrap();

    let store2 = mem_store();
    let mut other = AclGuard::new(store2, ProcessIdentity::AgenticLoop);
    let err = other
        .put("/circuit-breaker/llm", b"x".to_vec())
        .expect_err("AgenticLoop cannot write /circuit-breaker/");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));
}

#[test]
fn acl_task_agentic_loop_only() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::AgenticLoop);
    guarded.put("/task/42", b"running".to_vec()).unwrap();

    let store2 = mem_store();
    let mut other = AclGuard::new(store2, ProcessIdentity::HarnessLoop);
    let err = other
        .put("/task/42", b"x".to_vec())
        .expect_err("HarnessLoop cannot write /task/");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));
}

// ── Append-only enforcement ──────────────────────────────────────────────────

#[test]
fn audit_append_only_put_allowed() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::AgenticLoop);
    guarded.put("/audit/event1", b"log".to_vec()).unwrap();
}

#[test]
fn audit_delete_always_denied() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::Kernel);
    guarded.put("/audit/event1", b"log".to_vec()).unwrap();
    let err = guarded
        .delete("/audit/event1")
        .expect_err("delete on /audit/ should be denied");
    assert!(matches!(err, StoreError::AppendOnly(_)));
}

#[test]
fn evolution_log_delete_always_denied() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::Kernel);
    guarded.put("/evolution-log/v1", b"data".to_vec()).unwrap();
    let err = guarded
        .delete("/evolution-log/v1")
        .expect_err("delete on /evolution-log/ should be denied");
    assert!(matches!(err, StoreError::AppendOnly(_)));
}

#[test]
fn evolution_log_write_only_by_metaloop() {
    let store = mem_store();
    let mut guarded = AclGuard::new(store, ProcessIdentity::HarnessLoop);
    let err = guarded
        .put("/evolution-log/v1", b"x".to_vec())
        .expect_err("HarnessLoop cannot write /evolution-log/");
    assert!(matches!(err, StoreError::PermissionDenied { .. }));
}

// ── Concurrent access ────────────────────────────────────────────────────────

#[test]
fn concurrent_reads_and_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.db");
    let path_str = path.to_str().unwrap().to_string();

    // Seed with some data.
    {
        let mut store = SqliteStateStore::open(&path_str).unwrap();
        for i in 0..10 {
            store.put(&format!("/c/{i}"), vec![i as u8]).unwrap();
        }
    }

    // Spawn readers and writers concurrently using separate connections.
    let mut handles = Vec::new();
    for t in 0..4u8 {
        let p = path_str.clone();
        handles.push(thread::spawn(move || {
            let mut store = SqliteStateStore::open(&p).unwrap();
            // Writer
            for i in 0..20 {
                store
                    .put(&format!("/c/t{t}_{i}"), vec![t, i as u8])
                    .unwrap();
            }
            // Reader
            let keys = store.list_keys("/c/").unwrap();
            assert!(!keys.is_empty());
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }

    // Verify final state has keys from all threads.
    let store = SqliteStateStore::open(&path_str).unwrap();
    let keys = store.list_keys("/c/").unwrap();
    // 10 seed + 4 threads * 20 writes = 90
    assert!(keys.len() >= 90, "expected >= 90 keys, got {}", keys.len());
}

// ── Transaction reads inside scope ──────────────────────────────────────────

#[test]
fn transaction_reads_own_writes() {
    let store = mem_store();
    let mut tx = store.transaction().unwrap();
    tx.put("/tx/a", b"hello".to_vec()).unwrap();
    assert_eq!(tx.get("/tx/a").unwrap(), Some(b"hello".to_vec()));
    assert!(tx.exists("/tx/a").unwrap());
    let keys = tx.list_keys("/tx/").unwrap();
    assert_eq!(keys, vec!["/tx/a"]);
    tx.commit().unwrap();
}
