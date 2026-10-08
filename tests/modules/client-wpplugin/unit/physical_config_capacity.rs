// Owned Linux physical quota oracle; no live site or provider is contacted.
use super::*;
use crate::web_ui::routes::integrations::config_store::{load_config, save_config};

#[tokio::test]
async fn physical_sqlite_config_projected_original_journal_closes_after_quota_drop() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-config-journal-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = db.clone();
    let path = root.join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    let after = VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([(
            "owned".into(),
            make_test_oauth_config("http://127.0.0.1:1/token".into()),
        )]),
    };
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER retain_owned_config_receipt BEFORE DELETE ON system_config
             WHEN OLD.key='integration-save-v1:vendor_oauth_doc'
             BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let result = save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &after,
    )
    .await;
    assert!(
        result.is_err(),
        "owned trigger must retain the committed journal after file projection"
    );
    let original_file = std::fs::read(&path).unwrap();
    let projected = crate::bindings::load_vendor_oauth(path.to_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(projected).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    let (original_doc, original_journal) = {
        let conn = db.lock().await;
        conn.execute_batch("DROP TRIGGER retain_owned_config_receipt;")
            .unwrap();
        let doc = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap();
        let journal = crate::db::system::get_system_config_checked(
            &conn,
            "integration-save-v1:vendor_oauth_doc",
        )
        .unwrap()
        .unwrap();
        assert!(doc.starts_with("V1BUQw") && journal.starts_with("V1BUQw"));
        let decoded: Value =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&journal).unwrap())
                .unwrap();
        assert_eq!(decoded["db_value"], doc);
        assert_eq!(decoded["after"], serde_json::to_value(&after).unwrap());
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        (doc, journal)
    };
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    assert!(booked > 64 * 1024);
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let recovered: anyhow::Result<VendorOAuthDoc> =
        load_config(&state, path.to_str().unwrap(), "vendor_oauth_doc").await;
    let error = recovered.as_ref().err().map(ToString::to_string);
    assert!(
        recovered.is_ok(),
        "original config file already projected must close its exact DB journal with prior credit: root={} error={error:?} original_journal_encrypted={}",
        root.display(),
        original_journal.starts_with("V1BUQw")
    );
    assert_eq!(
        serde_json::to_value(recovered.unwrap()).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original_file);
    assert!(root.join("owned.sqlite-wal").metadata().unwrap().len() > 0);
    let conn = db.lock().await;
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap(),
        original_doc
    );
    assert!(crate::db::system::get_system_config_checked(
        &conn,
        "integration-save-v1:vendor_oauth_doc"
    )
    .unwrap()
    .is_none());
}

async fn config_recovery_boundary(changed: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = db.clone();
    let path = root.path().join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    let after = VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([(
            "owned".into(),
            make_test_oauth_config("http://127.0.0.1:1/token".into()),
        )]),
    };
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER retain_owned_config_receipt BEFORE DELETE ON system_config
         WHEN OLD.key='integration-save-v1:vendor_oauth_doc' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    assert!(save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &after
    )
    .await
    .is_err());
    db.lock()
        .await
        .execute_batch("DROP TRIGGER retain_owned_config_receipt;")
        .unwrap();
    match changed {
        "file" => {
            let mut newer = after.clone();
            newer.configs.get_mut("owned").unwrap().client_id = "owned-newer".into();
            crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &newer).unwrap();
        }
        "unprojected" => {
            crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
        }
        "missing" => std::fs::remove_file(&path).unwrap(),
        "authority" => {
            let mut newer = after.clone();
            newer.configs.get_mut("owned").unwrap().client_id = "owned-newer".into();
            crate::db::system::set_encrypted_config(
                &*db.lock().await,
                "vendor_oauth_doc",
                &serde_json::to_string(&newer).unwrap(),
            )
            .unwrap();
        }
        "scope" => {
            let conn = db.lock().await;
            let raw = crate::db::system::get_system_config_checked(
                &conn,
                "integration-save-v1:vendor_oauth_doc",
            )
            .unwrap()
            .unwrap();
            let mut saved: Value =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap())
                    .unwrap();
            saved["path"] = json!(root.path().join("not-the-original.json").to_str().unwrap());
            crate::db::system::set_encrypted_config(
                &conn,
                "integration-save-v1:vendor_oauth_doc",
                &saved.to_string(),
            )
            .unwrap();
        }
        "complete" => {
            let _: VendorOAuthDoc = load_config(&state, path.to_str().unwrap(), "vendor_oauth_doc")
                .await
                .unwrap();
        }
        "busy" => {}
        _ => unreachable!(),
    }
    let (doc, journal) = {
        let conn = db.lock().await;
        let doc = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc").unwrap();
        let journal = crate::db::system::get_system_config_checked(
            &conn,
            "integration-save-v1:vendor_oauth_doc",
        )
        .unwrap();
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        (doc, journal)
    };
    let bytes = std::fs::read(&path).ok();
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    let credit =
        crate::storage_capacity::database_recovery_credit(&root.path().join("owned.sqlite"), false)
            .unwrap();
    let held = (changed == "busy")
        .then(|| crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap());
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let result: anyhow::Result<VendorOAuthDoc> = crate::storage_capacity::with_result_credit(
        credit,
        load_config(&state, path.to_str().unwrap(), "vendor_oauth_doc"),
    )
    .await;
    drop(held);
    if changed == "complete" {
        assert!(
            result.is_ok(),
            "completed configuration replays read-only: {result:?}"
        );
    } else {
        assert!(
            result.is_err(),
            "changed/unprojected configuration cannot borrow DB credit: {changed}"
        );
    }
    assert_eq!(std::fs::read(&path).ok(), bytes);
    let conn = db.lock().await;
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc").unwrap(),
        doc
    );
    assert_eq!(crate::db::system::get_system_config_checked(
        &conn, "integration-save-v1:vendor_oauth_doc",
    ).unwrap(), journal);
    drop(conn);
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[tokio::test]
async fn physical_sqlite_config_changed_or_unprojected_authority_cannot_borrow_credit() {
    for changed in [
        "file",
        "authority",
        "scope",
        "unprojected",
        "missing",
        "busy",
    ] {
        config_recovery_boundary(changed).await;
    }
}

#[tokio::test]
async fn physical_sqlite_config_completed_replay_at_quota_is_read_only() {
    config_recovery_boundary("complete").await;
}

#[tokio::test]
async fn physical_sqlite_config_fresh_save_cannot_borrow_ambient_db_credit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = db.clone();
    let path = root.path().join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &before,
    )
    .await
    .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let raw = {
        let conn = db.lock().await;
        let raw = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc").unwrap();
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        raw
    };
    let credit =
        crate::storage_capacity::database_recovery_credit(&root.path().join("owned.sqlite"), false)
            .unwrap();
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let after = VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([(
            "owned".into(),
            make_test_oauth_config("http://127.0.0.1:1/token".into()),
        )]),
    };
    let result = crate::storage_capacity::with_result_credit(
        credit,
        save_config(
            &state,
            path.to_str().unwrap(),
            "vendor_oauth_doc",
            &before,
            &after,
        ),
    )
    .await;
    assert!(
        result.is_err(),
        "fresh config cannot spend original DB recovery booking"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let conn = db.lock().await;
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc").unwrap(),
        raw
    );
    assert!(crate::db::system::get_system_config_checked(
        &conn,
        "integration-save-v1:vendor_oauth_doc",
    )
    .unwrap()
    .is_none());
    drop(conn);
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}
