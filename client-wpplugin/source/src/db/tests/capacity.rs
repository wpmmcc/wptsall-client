use super::*;

fn owned_data_root() -> (tempfile::TempDir, crate::db::TestEnvVarGuard) {
    let root = tempfile::tempdir().unwrap();
    let guard = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    (root, guard)
}

fn database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::create_tables(&conn).unwrap();
    conn
}

#[test]
fn capacity_ignored_reservation_cannot_authorize_work() {
    let (_root, _data) = owned_data_root();
    let conn = database();
    conn.execute_batch(
        "CREATE TRIGGER refuse_capacity BEFORE INSERT ON retained_capacity
         BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    assert!(reserve_new(&tx, "owned", TEXT_RESERVATION_BYTES).is_err());
    assert_eq!(inventory(&tx).unwrap().retained_units, 0);
}

#[test]
fn capacity_reservation_rolls_back_with_failed_operation() {
    let (_root, _data) = owned_data_root();
    let conn = database();
    {
        let tx = conn.unchecked_transaction().unwrap();
        reserve_new(&tx, "owned", TEXT_RESERVATION_BYTES).unwrap();
        assert_eq!(inventory(&tx).unwrap().retained_units, 1);
    }
    assert_eq!(inventory(&conn).unwrap().retained_units, 0);
}

#[test]
fn capacity_damaged_inventory_refuses_instead_of_returning_zero() {
    let (_root, _data) = owned_data_root();
    let conn = database();
    conn.execute_batch("ALTER TABLE retained_capacity RENAME TO damaged_capacity;")
        .unwrap();
    assert!(inventory(&conn).is_err());
    let tx = conn.unchecked_transaction().unwrap();
    assert!(reserve_new(&tx, "owned", TEXT_RESERVATION_BYTES).is_err());
}

#[test]
fn capacity_legacy_operations_count_without_rewriting_them() {
    let (_root, _data) = owned_data_root();
    let conn = database();
    crate::db::system::set_system_config(
        &conn,
        "provider-operation-v1:owned-legacy",
        "unchanged-legacy-evidence",
    )
    .unwrap();
    let current = inventory(&conn).unwrap();
    assert_eq!(current.retained_units, 1);
    assert_eq!(current.legacy_units, 1);
    assert_eq!(current.reserved_bytes, MEDIA_RESERVATION_BYTES);
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "provider-operation-v1:owned-legacy")
            .unwrap()
            .as_deref(),
        Some("unchanged-legacy-evidence")
    );
}

#[test]
fn capacity_independent_connections_share_one_last_slot_and_retain_on_restart() {
    let (_data_root, _data) = owned_data_root();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.db");
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    crate::db::system::set_system_config(&conn, "storage_max_retained_units", "1").unwrap();
    drop(conn);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut threads = Vec::new();
    for index in 0..8 {
        let barrier = barrier.clone();
        let path = path.clone();
        threads.push(std::thread::spawn(move || {
            let conn = Connection::open(path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            barrier.wait();
            let tx = conn.unchecked_transaction().unwrap();
            match reserve_new(&tx, &format!("owned-{index}"), TEXT_RESERVATION_BYTES) {
                Ok(()) => tx.commit().is_ok(),
                Err(_) => false,
            }
        }));
    }
    let admitted = threads
        .into_iter()
        .map(|thread| usize::from(thread.join().unwrap()))
        .sum::<usize>();
    assert_eq!(
        admitted, 1,
        "there is one durable last slot, not one per runtime"
    );
    let conn = Connection::open(&path).unwrap();
    assert_eq!(inventory(&conn).unwrap().retained_units, 1);
    let tx = conn.unchecked_transaction().unwrap();
    assert!(reserve_new(&tx, "owned-restart", TEXT_RESERVATION_BYTES).is_err());
}

#[test]
fn capacity_pending_operator_projection_stops_new_admission_but_not_known_paid_recording() {
    let (_root, _data) = owned_data_root();
    let conn = database();
    crate::db::system::set_system_config(
        &conn,
        "integration-save-v1:vendor_keys_doc",
        "retained-owned-journal",
    )
    .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    let refused = reserve_new(&tx, "owned-new", TEXT_RESERVATION_BYTES);
    assert!(
        refused.is_err(),
        "an unresolved credential/proxy save must fence new external work"
    );
    assert_eq!(inventory(&tx).unwrap().retained_units, 0);
    record_existing(&tx, "owned-known-paid", TEXT_RESERVATION_BYTES).unwrap();
    assert_eq!(inventory(&tx).unwrap().retained_units, 1);
}
