use super::*;
#[path = "../../../../../tests/modules/client-wpplugin/unit/encrypted_json_manual_receipts.rs"]
mod encrypted_json_manual_receipts;
#[path = "../../../../../tests/modules/client-wpplugin/unit/physical_manual_capacity.rs"]
mod physical_manual_capacity;
#[path = "../../../../../tests/modules/client-wpplugin/unit/physical_manual_i18n_capacity.rs"]
mod physical_manual_i18n_capacity;
const REQUEST: &str = "c1b05b00-4af3-4a5f-9d88-13b6dc674f11";

fn fixture(conn: &Connection) -> TranslationItem {
    let job = super::super::jobs::create_job(
        conn,
        &super::super::jobs::CreateJobRequest {
            domain: "https://owned.invalid/tenant".into(),
            relation_id: 8,
            business_line: "ats".into(),
            triggered_by: "manual".into(),
        },
    )
    .unwrap();
    let id = super::super::jobs::create_item(
        conn,
        &super::super::jobs::CreateItemRequest {
            job_id: job,
            domain: "https://owned.invalid/tenant".into(),
            relation_id: 8,
            business_line: "ats".into(),
            object_type: "post_type".into(),
            wp_object_id: 80,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: "en".into(),
            target_lang: "zh_CN".into(),
            component_id: "owned".into(),
            component_ids: vec!["owned".into()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: "".into(),
            client_task_id: "owned-review".into(),
            max_retries: 3,
        },
    )
    .unwrap();
    super::super::jobs::get_item_checked(conn, id)
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn remaining_manual_attempt_language_change_cannot_reuse_unfinished_fee_unit() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let mut item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let raw = json!({"post_title":"owned"});
    scope(&*db.lock().await, &db, &lease, &item, &raw, REQUEST).unwrap();
    item.target_lang = "fr".into();
    assert!(scope(&*db.lock().await, &db, &lease, &item, &raw, REQUEST).is_err());
}

#[tokio::test]
async fn remaining_manual_attempt_cannot_finish_before_durable_result() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let attempt = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"post_title":"owned"}),
        REQUEST,
    )
    .unwrap();
    assert!(finish(&*db.lock().await, &db, &lease, &attempt, item.id).is_err());
}

#[tokio::test]
async fn remaining_manual_attempt_saved_request_replay_is_not_a_new_fee_generation() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let raw = json!({"post_title":"owned"});
    let first = scope(&*db.lock().await, &db, &lease, &item, &raw, REQUEST).unwrap();
    let payload: crate::types::TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":8,"business_line":"ats","object_type":"post_type","post_type":"post",
        "object_id":80,"translated_fields":{"post_title":"owned result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"owned-request","worker_id":"owned",
        "source_lang":"en","target_lang":"zh_CN","execution_time_ms":1,"source_revision":"owned",
    }))
    .unwrap();
    crate::task_engine::pipeline::persist_translated_claimed(
        &lease,
        &db,
        item.id,
        &payload,
        "owned-request",
        None,
        root.path().join("result.json").to_str().unwrap(),
        "/dev/null",
        Some(&first),
    )
    .await
    .unwrap();
    let replay = scope(&*db.lock().await, &db, &lease, &item, &raw, REQUEST).unwrap();
    assert_eq!(
        first.source_snapshot, replay.source_snapshot,
        "a reply lost after durable result save must resume the original paid request"
    );
}

#[tokio::test]
async fn remaining_manual_new_request_refuses_unfinished_original_and_preserves_ciphertext() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let raw = json!({"post_title":"owned"});
    scope(&*db.lock().await, &db, &lease, &item, &raw, REQUEST).unwrap();
    let previous =
        super::super::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap();
    let error = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &raw,
        "ec30126b-97fa-4a98-8f3b-032e284f7996",
    )
    .err()
    .expect("new request must refuse unfinished original");
    assert!(format!("{error:#}").contains("unfinished manual request"));
    assert_eq!(
        super::super::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        previous
    );
    assert_eq!(
        pending_request(&*db.lock().await, item.id)
            .unwrap()
            .as_deref(),
        Some(REQUEST)
    );
}

#[tokio::test]
async fn remaining_manual_head_zero_row_rolls_back_new_request() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_head BEFORE INSERT ON system_config
        WHEN NEW.key LIKE 'review-head-v2:%' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let error = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"post_title":"owned"}),
        REQUEST,
    )
    .err()
    .expect("ignored request head must refuse");
    assert!(format!("{error:#}").contains("head was not committed"));
    assert!(load(&*db.lock().await, item.id, REQUEST).unwrap().is_none());
    assert!(current(&*db.lock().await, item.id).unwrap().is_none());
}

#[tokio::test]
async fn remaining_manual_head_changed_during_insert_rolls_back_before_fee_scope() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    db.lock().await.execute_batch("CREATE TRIGGER change_head AFTER INSERT ON system_config
        WHEN NEW.key LIKE 'review-head-v2:%' BEGIN UPDATE system_config SET value='owned-damaged-head' WHERE key=NEW.key; END;").unwrap();
    assert!(
        scope(
            &*db.lock().await,
            &db,
            &lease,
            &item,
            &json!({"post_title":"owned"}),
            REQUEST
        )
        .is_err(),
        "one affected row is not proof that the saved request head still belongs to this scope"
    );
    assert!(
        load(&*db.lock().await, item.id, REQUEST).unwrap().is_none(),
        "damaged head must roll back request before provider work"
    );
}

#[tokio::test]
async fn remaining_manual_damaged_head_request_and_legacy_fail_closed() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    for bad in [
        "",
        "new",
        "00000000-0000-0000-0000-000000000000",
        "C1B05B00-4AF3-4A5F-9D88-13B6DC674F11",
    ] {
        assert!(scope(&*db.lock().await, &db, &lease, &item, &json!({}), bad).is_err());
    }
    super::super::system::set_system_config(
        &*db.lock().await,
        &format!("review-head-v2:{}", item.id),
        "{damaged",
    )
    .unwrap();
    assert!(scope(&*db.lock().await, &db, &lease, &item, &json!({}), REQUEST).is_err());
    assert_eq!(
        super::super::system::get_system_config_checked(
            &*db.lock().await,
            &format!("review-head-v2:{}", item.id)
        )
        .unwrap()
        .as_deref(),
        Some("{damaged")
    );
    db.lock()
        .await
        .execute(
            "DELETE FROM system_config WHERE key=?1",
            [format!("review-head-v2:{}", item.id)],
        )
        .unwrap();
    super::super::system::set_system_config(
        &*db.lock().await,
        &format!("review-attempt-v1:{}", item.id),
        "{owned legacy evidence",
    )
    .unwrap();
    assert!(scope(&*db.lock().await, &db, &lease, &item, &json!({}), REQUEST).is_err());
}

#[tokio::test]
async fn remaining_manual_completed_request_cannot_rebind_saved_result() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let item = fixture(&*db.lock().await);
    let lease = UnitLease::item(&db, item.id).await.unwrap();
    let scope = scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"post_title":"owned"}),
        REQUEST,
    )
    .unwrap();
    let mut payload: crate::types::TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":8,"business_line":"ats","object_type":"post_type","post_type":"post",
        "object_id":80,"translated_fields":{"post_title":"owned result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"owned-request","worker_id":"owned",
        "source_lang":"en","target_lang":"zh_CN","execution_time_ms":1,"source_revision":"owned",
    }))
    .unwrap();
    let original = root.path().join("result.json");
    crate::task_engine::pipeline::persist_translated_claimed(
        &lease,
        &db,
        item.id,
        &payload,
        "owned-request",
        None,
        original.to_str().unwrap(),
        "/dev/null",
        Some(&scope),
    )
    .await
    .unwrap();
    let previous =
        super::super::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap();
    payload
        .translated_fields
        .insert("post_title".into(), "different result".into());
    assert!(
        crate::task_engine::pipeline::persist_translated_claimed(
            &lease,
            &db,
            item.id,
            &payload,
            "owned-request",
            None,
            root.path().join("changed.json").to_str().unwrap(),
            "/dev/null",
            Some(&scope),
        )
        .await
        .is_err(),
        "a completed request is immutable, not permission to replace its receipt"
    );
    assert_eq!(
        super::super::system::get_system_config_checked(&*db.lock().await, &key(item.id, REQUEST))
            .unwrap()
            .unwrap(),
        previous
    );
    assert_eq!(
        super::super::jobs::get_item_checked(&*db.lock().await, item.id)
            .unwrap()
            .unwrap()
            .translated_path,
        original.to_str().unwrap()
    );
}
