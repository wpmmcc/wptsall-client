//! Physical receipt generation survives the candidate/file/projection windows.
use super::*;

fn owned_json_payload() -> crate::types::TranslationCallbackPayload {
    serde_json::from_value(json!({
        "relation_id":8,"business_line":"ats","object_type":"post_type","post_type":"post",
        "object_id":80,"translated_fields":{"post_title":"owned private manual result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"owned-json-manual","worker_id":"owned",
        "source_lang":"en","target_lang":"zh_CN","execution_time_ms":1,"source_revision":"owned",
    }))
    .unwrap()
}

fn owned_json_envelope() -> Value {
    json!({"idempotency_key":"owned-json-manual","payload":owned_json_payload()})
}

async fn owned_json_scope(
    db: &Arc<tokio::sync::Mutex<Connection>>,
) -> (
    TranslationItem,
    UnitLease,
    crate::db::async_jobs::AsyncJobScope,
) {
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(db, item.id).await.unwrap();
    let attempt = scope(
        &*db.lock().await,
        db,
        &lease,
        &item,
        &json!({"post_title":"owned frozen source"}),
        REQUEST,
    )
    .unwrap();
    (item, lease, attempt)
}

#[tokio::test]
async fn encrypted_json_manual_prepared_ciphertext_has_one_immutable_generation() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("candidate.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let value = owned_json_envelope();
    let first = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &value,
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    assert!(first.starts_with(b"WPTC"));
    assert!(
        !path.exists(),
        "candidate freezes before publishing the file"
    );
    let authority =
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap();
    let second = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &value,
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        second, first,
        "nonce must not change the frozen physical SHA"
    );
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        authority,
    );
    let mut changed = value.clone();
    changed["payload"]["translated_fields"]["post_title"] = json!("changed result");
    assert!(prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &changed,
        path.to_str().unwrap()
    )
    .await
    .is_err());
    assert!(prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &value,
        root.path().join("other.json").to_str().unwrap()
    )
    .await
    .is_err());
    assert!(!path.exists());
    assert_eq!(
        pending_request(&*db.lock().await, item.id)
            .unwrap()
            .as_deref(),
        Some(REQUEST)
    );
}

#[tokio::test]
async fn encrypted_json_manual_candidate_refusal_never_publishes_unbound_result() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("refused.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let authority =
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap();
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_json_candidate BEFORE UPDATE ON system_config
         WHEN OLD.key LIKE 'review-request-v2:%' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    assert!(prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &owned_json_envelope(),
        path.to_str().unwrap()
    )
    .await
    .is_err());
    assert!(!path.exists());
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        authority,
    );
    assert!(load(&*db.lock().await, item.id, REQUEST)
        .unwrap()
        .unwrap()
        .1
        .candidate
        .is_none());
}

#[tokio::test]
async fn encrypted_json_manual_candidate_only_sqlite_reopen_recovers_exact_bytes_and_uuid() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("owned.sqlite");
    let path = root.path().join("candidate.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(db_path.to_str().unwrap()).unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let first = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &owned_json_envelope(),
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    assert!(!path.exists());
    drop(attempt);
    drop(lease);
    drop(db);
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(db_path.to_str().unwrap()).unwrap(),
    ));
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let recovered = recover_complete_file(&db, &lease, item.id, REQUEST)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.translated_path, path.to_string_lossy());
    assert_eq!(std::fs::read(&path).unwrap(), first);
    let conn = db.lock().await;
    let restored = crate::db::jobs::get_item_checked(&conn, item.id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.status, "pending_review");
    assert_eq!(restored.client_task_id, "owned-json-manual");
    assert_eq!(current(&conn, item.id).unwrap().as_deref(), Some(REQUEST));
    assert!(pending_request(&conn, item.id).unwrap().is_none());
    assert_eq!(
        replay(&conn, item.id, REQUEST).unwrap().unwrap().sha256,
        recovered.sha256
    );
}

#[tokio::test]
async fn encrypted_json_manual_damaged_ciphertext_is_retained_without_projection() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("damaged.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let mut encoded = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &owned_json_envelope(),
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    *encoded.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &encoded).unwrap();
    assert!(recover_complete_file(&db, &lease, item.id, REQUEST)
        .await
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), encoded);
    let conn = db.lock().await;
    assert!(load(&conn, item.id, REQUEST)
        .unwrap()
        .unwrap()
        .1
        .result
        .is_none());
    assert_eq!(
        pending_request(&conn, item.id).unwrap().as_deref(),
        Some(REQUEST)
    );
    assert_eq!(
        crate::db::jobs::get_item_checked(&conn, item.id)
            .unwrap()
            .unwrap()
            .translated_path,
        item.translated_path
    );
}

#[tokio::test]
async fn encrypted_json_manual_legacy_candidate_and_complete_file_remain_physical_plaintext() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let value = owned_json_envelope();
    let before = serde_json::to_vec_pretty(&value).unwrap();
    std::fs::write(&path, &before).unwrap();
    let prepared = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &value,
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(prepared, before);
    let legacy = crate::db::system::decrypt_config_value(
        &crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(!serde_json::from_str::<Value>(&legacy)
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("encoded_result"));
    recover_complete_file(&db, &lease, item.id, REQUEST)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(replay(&*db.lock().await, item.id, REQUEST)
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn encrypted_json_manual_candidate_bad_readback_rolls_back_before_file_publication() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("unconfirmed.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let before =
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap();
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER alter_json_candidate AFTER UPDATE ON system_config
         WHEN NEW.key LIKE 'review-request-v2:%' BEGIN UPDATE system_config
         SET value='owned-damaged-manual-readback' WHERE key=NEW.key; END;",
        )
        .unwrap();
    assert!(
        prepare_result(
            &db,
            &lease,
            &attempt,
            item.id,
            &owned_json_envelope(),
            path.to_str().unwrap()
        )
        .await
        .is_err(),
        "candidate requires exact authority readback, not only an affected row"
    );
    assert!(!path.exists());
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn encrypted_json_manual_receipt_bad_readback_retains_orphan_and_original_pointer() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("complete-orphan.json");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let (item, lease, attempt) = owned_json_scope(&db).await;
    let encoded = prepare_result(
        &db,
        &lease,
        &attempt,
        item.id,
        &owned_json_envelope(),
        path.to_str().unwrap(),
    )
    .await
    .unwrap();
    let before =
        crate::db::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap();
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER alter_json_receipt AFTER UPDATE ON system_config
         WHEN NEW.key LIKE 'review-request-v2:%' BEGIN UPDATE system_config
         SET value='owned-damaged-receipt-readback' WHERE key=NEW.key; END;",
        )
        .unwrap();
    assert!(
        crate::task_engine::pipeline::persist_translated_claimed(
            &lease,
            &db,
            item.id,
            &owned_json_payload(),
            "owned-json-manual",
            None,
            path.to_str().unwrap(),
            "/dev/null",
            Some(&attempt),
        )
        .await
        .is_err(),
        "unconfirmed receipt cannot project a completed request"
    );
    assert_eq!(std::fs::read(&path).unwrap(), encoded);
    let conn = db.lock().await;
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        before
    );
    let unchanged = crate::db::jobs::get_item_checked(&conn, item.id)
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.translated_path, item.translated_path);
    assert_eq!(unchanged.status, item.status);
    drop(conn);
    db.lock()
        .await
        .execute_batch("DROP TRIGGER alter_json_receipt;")
        .unwrap();
    recover_complete_file(&db, &lease, item.id, REQUEST)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), encoded);
}
