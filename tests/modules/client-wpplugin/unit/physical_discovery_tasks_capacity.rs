use super::*;

#[test]
fn physical_sqlite_discovery_original_task_initialization_at_quota_is_read_only() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-discovery-task-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let conn = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
    ensure_discovery_tasks(&conn, "http://127.0.0.1:9", &[7]).unwrap();
    let before = serde_json::to_value(list_discovery_tasks(&conn).unwrap()).unwrap();
    let sequence: i64 = conn
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='discovery_tasks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy, 0);
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    eprintln!("OWNED_DISCOVERY_TASK_ROOT={}", root.display());
    let result = ensure_discovery_tasks(&conn, "http://127.0.0.1:9", &[7]);
    assert!(
        result.is_ok(),
        "already-owned task must not allocate an ignored sequence: {result:?}"
    );
    assert_eq!(
        serde_json::to_value(list_discovery_tasks(&conn).unwrap()).unwrap(),
        before
    );
    let current: i64 = conn
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='discovery_tasks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current, sequence);
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
    assert_eq!(
        crate::storage_capacity::inventory_at(&root)
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[test]
fn physical_sqlite_discovery_fresh_task_cannot_borrow_ambient_database_credit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-discovery-fresh-task-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let conn = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy, 0);
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    let credit =
        crate::storage_capacity::database_recovery_credit(&root.join("owned.sqlite"), false)
            .unwrap();
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    eprintln!("OWNED_DISCOVERY_TASK_ROOT={}", root.display());
    let result = crate::storage_capacity::with_database_credit(credit, || {
        ensure_discovery_tasks(&conn, "http://127.0.0.1:9", &[7])
    });
    assert!(
        result.is_err(),
        "new discovery settings must not use the original DB booking"
    );
    assert!(list_discovery_tasks(&conn).unwrap().is_empty());
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
    assert_eq!(
        crate::storage_capacity::inventory_at(&root)
            .unwrap()
            .root_booked_bytes,
        booked
    );
}
