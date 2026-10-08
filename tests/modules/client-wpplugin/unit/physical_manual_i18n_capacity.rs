// Owned Linux physical quota oracle; no live site or provider is contacted.
use super::*;

#[tokio::test]
async fn physical_sqlite_manual_i18n_existing_candidate_projects_original_uuid_at_quota() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-manual-i18n-candidate-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let item = {
        let conn = db.lock().await;
        let item = fixture(&conn);
        conn.execute(
            "UPDATE translation_items SET object_type='language_pack',
             business_line='plugin_i18n',wp_object_subtype='plugin' WHERE id=?1",
            [item.id],
        )
        .unwrap();
        crate::db::jobs::get_item_checked(&conn, item.id)
            .unwrap()
            .unwrap()
    };
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let envelope: crate::types::I18nTranslatedEnvelope = serde_json::from_value(json!({
        "payload_type":"i18n_language_pack","idempotency_key":"owned-original-manual-i18n",
        "route_secret":"owned","persisted_at":1,
        "payload":{
            "business_line":"plugin_i18n","relation_id":8,"client_task_id":"owned-original-manual-i18n",
            "worker_id":"owned","source_lang":"en","target_lang":"zh_CN",
            "entries":[{"entry_id":80,"msgstr":"Owned original result"}]
        }
    }))
    .unwrap();
    let envelope = serde_json::to_value(envelope).unwrap();
    let source = json!({
        "relation_id":8,"business_line":"plugin_i18n","__manual_i18n_envelope":envelope,
        "entries":[{"entry_id":80,"msgstr":"Owned original result",
                    "source":{"object_id":8,"entry_id":80,"msgid":"Owned frozen source",
                              "text_domain":"owned-pack"}}]
    });
    let attempt = scope(&*db.lock().await, &db, &lease, &item, &source, REQUEST).unwrap();
    let file = root.join("owned-candidate.json");
    let encoded = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &envelope,
        file.to_str().unwrap(),
    )
    .await
    .unwrap();
    crate::task_engine::pipeline::install_json_snapshot(&file, &envelope, &encoded).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), encoded);
    {
        let conn = db.lock().await;
        assert_eq!(
            pending_request(&conn, item.id).unwrap().as_deref(),
            Some(REQUEST)
        );
        assert_eq!(current(&conn, item.id).unwrap().as_deref(), Some(REQUEST));
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }
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
    let recovered = recover_complete_file(&db, &lease, item.id, REQUEST).await;
    let error = recovered.as_ref().err().map(ToString::to_string);
    assert!(
        matches!(&recovered, Ok(Some(_))),
        "complete immutable manual i18n candidate must close original receipt with prior DB credit: root={} error={error:?}",
        root.display()
    );
    assert_eq!(std::fs::read(&file).unwrap(), encoded);
    assert!(root.join("owned.sqlite-wal").metadata().unwrap().len() > 0);
    let conn = db.lock().await;
    assert_eq!(current(&conn, item.id).unwrap().as_deref(), Some(REQUEST));
    assert!(pending_request(&conn, item.id).unwrap().is_none());
    let receipt = replay(&conn, item.id, REQUEST).unwrap().unwrap();
    assert_eq!(receipt.translated_path, file.to_str().unwrap());
    assert_eq!(receipt.sha256, recovered.unwrap().unwrap().sha256);
    let saved = crate::db::jobs::get_item_checked(&conn, item.id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "pending_review");
    assert_eq!(saved.client_task_id, "owned-original-manual-i18n");
}

async fn manual_i18n_recovery_boundary(changed: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let item = {
        let conn = db.lock().await;
        let item = fixture(&conn);
        conn.execute(
            "UPDATE translation_items SET object_type='language_pack',
             business_line='plugin_i18n',wp_object_subtype='plugin' WHERE id=?1",
            [item.id],
        )
        .unwrap();
        crate::db::jobs::get_item_checked(&conn, item.id)
            .unwrap()
            .unwrap()
    };
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let envelope: crate::types::I18nTranslatedEnvelope = serde_json::from_value(json!({
        "payload_type":"i18n_language_pack","idempotency_key":"owned-manual-i18n-boundary",
        "route_secret":"owned","persisted_at":1,
        "payload":{"business_line":"plugin_i18n","relation_id":8,
            "client_task_id":"owned-manual-i18n-boundary","worker_id":"owned",
            "source_lang":"en","target_lang":"zh_CN",
            "entries":[{"entry_id":80,"msgstr":"Owned result"}]},
    }))
    .unwrap();
    let envelope = serde_json::to_value(envelope).unwrap();
    let attempt = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({
            "relation_id":8,"business_line":"plugin_i18n","__manual_i18n_envelope":envelope,
        }),
        REQUEST,
    )
    .unwrap();
    let path = root.path().join("owned-candidate.json");
    let encoded = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &envelope,
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    crate::task_engine::pipeline::install_json_snapshot(&path, &envelope, &encoded).unwrap();
    let receipt = if changed == "complete" {
        Some(
            recover_complete_file(&db, &lease, item.id, REQUEST)
                .await
                .unwrap()
                .unwrap(),
        )
    } else {
        None
    };
    match changed {
        "file" => crate::bindings::save_encrypted_file(&path, &envelope.to_string()).unwrap(),
        "missing" => std::fs::remove_file(&path).unwrap(),
        "owner" => {
            crate::db::system::set_encrypted_config(
                &*db.lock().await,
                &format!("review-execution-claim-v1:{}", item.id),
                &json!({"format":"review-execution-claim-v1","item_id":item.id,
                    "owner":uuid::Uuid::new_v4().to_string()})
                .to_string(),
            )
            .unwrap();
        }
        "unknown" | "busy" | "complete" => {}
        _ => unreachable!(),
    }
    let before = {
        let conn = db.lock().await;
        let attempt =
            crate::db::system::get_system_config_checked(&conn, &key(item.id, REQUEST)).unwrap();
        let head = crate::db::system::get_system_config_checked(
            &conn,
            &format!("review-head-v2:{}", item.id),
        )
        .unwrap();
        let saved = crate::db::jobs::get_item_checked(&conn, item.id)
            .unwrap()
            .unwrap();
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        (attempt, head, serde_json::to_value(saved).unwrap())
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
    let request = if changed == "unknown" {
        "9b3d8f1d-2f35-4d3a-9e0c-d746c9b8a3b1"
    } else {
        REQUEST
    };
    let result = crate::storage_capacity::with_result_credit(
        credit,
        recover_complete_file(&db, &lease, item.id, request),
    )
    .await;
    drop(held);
    if changed == "complete" {
        assert!(result.unwrap() == receipt);
    } else if matches!(changed, "unknown" | "missing") {
        assert!(matches!(result, Ok(None)));
    } else {
        assert!(
            result.is_err(),
            "changed manual i18n candidate cannot borrow credit: {changed}"
        );
    }
    assert_eq!(std::fs::read(&path).ok(), bytes);
    let conn = db.lock().await;
    let attempt =
        crate::db::system::get_system_config_checked(&conn, &key(item.id, REQUEST)).unwrap();
    let head =
        crate::db::system::get_system_config_checked(&conn, &format!("review-head-v2:{}", item.id))
            .unwrap();
    let saved = crate::db::jobs::get_item_checked(&conn, item.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (attempt, head, serde_json::to_value(saved).unwrap()),
        before
    );
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
async fn physical_sqlite_manual_i18n_changed_unknown_missing_or_busy_candidate_cannot_borrow_credit(
) {
    for changed in ["file", "missing", "owner", "unknown", "busy"] {
        manual_i18n_recovery_boundary(changed).await;
    }
}

#[tokio::test]
async fn physical_sqlite_manual_i18n_completed_receipt_at_quota_is_read_only() {
    manual_i18n_recovery_boundary("complete").await;
}
