use super::*;

#[tokio::test]
async fn physical_sqlite_manual_existing_candidate_projects_original_uuid_at_quota() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-manual-candidate-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let attempt = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"post_title":"owned frozen source"}),
        REQUEST,
    )
    .unwrap();
    let envelope = json!({
        "idempotency_key":"owned-original-manual-candidate",
        "payload":{
            "relation_id":8,"business_line":"ats","object_type":"post_type","post_type":"post",
            "object_id":80,"translated_fields":{"post_title":"owned original result"},
            "translated_meta":{},"media_mappings":[],"client_task_id":"owned-original-manual-candidate",
            "worker_id":"owned","source_lang":"en","target_lang":"zh_CN",
            "source_revision":"owned-r1","execution_time_ms":1
        }
    });
    let payload: crate::types::TranslationCallbackPayload =
        serde_json::from_value(envelope["payload"].clone()).unwrap();
    let envelope = json!({
        "idempotency_key":"owned-original-manual-candidate",
        "payload":payload,
    });
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
        "complete immutable manual candidate must close original receipt with prior DB credit: root={} error={error:?}",
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
    assert_eq!(saved.client_task_id, "owned-original-manual-candidate");
}

async fn manual_recovery_boundary(changed: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let attempt = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"post_title":"owned frozen source"}),
        REQUEST,
    )
    .unwrap();
    let payload: crate::types::TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":8,"business_line":"ats","object_type":"post_type","post_type":"post",
        "object_id":80,"translated_fields":{"post_title":"owned original result"},
        "translated_meta":{},"media_mappings":[],"client_task_id":"owned-manual-boundary",
        "worker_id":"owned","source_lang":"en","target_lang":"zh_CN",
        "source_revision":"owned-r1","execution_time_ms":1
    }))
    .unwrap();
    let envelope = json!({"idempotency_key":"owned-manual-boundary","payload":payload});
    let file = root.path().join("owned-candidate.json");
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
    let already_completed = if changed == "complete" {
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
        "file" => crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap(),
        "missing" => std::fs::remove_file(&file).unwrap(),
        "owner" => {
            let conn = db.lock().await;
            let key = format!("review-execution-claim-v1:{}", item.id);
            super::super::super::system::set_encrypted_config(
                &conn,
                &key,
                &json!({
                    "format":"review-execution-claim-v1","item_id":item.id,
                    "owner":uuid::Uuid::new_v4().to_string(),
                })
                .to_string(),
            )
            .unwrap();
        }
        "unknown" | "complete" | "busy" => {}
        _ => unreachable!(),
    }
    let (original_attempt, original_head, original_item) = {
        let conn = db.lock().await;
        let attempt =
            super::super::super::system::get_system_config_checked(&conn, &key(item.id, REQUEST))
                .unwrap()
                .unwrap();
        let head = super::super::super::system::get_system_config_checked(
            &conn,
            &format!("review-head-v2:{}", item.id),
        )
        .unwrap()
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
    let bytes = std::fs::read(&file).ok();
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
    let recovered = crate::storage_capacity::with_result_credit(
        credit,
        recover_complete_file(&db, &lease, item.id, request),
    )
    .await;
    drop(held);
    if changed == "complete" {
        assert!(
            matches!(&recovered, Ok(Some(receipt)) if Some(receipt) == already_completed.as_ref()),
            "completed receipt must replay without writes"
        );
    } else if changed == "unknown" {
        assert!(
            matches!(recovered, Ok(None)),
            "unknown UUID is not a new request"
        );
    } else {
        assert!(
            recovered.is_err(),
            "{changed}: original evidence must be retained"
        );
    }
    let conn = db.lock().await;
    assert_eq!(
        super::super::super::system::get_system_config_checked(&conn, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        original_attempt
    );
    assert_eq!(
        super::super::super::system::get_system_config_checked(
            &conn,
            &format!("review-head-v2:{}", item.id)
        )
        .unwrap()
        .unwrap(),
        original_head
    );
    assert_eq!(
        serde_json::to_value(
            crate::db::jobs::get_item_checked(&conn, item.id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        original_item
    );
    assert_eq!(std::fs::read(&file).ok(), bytes);
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn physical_sqlite_manual_receipt_replay_at_quota_is_read_only() {
    manual_recovery_boundary("complete").await;
}

#[tokio::test]
async fn physical_sqlite_manual_unknown_or_changed_candidate_cannot_borrow_credit() {
    for changed in ["unknown", "file", "missing", "owner", "busy"] {
        manual_recovery_boundary(changed).await;
    }
}

#[tokio::test]
async fn physical_sqlite_manual_fresh_request_cannot_borrow_ambient_db_credit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let before_item = {
        let conn = db.lock().await;
        assert!(current(&conn, item.id).unwrap().is_none());
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        serde_json::to_value(&item).unwrap()
    };
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    let credit =
        crate::storage_capacity::database_recovery_credit(&root.path().join("owned.sqlite"), false)
            .unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let created = crate::storage_capacity::with_result_credit(credit, async {
        scope(
            &*db.lock().await,
            &db,
            &lease,
            &item,
            &json!({"post_title":"owned fresh source"}),
            REQUEST,
        )
    })
    .await;
    assert!(
        created.is_err(),
        "fresh manual request must not consume the original DB recovery booking"
    );
    let conn = db.lock().await;
    assert!(current(&conn, item.id).unwrap().is_none());
    assert!(load(&conn, item.id, REQUEST).unwrap().is_none());
    assert_eq!(
        serde_json::to_value(
            crate::db::jobs::get_item_checked(&conn, item.id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        before_item
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
}
