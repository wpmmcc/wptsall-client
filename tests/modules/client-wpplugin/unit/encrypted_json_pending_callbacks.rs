//! New callback payloads are private; old paid rows are read without migration.
use super::*;

const MARKER: &str = "owned-private-callback-body-sentinel";

fn owned_pending_entry() -> PendingCallbackEntry {
    let mut entry = make_entry("https://owned.invalid", 1, 100);
    entry
        .payload
        .translated_fields
        .insert("post_title".into(), MARKER.into());
    entry
}

fn physical_pending_payload(conn: &Connection, identity: &str) -> String {
    conn.query_row(
        "SELECT payload_json FROM pending_callbacks WHERE idempotency_key=?1",
        [identity],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn encrypted_json_pending_actual_writer_hides_payload_and_replay_keeps_same_ciphertext() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let entry = owned_pending_entry();
    add_pending_callback(&conn, &entry).unwrap();
    let first = physical_pending_payload(&conn, &entry.idempotency_key);
    assert!(
        first.starts_with("V1BUQw"),
        "new pending payload must be encrypted"
    );
    assert!(!first.contains(MARKER));
    let logical: serde_json::Value =
        serde_json::from_str(&crate::db::system::decrypt_config_value(&first).unwrap()).unwrap();
    assert_eq!(logical, serde_json::to_value(&entry.payload).unwrap());
    add_pending_callback(&conn, &entry).unwrap();
    assert_eq!(
        physical_pending_payload(&conn, &entry.idempotency_key),
        first
    );
}

#[test]
fn encrypted_json_pending_unconfirmed_insert_rolls_back_instead_of_authorizing_callback() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    conn.execute_batch(
        "CREATE TRIGGER alter_pending_payload AFTER INSERT ON pending_callbacks
         BEGIN UPDATE pending_callbacks SET payload_json='owned-damaged-projection'
         WHERE idempotency_key=NEW.idempotency_key; END;",
    )
    .unwrap();
    let entry = owned_pending_entry();
    assert!(
        add_pending_callback(&conn, &entry).is_err(),
        "one row is not proof of the payload readback"
    );
    let count: i64 = conn
        .query_row("SELECT count(*) FROM pending_callbacks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "unconfirmed projection must roll back before callback"
    );
}

#[test]
fn encrypted_json_pending_sqlite_reopen_keeps_original_logical_callback_identity() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.sqlite");
    let entry = owned_pending_entry();
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    add_pending_callback(&conn, &entry).unwrap();
    let first = physical_pending_payload(&conn, &entry.idempotency_key);
    drop(conn);
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    let saved = find_pending_callback(&conn, &entry.api_base_url, 1, "post_type", 100)
        .unwrap()
        .unwrap();
    assert_eq!(saved.idempotency_key, entry.idempotency_key);
    assert_eq!(
        serde_json::to_value(saved.payload).unwrap(),
        serde_json::to_value(&entry.payload).unwrap()
    );
    assert_eq!(
        physical_pending_payload(&conn, &entry.idempotency_key),
        first
    );
}

#[test]
fn encrypted_json_pending_wrong_key_corruption_or_body_swap_refuses_without_removing_rows() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let entry = owned_pending_entry();
    add_pending_callback(&conn, &entry).unwrap();
    let before = physical_pending_payload(&conn, &entry.idempotency_key);
    {
        let _wrong = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-wrong-pending-key",
        );
        assert!(list_all_pending_callbacks(&conn).is_err());
        assert_eq!(
            physical_pending_payload(&conn, &entry.idempotency_key),
            before
        );
    }
    let other = make_entry("https://owned.invalid", 1, 200);
    add_pending_callback(&conn, &other).unwrap();
    let swapped = physical_pending_payload(&conn, &other.idempotency_key);
    conn.execute(
        "UPDATE pending_callbacks SET payload_json=?1 WHERE idempotency_key=?2",
        rusqlite::params![swapped, entry.idempotency_key],
    )
    .unwrap();
    assert!(list_all_pending_callbacks(&conn).is_err());
    assert_eq!(
        physical_pending_payload(&conn, &entry.idempotency_key),
        swapped
    );
    let damaged = format!("{}!", before);
    conn.execute(
        "UPDATE pending_callbacks SET payload_json=?1 WHERE idempotency_key=?2",
        rusqlite::params![damaged, entry.idempotency_key],
    )
    .unwrap();
    assert!(list_all_pending_callbacks(&conn).is_err());
    assert_eq!(
        physical_pending_payload(&conn, &entry.idempotency_key),
        damaged
    );
}

#[test]
fn encrypted_json_pending_legacy_body_and_endpoint_are_read_only_without_migration() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let entry = owned_pending_entry();
    let before = serde_json::to_string(&entry.payload).unwrap();
    conn.execute(
        "INSERT INTO pending_callbacks(api_base_url,idempotency_key,payload_json,route_secret_enc,
         created_at,retry_count,last_retry_at,relation_id,object_id,object_type)
         VALUES (?1,?2,?3,?4,?5,0,0,1,100,'post_type')",
        rusqlite::params![
            entry.api_base_url,
            entry.idempotency_key,
            before,
            entry.route_secret,
            entry.created_at
        ],
    )
    .unwrap();
    let found = find_pending_callback(&conn, &entry.api_base_url, 1, "post_type", 100)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(found.payload).unwrap(),
        serde_json::to_value(&entry.payload).unwrap()
    );
    add_pending_callback(&conn, &entry).unwrap();
    assert_eq!(
        physical_pending_payload(&conn, &entry.idempotency_key),
        before
    );
    let stored_url: String = conn
        .query_row("SELECT api_base_url FROM pending_callbacks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored_url, entry.api_base_url);
}

#[test]
fn encrypted_json_pending_no_key_preserves_existing_and_refuses_new_payload() {
    const PROBE: &str = "WPTSALL_OWNED_PENDING_NO_KEY_PROBE";
    if let Ok(root) = std::env::var(PROBE) {
        let root = std::path::PathBuf::from(root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(crate::bindings::bindings_secret().is_none());
        let conn = crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap();
        let original = owned_pending_entry();
        let before = physical_pending_payload(&conn, &original.idempotency_key);
        assert!(list_all_pending_callbacks(&conn).is_err());
        let entry = make_entry("https://owned.invalid", 1, 200);
        assert!(add_pending_callback(&conn, &entry).is_err());
        assert_eq!(
            physical_pending_payload(&conn, &original.idempotency_key),
            before
        );
        let count: i64 = conn
            .query_row("SELECT count(*) FROM pending_callbacks", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        return;
    }
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let conn = crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap();
    add_pending_callback(&conn, &owned_pending_entry()).unwrap();
    drop(conn);
    let executable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let output = std::process::Command::new(executable)
        .args([
            "--exact",
            "db::pending_callbacks::tests::encrypted_json_pending_callbacks::encrypted_json_pending_no_key_preserves_existing_and_refuses_new_payload",
            "--nocapture",
        ])
        .env(PROBE, root.path())
        .env_remove("WPTSALL_COMPONENT_BINDINGS_SECRET")
        .env_remove("WPTSALL_DEVICE_ID")
        .env("WPTSALL_DATA_DIR", root.path())
        .output().unwrap();
    assert!(
        output.status.success(),
        "owned no-key pending child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
