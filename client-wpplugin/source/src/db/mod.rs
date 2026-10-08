pub mod async_jobs;
pub mod bindings;
pub(crate) mod capacity;
mod capacity_vfs;
pub mod components;
pub mod coordination;
pub mod discovery_tasks;
pub mod jobs;
pub mod pending_callbacks;
pub mod proxy;
pub(crate) mod review_attempts;
pub(crate) mod runtime;
pub mod schema;
pub mod sync_inflight;
pub mod system;
pub mod translations;
pub(crate) mod unit_lock;
pub mod vendor;



use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

pub(crate) fn open_db(path: &str) -> Result<Connection> {
    open_db_with_busy_timeout(path, std::time::Duration::from_secs(5))
}

/// open_db with an explicit busy_timeout for lock-heavy callers. The
/// timeout is applied BEFORE the WAL pragma and schema DDL, so it also
/// bounds open_db's own statements under contention. (Rationale: saturated
/// CI runners can blow past rusqlite's 5s default — observed twice on the
/// bursty 8-thread concurrent-writers probe: insert-time DatabaseBusy on
/// the v2.1.3 tag run, then open-time DatabaseBusy in
/// per-thread open_db (DDL + WAL) on windows-latest 2026-09-14 even with
/// the post-open raise to 30s, because that raise landed after open_db's
/// internal statements had already run.)
pub(crate) fn open_db_with_busy_timeout(
    path: &str,
    timeout: std::time::Duration,
) -> Result<Connection> {
    anyhow::ensure!(
        !path.starts_with("file:"),
        "SQLite URI overrides are not storage authority"
    );
    let credit = if path == ":memory:" {
        None
    } else {
        crate::storage_capacity::database_recovery_credit(Path::new(path), true)?
    };
    crate::storage_capacity::with_database_credit(credit, || {
        let conn = capacity_vfs::open(path)?;
        // S3/SEC-02 (03/04 口径, 12 批 A5): the DB carries credentials —
        // owner-only file mode on unix (0600) so at-rest bytes are not
        // world/group readable even before encryption applies.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            {
                // Best-effort hardening: a failure to chmod (e.g. exotic
                // filesystems) must not brick the client, but it is surfaced
                // in logs by the caller's context if the DB then fails.
                eprintln!("warning: could not set 0600 on db file {path}: {err}");
            }
        }
        conn.busy_timeout(timeout)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        schema::create_tables(&conn)?;
        Ok(conn)
    })
}

pub(crate) fn with_read_only_db<T>(
    path: &str,
    read: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let conn = capacity_vfs::open_read_only(path)?;
    // A read can initialize SHM, but may only spend an already admitted DB
    // booking. Do not run schema/bootstrap or reserve new recovery capacity.
    with_recovery_credit(&conn, || read(&conn))
}

pub(crate) fn with_recovery_credit<T>(
    conn: &Connection,
    recover: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let credit = match conn.path().filter(|path| !path.is_empty()) {
        Some(path) => crate::storage_capacity::database_recovery_credit(Path::new(path), false)?,
        None => None,
    };
    crate::storage_capacity::with_database_credit(credit, recover)
}

pub(crate) fn is_storage_exhausted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<crate::storage_capacity::RootCapacityExhausted>()
            || matches!(cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ffi::ErrorCode::DiskFull)
    })
}

pub(crate) fn migrate_from_json_if_needed(conn: &Connection) -> Result<()> {
    if system::get_system_config_checked(conn, "json_migration_done")?.is_some() {
        return Ok(());
    }

    let tx = conn.unchecked_transaction()?;
    system::migrate_device_id_from_file(&tx)?;
    system::migrate_signing_key_from_file(&tx)?;
    bindings::migrate_component_bindings_from_json(&tx)?;
    bindings::migrate_domain_token_bindings_from_json(&tx)?;
    bindings::migrate_task_type_bindings_from_json(&tx)?;
    bindings::migrate_rule_bindings_from_json(&tx)?;
    vendor::migrate_vendor_keys_from_json(&tx)?;
    vendor::migrate_vendor_oauth_from_json(&tx)?;
    proxy::migrate_proxy_profiles_from_json(&tx)?;
    components::migrate_local_components_from_json(&tx)?;

    tx.execute(
        "INSERT OR REPLACE INTO system_config (key, value) VALUES ('json_migration_done', '1')",
        [],
    )?;
    tx.commit()?;

    Ok(())
}

// Private harness serialization, migrated with the private test overlay.
