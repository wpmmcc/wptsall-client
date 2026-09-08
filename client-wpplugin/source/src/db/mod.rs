pub mod bindings;
pub mod components;
pub mod coordination;
pub mod discovery_tasks;
pub mod jobs;
pub mod pending_callbacks;
pub mod proxy;
pub mod schema;
pub mod system;
pub mod translations;
pub mod vendor;

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub(crate) fn open_db(path: &str) -> Result<Connection> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    schema::create_tables(&conn)?;
    Ok(conn)
}

pub(crate) fn migrate_from_json_if_needed(conn: &Connection) -> Result<()> {
    let done: bool = conn
        .query_row(
            "SELECT 1 FROM system_config WHERE key = 'json_migration_done'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if done {
        return Ok(());
    }

    // Non-fatal migrations
    let _ = system::migrate_device_id_from_file(conn);
    let _ = system::migrate_signing_key_from_file(conn);
    let _ = bindings::migrate_component_bindings_from_json(conn);
    let _ = bindings::migrate_domain_token_bindings_from_json(conn);
    let _ = bindings::migrate_task_type_bindings_from_json(conn);
    let _ = bindings::migrate_rule_bindings_from_json(conn);
    let _ = vendor::migrate_vendor_keys_from_json(conn);
    let _ = vendor::migrate_vendor_oauth_from_json(conn);
    let _ = proxy::migrate_proxy_profiles_from_json(conn);
    let _ = components::migrate_local_components_from_json(conn);

    conn.execute(
        "INSERT OR REPLACE INTO system_config (key, value) VALUES ('json_migration_done', '1')",
        [],
    )?;

    Ok(())
}

#[cfg(test)]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(test)]
use std::{
    cell::{Cell, RefCell},
    thread_local,
};

#[cfg(test)]
thread_local! {
    static TEST_ENV_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
    static TEST_ENV_LOCK_GUARD: RefCell<Option<std::sync::MutexGuard<'static, ()>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static StdMutex<()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
}

#[cfg(test)]
pub(crate) struct TestEnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

#[cfg(test)]
impl TestEnvVarGuard {
    fn acquire_scope() {
        TEST_ENV_LOCK_DEPTH.with(|depth| {
            let current = depth.get();
            if current == 0 {
                TEST_ENV_LOCK_GUARD.with(|slot| {
                    let guard = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
                    *slot.borrow_mut() = Some(guard);
                });
            }
            depth.set(current + 1);
        });
    }

    fn release_scope() {
        TEST_ENV_LOCK_DEPTH.with(|depth| {
            let current = depth.get();
            debug_assert!(current > 0, "test env lock depth underflow");
            let next = current.saturating_sub(1);
            depth.set(next);
            if next == 0 {
                TEST_ENV_LOCK_GUARD.with(|slot| {
                    *slot.borrow_mut() = None;
                });
            }
        });
    }

    pub(crate) fn set(key: &'static str, value: impl Into<String>) -> Self {
        Self::acquire_scope();
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value.into());
        Self { key, previous }
    }
}

#[cfg(test)]
impl Drop for TestEnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_deref() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
        Self::release_scope();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        open_db(":memory:").expect("in-memory db")
    }

    #[test]
    fn test_open_db_creates_tables() {
        let conn = make_db();
        // Verify system_config table exists by querying it
        let result: rusqlite::Result<i64> =
            conn.query_row("SELECT COUNT(*) FROM system_config", [], |row| row.get(0));
        assert!(result.is_ok(), "system_config table should exist");
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn open_db_busy_timeout_default_provides_bounded_wait() {
        // Live probe (2026-09-08) DISPROVED the FM-DB-LOCK premise "no
        // busy_timeout configured": rusqlite's Connection::open applies a
        // default 5000ms busy_timeout, so lock contention waits up to 5s
        // instead of failing with an immediate SQLITE_BUSY. Hard-assert the
        // bounded wait. Unproven remainder tracked on FM-DB-LOCK notes:
        // explicit/configurable timeout, concurrent-writer integrity,
        // integrity_check on open.
        let conn = make_db();
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("PRAGMA busy_timeout must be queryable");
        assert!(
            timeout >= 5000,
            "busy_timeout must provide a bounded wait for lock contention, got {timeout}ms"
        );
    }

    #[test]
    fn concurrent_writers_preserve_wal_integrity() {
        // DB-03 open half (2026-09-08, Phase 4 L3 round 6): multi-connection
        // concurrent writes through the REAL open_db path — every open runs
        // schema DDL (create_tables), so this also probes DB-03's hot-path
        // DDL concern. WAL + the default busy_timeout must land every row
        // and leave the file passing PRAGMA integrity_check.
        let dir = std::env::temp_dir().join(format!(
            "wptsall-db-concurrent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("concurrent.db");
        let path_str = db_path.to_str().unwrap().to_string();

        {
            let conn = open_db(&path_str).expect("initial open_db");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS concurrent_probe (
                    id        INTEGER PRIMARY KEY,
                    thread_id INTEGER NOT NULL,
                    seq       INTEGER NOT NULL
                );",
            )
            .expect("probe table");
        }

        const THREADS: usize = 8;
        const INSERTS: usize = 50;
        let mut handles = Vec::new();
        for t in 0..THREADS {
            let p = path_str.clone();
            handles.push(std::thread::spawn(move || {
                let conn = open_db(&p).expect("per-thread open_db (DDL + WAL)");
                for i in 0..INSERTS {
                    conn.execute(
                        "INSERT INTO concurrent_probe (thread_id, seq) VALUES (?1, ?2)",
                        rusqlite::params![t as i64, i as i64],
                    )
                    .expect("concurrent insert");
                }
            }));
        }
        for h in handles {
            h.join().expect("writer thread join");
        }

        let conn = open_db(&path_str).expect("verify open_db");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM concurrent_probe", [], |row| row.get(0))
            .expect("count");
        assert_eq!(
            count as usize,
            THREADS * INSERTS,
            "every concurrent write must land — no silent losses"
        );
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("integrity_check");
        assert_eq!(
            integrity, "ok",
            "WAL database must pass integrity_check after concurrent writers"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_migrate_from_json_if_needed_idempotent() {
        let conn = make_db();

        // First call: performs migration and sets the done flag
        migrate_from_json_if_needed(&conn).unwrap();

        // Verify done flag was set
        let done_flag = system::get_system_config(&conn, "json_migration_done");
        assert_eq!(done_flag, Some("1".to_string()));

        // Second call should be a no-op (no error, idempotent)
        migrate_from_json_if_needed(&conn).unwrap();

        // Done flag should still be "1"
        let done_flag2 = system::get_system_config(&conn, "json_migration_done");
        assert_eq!(done_flag2, Some("1".to_string()));
    }

    #[test]
    fn test_migrate_sets_done_flag() {
        let conn = make_db();

        // Before migration, done flag should not exist
        let before = system::get_system_config(&conn, "json_migration_done");
        assert_eq!(before, None);

        // Run migration
        migrate_from_json_if_needed(&conn).unwrap();

        // After migration, done flag should be "1"
        let after = system::get_system_config(&conn, "json_migration_done");
        assert_eq!(after, Some("1".to_string()));
    }

    /// Verify that pending_callbacks has the relation_id/object_id/object_type
    /// columns
    /// after create_tables() (fresh DB).
    #[test]
    fn test_pending_callbacks_has_new_columns() {
        let conn = make_db();
        // Insert a row using the new columns to prove they exist
        conn.execute(
            "INSERT INTO pending_callbacks
             (api_base_url, idempotency_key, payload_json, created_at, relation_id, object_id, object_type)
             VALUES ('https://x.com', 'key1', '{}', 0, 5, 42, 'post_type')",
            [],
        )
        .expect("insert with new columns should succeed");

        let (rel, obj, obj_type): (i64, i64, String) = conn
            .query_row(
                "SELECT relation_id, object_id, object_type FROM pending_callbacks WHERE idempotency_key = 'key1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("should read back inserted values");
        assert_eq!(rel, 5);
        assert_eq!(obj, 42);
        assert_eq!(obj_type, "post_type");
    }

    /// Verify the ALTER TABLE upgrade path: simulate an old database that has the
    /// pending_callbacks table WITHOUT relation_id/object_id/object_type, then call create_tables()
    /// again and confirm the columns are added (idempotent schema upgrade).
    #[test]
    fn test_alter_table_upgrade_adds_new_columns() {
        let conn = Connection::open(":memory:").expect("in-memory");
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .unwrap();

        // Create the old-style pending_callbacks table without the new columns.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_callbacks (
                api_base_url     TEXT NOT NULL,
                idempotency_key  TEXT PRIMARY KEY NOT NULL,
                payload_json     TEXT NOT NULL DEFAULT '{}',
                route_secret_enc TEXT,
                created_at       INTEGER NOT NULL DEFAULT 0,
                retry_count      INTEGER NOT NULL DEFAULT 0,
                last_retry_at    INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();

        // Insert a row into the old schema to ensure existing data is preserved.
        conn.execute(
            "INSERT INTO pending_callbacks (api_base_url, idempotency_key, payload_json)
             VALUES ('https://old.com', 'old-key', '{}')",
            [],
        )
        .unwrap();

        // Now run create_tables() — the ALTER TABLE calls should add the new columns.
        schema::create_tables(&conn).expect("create_tables on existing db should succeed");

        // Verify new columns exist and have default values for the old row.
        let (rel, obj, obj_type): (i64, i64, String) = conn
            .query_row(
                "SELECT relation_id, object_id, object_type FROM pending_callbacks WHERE idempotency_key = 'old-key'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("columns should exist after ALTER TABLE");
        assert_eq!(rel, 0, "old row gets DEFAULT 0 for relation_id");
        assert_eq!(obj, 0, "old row gets DEFAULT 0 for object_id");
        assert_eq!(
            obj_type, "post_type",
            "old row is normalized to 'post_type' for object_type"
        );

        // Verify calling create_tables() a second time is safe (idempotent).
        schema::create_tables(&conn).expect("second create_tables call should be idempotent");
    }
}
