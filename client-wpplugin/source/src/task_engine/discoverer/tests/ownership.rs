use super::*;

async fn original_pending_case(hold_review: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").unwrap()));
    let base = "http://127.0.0.1:9/wp-json/wptsall/v2/owned/client";
    let relation = sample_relation();
    let item = ContentItem {
        object_type: "post_type".into(),
        subtype: "post".into(),
        object_id: 800,
        needs_resync: true,
        mapping_id: None,
        complete_data: json!({"post_title":"owned-source", "__wptsall_job_snapshot":{
            "source_revision":"owned-source-v2", "policy_version":"owned-policy"
        }}),
    };
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":relation.id, "business_line":"post_content", "object_type":"post_type",
        "post_type":"post", "object_id":800, "translated_fields":{"post_title":"original-paid-result"},
        "translated_meta":{}, "media_mappings":[], "client_task_id":"owned-original",
        "worker_id":"owned", "source_lang":relation.source_lang, "target_lang":relation.target_lang,
        "execution_time_ms":1, "source_revision":"owned-source-v1"
    })).unwrap();
    let job = crate::db::jobs::create_job(
        &*db.lock().await,
        &crate::db::jobs::CreateJobRequest {
            domain: base.into(),
            relation_id: relation.id,
            business_line: "post_content".into(),
            triggered_by: "manual".into(),
        },
    )
    .unwrap();
    let id = crate::db::jobs::create_item(
        &*db.lock().await,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: base.into(),
            relation_id: relation.id,
            business_line: "post_content".into(),
            object_type: "post_type".into(),
            wp_object_id: 800,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: relation.source_lang.clone(),
            target_lang: relation.target_lang.clone(),
            component_id: "owned".into(),
            component_ids: vec!["owned".into()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: "".into(),
            client_task_id: "owned-original".into(),
            max_retries: 3,
        },
    )
    .unwrap();
    crate::db::jobs::update_item_status(&*db.lock().await, id, "pending_review", None).unwrap();
    if !hold_review {
        add_pending_callback(
            &*db.lock().await,
            &PendingCallbackEntry {
                api_base_url: base.into(),
                idempotency_key: "owned-original".into(),
                payload,
                route_secret: Some("owned".into()),
                created_at: 1,
                retry_count: 40,
                last_retry_at: 1,
                relation_id: relation.id,
                object_id: 800,
                object_type: "post_type".into(),
            },
        )
        .unwrap();
    }
    let _owner = if hold_review {
        Some(
            crate::db::unit_lock::UnitLease::item(&db, id)
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let pending = Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap()));
    let result = translate_content_item_owned(
        Client::builder().no_proxy().build().unwrap(),
        base.into(),
        "owned".into(),
        "/dev/null".into(),
        item,
        relation.clone(),
        Arc::new(vec![]),
        None,
        None,
        "".into(),
        vec![],
        None,
        None,
        discovery_test_worker_config(),
        Some("owned".into()),
        pending,
        Some(db.clone()),
        Arc::new(Semaphore::new(1)),
        None,
        None,
        Arc::new(AdaptiveRateControl::new(false, 0)),
        Some(job),
        DiscoveryTaskParams::default(),
    )
    .await;
    assert!(
        result.is_err(),
        "automatic resync cannot bypass retained review/delivery authority"
    );
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .status,
        "pending_review"
    );
    if !hold_review {
        assert_eq!(
            find_pending_callback(&*db.lock().await, base, relation.id, "post_type", 800)
                .unwrap()
                .unwrap()
                .idempotency_key,
            "owned-original"
        );
    }
}

#[tokio::test]
async fn remaining_automatic_resync_cannot_bypass_live_review_owner() {
    original_pending_case(true).await;
}

#[tokio::test]
async fn remaining_automatic_source_change_does_not_erase_unconfirmed_delivery() {
    original_pending_case(false).await;
}

async fn automatic_pending_projection_case(fault: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").unwrap()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (ready, unblock) = (entered.clone(), release.clone());
    let refusal = fault == "retry-refusal";
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        let _ = socket.read(&mut request).await.unwrap();
        ready.notify_one();
        unblock.notified().await;
        let (status, body) = if refusal {
            (
                "503 Service Unavailable",
                r#"{"code":"owned_unavailable","message":"owned unavailable"}"#,
            )
        } else {
            (
                "200 OK",
                r#"{"success":true,"result_id":81,"protocol":"v2","result_status":"synced","sync_result":{"success":true}}"#,
            )
        };
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        let _ = socket.write_all(response.as_bytes()).await;
    });
    let relation = sample_relation();
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":relation.id,"business_line":"post_content","object_type":"post_type",
        "post_type":"post","object_id":800,"translated_fields":{"post_title":"original-paid-result"},
        "translated_meta":{},"media_mappings":[],"client_task_id":"owned-original","worker_id":"owned",
        "source_lang":relation.source_lang,"target_lang":relation.target_lang,
        "execution_time_ms":1,"source_revision":"owned-revision"
    })).unwrap();
    let job = crate::db::jobs::create_job(
        &*db.lock().await,
        &crate::db::jobs::CreateJobRequest {
            domain: base.clone(),
            relation_id: relation.id,
            business_line: "post_content".into(),
            triggered_by: "manual".into(),
        },
    )
    .unwrap();
    let path = root.path().join("owned-paid-result.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"idempotency_key":"owned-original","payload":payload})).unwrap(),
    )
    .unwrap();
    let id = crate::db::jobs::record_item_outcome(
        &*db.lock().await,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: base.clone(),
            relation_id: relation.id,
            business_line: "post_content".into(),
            object_type: "post_type".into(),
            wp_object_id: 800,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: relation.source_lang.clone(),
            target_lang: relation.target_lang.clone(),
            component_id: "owned".into(),
            component_ids: vec!["owned".into()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: "".into(),
            client_task_id: "owned-original".into(),
            max_retries: 3,
        },
        path.to_str().unwrap(),
        "translated",
        None,
    )
    .unwrap();
    add_pending_callback(
        &*db.lock().await,
        &PendingCallbackEntry {
            api_base_url: base.clone(),
            idempotency_key: "owned-original".into(),
            payload,
            route_secret: Some("owned".into()),
            created_at: 1,
            retry_count: 0,
            last_retry_at: 0,
            relation_id: relation.id,
            object_id: 800,
            object_type: "post_type".into(),
        },
    )
    .unwrap();
    match fault {
        "delete-refusal" => db.lock().await.execute_batch("CREATE TRIGGER owned_refuse_delete BEFORE DELETE ON pending_callbacks BEGIN SELECT RAISE(IGNORE); END;").unwrap(),
        "retry-refusal" => db.lock().await.execute_batch("CREATE TRIGGER owned_refuse_retry BEFORE UPDATE ON pending_callbacks BEGIN SELECT RAISE(IGNORE); END;").unwrap(),
        "terminal-refusal" => db.lock().await.execute_batch("CREATE TRIGGER owned_refuse_terminal BEFORE UPDATE OF status ON translation_items WHEN NEW.status='done' BEGIN SELECT RAISE(IGNORE); END;").unwrap(),
        _ => {},
    }
    let state = db.clone();
    let domain = base.clone();
    let rel = relation.clone();
    let work = tokio::spawn(async move {
        translate_content_item_owned(
            Client::builder().no_proxy().build().unwrap(),
            domain,
            "owned".into(),
            "/dev/null".into(),
            ContentItem {
                object_type: "post_type".into(),
                subtype: "post".into(),
                object_id: 800,
                needs_resync: false,
                mapping_id: None,
                complete_data: json!({"post_title":"owned source"}),
            },
            rel,
            Arc::new(vec![]),
            None,
            None,
            "".into(),
            vec![],
            None,
            None,
            discovery_test_worker_config(),
            Some("owned".into()),
            Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap())),
            Some(state),
            Arc::new(Semaphore::new(1)),
            None,
            None,
            Arc::new(AdaptiveRateControl::new(false, 0)),
            Some(job),
            DiscoveryTaskParams::default(),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    if fault == "owner-change" {
        db.lock().await.execute("UPDATE system_config SET value=value || '-owned-change' WHERE key LIKE 'review-execution-claim-v1:%'",[]).unwrap();
    }
    release.notify_one();
    let outcome = work.await.unwrap();
    server.await.unwrap();
    if fault == "success" {
        assert!(outcome.unwrap());
        assert_eq!(
            crate::db::jobs::get_item_checked(&*db.lock().await, id)
                .unwrap()
                .unwrap()
                .status,
            "done",
            "durably applied pending replay must project the original item terminal state"
        );
        assert!(
            find_pending_callback(&*db.lock().await, &base, relation.id, "post_type", 800)
                .unwrap()
                .is_none()
        );
    } else {
        assert!(outcome.is_err(),"automatic callback projection refusal must not report success or a saved retry: {fault}");
        let pending =
            find_pending_callback(&*db.lock().await, &base, relation.id, "post_type", 800)
                .unwrap()
                .unwrap();
        assert_eq!(pending.idempotency_key, "owned-original");
        assert_eq!(pending.retry_count, 0);
        assert_eq!(
            crate::db::jobs::get_item_checked(&*db.lock().await, id)
                .unwrap()
                .unwrap()
                .status,
            "translated"
        );
    }
}

#[tokio::test]
async fn remaining_automatic_pending_delete_refusal_is_not_success() {
    automatic_pending_projection_case("delete-refusal").await;
}

#[tokio::test]
async fn remaining_automatic_pending_retry_refusal_is_not_a_saved_retry() {
    automatic_pending_projection_case("retry-refusal").await;
}

#[tokio::test]
async fn remaining_automatic_pending_receipt_projects_original_item_terminal_state() {
    automatic_pending_projection_case("success").await;
}

#[tokio::test]
async fn remaining_automatic_pending_terminal_projection_refusal_retains_original_callback() {
    automatic_pending_projection_case("terminal-refusal").await;
}

#[tokio::test]
async fn remaining_automatic_pending_owner_change_retains_original_callback() {
    automatic_pending_projection_case("owner-change").await;
}

async fn run_store_only_pending(
    store: Arc<Mutex<PendingCallbackStore>>,
    base: String,
) -> anyhow::Result<bool> {
    run_store_only_pending_with_resync(store, base, false).await
}

async fn run_store_only_pending_with_resync(
    store: Arc<Mutex<PendingCallbackStore>>,
    base: String,
    needs_resync: bool,
) -> anyhow::Result<bool> {
    translate_content_item_owned(
        Client::builder().no_proxy().build().unwrap(),
        base,
        "owned".into(),
        "/dev/null".into(),
        ContentItem {
            object_type: "post_type".into(),
            subtype: "post".into(),
            object_id: 800,
            needs_resync,
            mapping_id: None,
            complete_data: json!({"post_title":"owned store source"}),
        },
        sample_relation(),
        Arc::new(vec![]),
        None,
        None,
        "".into(),
        vec![],
        None,
        None,
        discovery_test_worker_config(),
        Some("owned".into()),
        store,
        None,
        Arc::new(Semaphore::new(1)),
        None,
        None,
        Arc::new(AdaptiveRateControl::new(false, 0)),
        None,
        DiscoveryTaskParams::default(),
    )
    .await
}

async fn store_only_callback_case(competing: bool, resync: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let path = root.path().join("pending.db").display().to_string();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, wait) = tokio::sync::watch::channel(false);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (ready, count) = (entered.clone(), calls.clone());
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (ready, count, mut wait) = (ready.clone(), count.clone(), wait.clone());
            handlers.spawn(async move {
                let mut raw = Vec::new();
                let mut buffer = [0;8192];
                let end = loop {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buffer[..n]);
                    if let Some(end) = raw.windows(4).position(|s| s == b"\r\n\r\n") { break end + 4; }
                };
                let headers = std::str::from_utf8(&raw[..end]).unwrap();
                assert!(headers.lines().next().unwrap().contains("/translation-callback"));
                let length = headers.lines().find_map(|line| {
                    let (key,value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                }).unwrap();
                while raw.len() < end + length {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    raw.extend_from_slice(&buffer[..n]);
                }
                let body: serde_json::Value = serde_json::from_slice(&raw[end..end+length]).unwrap();
                assert_eq!(body["client_task_id"],"owned-store-original");
                count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                ready.notify_one();
                while !*wait.borrow() { wait.changed().await.unwrap(); }
                let body = r#"{"success":true,"result_id":81,"protocol":"v2","result_status":"synced","sync_result":{"success":true}}"#;
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    let store = Arc::new(Mutex::new(PendingCallbackStore::open(&path).unwrap()));
    let relation = sample_relation();
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":relation.id,"business_line":"post_content","object_type":"post_type","post_type":"post",
        "object_id":800,"translated_fields":{"post_title":"owned paid store result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"owned-store-original","worker_id":"owned",
        "source_lang":relation.source_lang,"target_lang":relation.target_lang,
        "execution_time_ms":1,"source_revision":"owned-store-revision"
    })).unwrap();
    store
        .lock()
        .await
        .add(crate::persistence::PendingCallback {
            api_base_url: base.clone(),
            idempotency_key: "owned-store-original".into(),
            payload,
            route_secret: Some("owned".into()),
            created_at: 1,
            retry_count: 0,
            last_retry_at: 0,
        })
        .unwrap();
    let work = tokio::spawn(run_store_only_pending(store.clone(), base.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let competitor = if competing {
        let other = Arc::new(Mutex::new(PendingCallbackStore::open(&path).unwrap()));
        Some(
            tokio::time::timeout(
                std::time::Duration::from_millis(500),
                run_store_only_pending(other, base.clone()),
            )
            .await,
        )
    } else {
        None
    };
    release.send(true).unwrap();
    assert!(work.await.unwrap().unwrap());
    if let Some(outcome) = competitor {
        assert!(
            matches!(outcome,Ok(Err(ref error)) if error.to_string().contains("REVIEW_ITEM_BUSY")),
            "a second store-only runner must refuse before callback egress: {outcome:?}"
        );
    } else {
        drop(store);
        let reopened = Arc::new(Mutex::new(PendingCallbackStore::open(&path).unwrap()));
        if resync {
            assert!(
                run_store_only_pending_with_resync(reopened.clone(), base.clone(), true)
                    .await
                    .unwrap(),
                "unchanged explicit resync must redeliver the immutable original callback"
            );
        }
        assert!(!run_store_only_pending(reopened,base).await.unwrap(),
            "applied store-only receipt must survive restart as a no-op without invoking source or provider");
    }
    server.abort();
    let _ = server.await;
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        if resync { 2 } else { 1 }
    );
}

#[tokio::test]
async fn remaining_automatic_store_only_receipt_survives_restart() {
    store_only_callback_case(false, false).await;
}

#[tokio::test]
async fn remaining_automatic_store_only_unchanged_resync_replays_original_after_restart() {
    store_only_callback_case(false, true).await;
}

#[tokio::test]
async fn remaining_automatic_store_only_live_owner_excludes_competitor() {
    store_only_callback_case(true, false).await;
}

async fn fresh_callback_projection_case(fault: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let wp = format!("{base}/wp-json/wptsall/v2/owned/client");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0; 8192];
            let end = loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buffer[..n]);
                if let Some(end) = raw.windows(4).position(|s| s == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&raw[..end]).unwrap();
            let first = headers.lines().next().unwrap().to_string();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while raw.len() < end + length {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buffer[..n]);
            }
            let value: serde_json::Value = serde_json::from_slice(&raw[end..end + length]).unwrap();
            let body = if first.starts_with("POST /provider ") {
                assert_eq!(value["text"], "owned fresh source");
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                r#"{"text":"owned paid translated result"}"#
            } else {
                assert!(first.contains("/translation-callback"), "{first}");
                assert_eq!(
                    value["translated_fields"]["post_title"],
                    "owned paid translated result"
                );
                r#"{"success":true,"result_id":81,"protocol":"v2","result_status":"synced","sync_result":{"success":true}}"#
            };
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").unwrap()));
    match fault {
        "history-read-damage" => db.lock().await.execute_batch("ALTER TABLE translation_records RENAME COLUMN status TO owned_damaged_status;").unwrap(),
        "review-read-damage" => db.lock().await.execute_batch("ALTER TABLE translation_items RENAME COLUMN status TO owned_damaged_status;").unwrap(),
        _ => db.lock().await.execute_batch("CREATE TRIGGER owned_refuse_history BEFORE INSERT ON translation_records BEGIN SELECT RAISE(IGNORE); END;").unwrap(),
    }
    let mut registry = sample_registry();
    let runtime = registry.runtimes.get_mut("comp-selected").unwrap();
    runtime.template.request.url = format!("{base}/provider");
    runtime.supported_business_lines = Vec::new();
    runtime.supported_content_formats = vec!["plain_text".into()];
    let mut rule = sample_rule("post", "post");
    rule.translate_fields = vec!["post_title".into()];
    rule.field_content_formats
        .insert("post_title".into(), "plain_text".into());
    rule.field_storage_map
        .insert("post_title".into(), "post_column".into());
    let item = ContentItem {
        object_type: "post_type".into(),
        subtype: "post".into(),
        object_id: 801,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title":"owned fresh source","__wptsall_job_snapshot":{
            "source_revision":"owned-fresh-revision","policy_version":"owned-policy"
        }}),
    };
    let outcome = translate_content_item_owned(
        Client::builder().no_proxy().build().unwrap(),
        wp.clone(),
        "owned".into(),
        "/dev/null".into(),
        item,
        sample_relation(),
        Arc::new(vec![rule]),
        Some(Arc::new(registry)),
        None,
        "comp-selected".into(),
        vec![],
        None,
        None,
        discovery_test_worker_config(),
        Some("owned".into()),
        Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap())),
        Some(db.clone()),
        Arc::new(Semaphore::new(1)),
        None,
        None,
        Arc::new(AdaptiveRateControl::new(false, 0)),
        None,
        DiscoveryTaskParams::default(),
    )
    .await;
    server.abort();
    let _ = server.await;
    if fault.ends_with("read-damage") {
        assert!(
            outcome.is_err(),
            "damaged materialized/review reads cannot mean permission to charge"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "durable read refusal must precede provider egress"
        );
        return;
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the fixture must reach its paid provider boundary"
    );
    assert!(
        outcome.is_err(),
        "ignored history write cannot claim automatic completion"
    );
    assert!(
        find_pending_callback(
            &*db.lock().await,
            &wp,
            sample_relation().id,
            "post_type",
            801
        )
        .unwrap()
        .is_some(),
        "applied callback must remain until local completion is committed"
    );
}

#[tokio::test]
async fn remaining_automatic_fresh_callback_history_refusal_retains_original_paid_delivery() {
    fresh_callback_projection_case("history-write-refusal").await;
}

#[tokio::test]
async fn remaining_automatic_damaged_history_read_cannot_authorize_new_fee() {
    fresh_callback_projection_case("history-read-damage").await;
}

#[tokio::test]
async fn remaining_automatic_damaged_review_read_cannot_authorize_new_fee() {
    fresh_callback_projection_case("review-read-damage").await;
}
