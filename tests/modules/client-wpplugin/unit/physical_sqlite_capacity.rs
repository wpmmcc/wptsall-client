use super::*;
use std::path::Path;

fn sqlite_policy(root: &Path, max: u64) {
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        serde_json::to_vec(&serde_json::json!({
            "format": "wptsall-storage-v1",
            "max_physical_bytes": max,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn owned_sqlite(root: &Path) -> Connection {
    let conn = open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
    conn.execute_batch(
        "PRAGMA wal_autocheckpoint=0;
         CREATE TABLE owned_capacity_probe(receipt TEXT PRIMARY KEY, body BLOB NOT NULL);
         INSERT INTO owned_capacity_probe VALUES ('retained-paid-receipt', X'010203');",
    )
    .unwrap();
    conn
}

fn retained_receipt(conn: &Connection) {
    assert_eq!(
        conn.query_row(
            "SELECT body FROM owned_capacity_probe WHERE receipt='retained-paid-receipt'",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .unwrap(),
        [1, 2, 3]
    );
}

#[test]
fn physical_sqlite_full_root_refuses_new_schema_before_creating_database() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    sqlite_policy(root.path(), 1);
    let path = root.path().join("new.sqlite");
    assert!(
        open_db(path.to_str().unwrap()).is_err(),
        "a full root must not silently create a SQLite schema beyond its physical limit"
    );
    assert!(
        !path.exists(),
        "refusal must precede a new persistent database"
    );
}

#[test]
fn physical_sqlite_uri_override_refuses_before_any_directory_or_booking() {
    const CHILD: &str = "WPTSALL_OWNED_SQLITE_URI_PROBE";
    if std::env::var_os(CHILD).is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("WPTSALL_DATA_DIR").unwrap());
        assert_eq!(std::env::current_dir().unwrap(), root);
        let uri = format!("file:{}?vfs=unix", root.join("denied.sqlite").display());
        assert!(open_db(&uri).is_err());
        assert!(
            !root.join("file:").exists(),
            "reject a SQLite URI before treating it as a directory authority"
        );
        assert!(
            !root.join(".wptsall-storage-bookings-v1").exists(),
            "rejected URI must not consume a database recovery booking"
        );
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("sqlite-uri-capacity-")
        .tempdir()
        .unwrap()
        .keep();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::physical_sqlite_capacity::physical_sqlite_uri_override_refuses_before_any_directory_or_booking",
            "--nocapture",
        ])
        .current_dir(&root)
        .env(CHILD, "1")
        .env("WPTSALL_DATA_DIR", &root)
        .env_remove("WPTSALL_COMPONENT_BINDINGS_SECRET")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "owned URI probe at {} failed: {}{}",
        root.display(),
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

#[test]
fn physical_sqlite_wal_growth_refuses_and_retains_original_receipt() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let conn = owned_sqlite(root.path());
    let wal = root.path().join("owned.sqlite-wal");
    let before = wal.metadata().unwrap().len();
    sqlite_policy(root.path(), 1);
    assert!(
        conn.execute(
            "INSERT INTO owned_capacity_probe VALUES ('unadmitted-new-work', ?1)",
            [vec![7u8; 256 * 1024]],
        )
        .is_err(),
        "actual WAL append must participate in root admission"
    );
    assert_eq!(wal.metadata().unwrap().len(), before);
    retained_receipt(&conn);
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM owned_capacity_probe WHERE receipt='unadmitted-new-work'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
}

#[test]
fn physical_sqlite_rollback_journal_growth_refuses_without_partial_commit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let conn = owned_sqlite(root.path());
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .unwrap();
    let path = root.path().join("owned.sqlite");
    let before = std::fs::read(&path).unwrap();
    sqlite_policy(root.path(), 1);
    assert!(
        conn.execute(
            "INSERT INTO owned_capacity_probe VALUES ('unadmitted-journal-work', ?1)",
            [vec![8u8; 256 * 1024]],
        )
        .is_err(),
        "rollback journal and main-file growth must not bypass root quota"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    retained_receipt(&conn);
}

#[test]
fn physical_sqlite_checkpoint_checks_actual_main_file_growth() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let conn = owned_sqlite(root.path());
    conn.execute(
        "INSERT INTO owned_capacity_probe VALUES ('known-paid-wal-result', ?1)",
        [vec![9u8; 256 * 1024]],
    )
    .unwrap();
    let path = root.path().join("owned.sqlite");
    let before = path.metadata().unwrap().len();
    sqlite_policy(root.path(), 1);
    let checkpoint = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    assert!(
        checkpoint.is_err() || checkpoint.as_ref().unwrap().0 != 0,
        "checkpoint must not silently grow the main DB beyond a lowered limit: {checkpoint:?}"
    );
    assert_eq!(path.metadata().unwrap().len(), before);
    retained_receipt(&conn);
    assert_eq!(
        conn.query_row(
            "SELECT LENGTH(body) FROM owned_capacity_probe WHERE receipt='known-paid-wal-result'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        256 * 1024
    );
}
