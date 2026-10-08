use super::*;

async fn seed(db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>, root: &std::path::Path) -> i64 {
    let conn = db.lock().await;
    let job = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: "https://owned.invalid".into(),
            relation_id: 8,
            business_line: "post_content".into(),
            triggered_by: "manual".into(),
        },
    )
    .unwrap();
    crate::db::jobs::create_item(
        &conn,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: "https://owned.invalid".into(),
            relation_id: 8,
            business_line: "post_content".into(),
            object_type: "post_type".into(),
            wp_object_id: 80,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            component_id: "owned".into(),
            component_ids: vec!["owned".into()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: root.join("raw.json").display().to_string(),
            client_task_id: "owned".into(),
            max_retries: 3,
        },
    )
    .unwrap()
}

fn payload() -> TranslationCallbackPayload {
    serde_json::from_value(json!({
        "schema_version": crate::config::TASK_CALLBACK_SCHEMA_VERSION,
        "attempt_id":"owned", "object_snapshot_hash":"owned", "source_revision":"owned",
        "policy_version":"owned", "field_results":[], "relation_id":8,
        "business_line":"post_content", "object_type":"post_type", "post_type":"post",
        "object_id":80, "translated_fields":{"post_title":"owned-result"}, "translated_meta":{},
        "media_mappings":[], "media_field_sources":{}, "client_task_id":"owned", "worker_id":"owned",
        "source_lang":"en", "target_lang":"zh", "execution_time_ms":1
    })).unwrap()
}

#[tokio::test]
async fn remaining_persistence_raw_db_failure_is_not_success() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_raw BEFORE UPDATE ON translation_items
         WHEN NEW.status='fetched' BEGIN SELECT RAISE(ABORT,'owned raw write refusal'); END;",
        )
        .unwrap();
    let error = persist_raw_content(
        &db,
        id,
        &json!({"post_title":"owned"}),
        root.path().join("raw.json").to_str().unwrap(),
        "/dev/null",
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("owned raw write refusal"));
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
}

#[tokio::test]
async fn remaining_persistence_translated_db_failure_is_atomic() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_result BEFORE UPDATE ON translation_items
         WHEN NEW.status='translated' BEGIN SELECT RAISE(ABORT,'owned result refusal'); END;",
        )
        .unwrap();
    let result = persist_translated(
        &db,
        id,
        &payload(),
        "owned",
        Some("owned-secret"),
        root.path().join("result.json").to_str().unwrap(),
        "/dev/null",
    )
    .await;
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert!(format!("{:#}", result.unwrap_err()).contains("owned result refusal"));
    assert_eq!(item.status, "pending");
    assert!(
        item.translated_path.is_empty(),
        "failed projection must roll back its path"
    );
}

#[tokio::test]
async fn remaining_persistence_competing_writer_preserves_review_result() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    let path = root.path().join("result.json");
    std::fs::write(&path, b"owned-existing-evidence").unwrap();
    let _owner = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    assert!(persist_translated(
        &db,
        id,
        &payload(),
        "owned",
        None,
        path.to_str().unwrap(),
        "/dev/null"
    )
    .await
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"owned-existing-evidence");
}

#[tokio::test]
async fn remaining_persistence_done_result_cannot_be_overwritten_by_stale_writer() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    crate::db::jobs::update_item_status(&*db.lock().await, id, "done", None).unwrap();
    let path = root.path().join("result.json");
    std::fs::write(&path, b"owned-completed-evidence").unwrap();
    assert!(persist_translated(
        &db,
        id,
        &payload(),
        "owned",
        None,
        path.to_str().unwrap(),
        "/dev/null"
    )
    .await
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"owned-completed-evidence");
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .status,
        "done"
    );
}

#[tokio::test]
async fn remaining_review_stale_owner_cannot_mutate_item() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = ensure_db(None);
    let root = tempfile::tempdir().unwrap();
    let id = seed(&db, root.path()).await;
    let owner = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    db.lock()
        .await
        .execute(
            "INSERT OR REPLACE INTO system_config(key,value) VALUES (?1,'owned-superseded')",
            [format!("review-execution-claim-v1:{id}")],
        )
        .unwrap();
    assert!(owner.assert_item(&*db.lock().await, &db, id).is_err());
}

#[tokio::test]
async fn remaining_fetch_projection_failure_returns_error_and_retains_original_state() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    db.lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_fetch BEFORE UPDATE ON translation_items
        WHEN NEW.status='fetched' BEGIN SELECT RAISE(ABORT,'owned fetch refusal'); END;",
        )
        .unwrap();
    let result = fetch_item_content(
        db.clone(),
        &Client::new(),
        "/dev/null",
        &item,
        &json!({"post_title":"owned"}),
    )
    .await;
    assert!(format!("{:#}", result.unwrap_err()).contains("owned fetch refusal"));
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
}

#[tokio::test]
async fn remaining_fetch_competitor_cannot_replace_existing_raw_evidence() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    std::fs::write(&item.raw_path, b"owned retained source").unwrap();
    let _owner = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    assert!(fetch_item_content(
        db.clone(),
        &Client::new(),
        "/dev/null",
        &item,
        &json!({"post_title":"changed"})
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read(&item.raw_path).unwrap(),
        b"owned retained source"
    );
}

#[tokio::test]
async fn remaining_persistence_owner_change_during_projection_rolls_back() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = ensure_db(None);
    let id = seed(&db, root.path()).await;
    db.lock()
        .await
        .execute_batch(&format!(
            "CREATE TRIGGER supersede_owner AFTER UPDATE OF translated_path
        ON translation_items BEGIN UPDATE system_config SET value='owned superseded'
        WHERE key='review-execution-claim-v1:{id}'; END;"
        ))
        .unwrap();
    let result = persist_translated(
        &db,
        id,
        &payload(),
        "owned",
        None,
        root.path().join("result.json").to_str().unwrap(),
        "/dev/null",
    )
    .await;
    assert!(format!("{:#}", result.unwrap_err()).contains("owner changed"));
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert!(item.translated_path.is_empty());
    assert_eq!(item.status, "pending");
}

#[tokio::test]
async fn remaining_manual_distinct_requests_bind_distinct_callback_attempts() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = ensure_db(None);
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id":8, "name":"owned", "source_site_id":1, "target_site_id":2,
        "source_lang":"en", "target_lang":"zh", "target_site_type":"virtual",
        "sync_mode":"bidirectional", "template":"owned", "models":[]
    }))
    .unwrap();
    let item = ContentItem {
        object_type: "post_type".into(),
        subtype: "attachment".into(),
        object_id: 80,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "attachment_url":"https://owned.invalid/owned.png",
            "__wptsall_job_snapshot":{"source_revision":"owned-revision","policy_version":"owned-policy"}
        }),
    };
    let worker = test_worker_config();
    let mut identities = Vec::new();
    for request in [
        "c1b05b00-4af3-4a5f-9d88-13b6dc674f11",
        "ec30126b-97fa-4a98-8f3b-032e284f7996",
    ] {
        let scope = crate::db::async_jobs::AsyncJobScope {
            db: db.clone(),
            domain: "https://owned.invalid".into(),
            relation_id: 8,
            object_type: "post_type".into(),
            object_id: 80,
            source_snapshot: Some(json!({"manual_generation":request,"binding":"owned"})),
        };
        let trace = translate_item_fields_with_trace_using_proxy(
            &Client::new(),
            None,
            "https://owned.invalid",
            &item,
            &relation,
            &[],
            None,
            "",
            &[],
            None,
            None,
            &worker,
            "/dev/null",
            None,
            Some(scope),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!trace.payload.media_mappings.is_empty());
        identities.push((trace.idempotency_key, trace.payload.attempt_id));
    }
    assert_ne!(
        identities[0].0, identities[1].0,
        "new explicitly authorized retranslation must not reuse callback idempotency"
    );
    assert_ne!(
        identities[0].1, identities[1].1,
        "callback attempt must identify the manual request"
    );
}
