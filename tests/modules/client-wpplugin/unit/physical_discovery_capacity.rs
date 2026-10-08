// Owned Linux physical quota oracle; no live site or provider is contacted.
use super::*;
use rusqlite::OptionalExtension;

#[tokio::test]
async fn physical_sqlite_discovery_original_pending_ack_commits_after_quota_drop() {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::Builder::new()
        .prefix("sqlite-discovery-ack-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/route_test/client",
        listener.local_addr().unwrap()
    );
    let received = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let seen = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let server = {
        let received = received.clone();
        let seen = seen.clone();
        let release = release.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                    assert!(bytes.len() < 64 * 1024);
                }
                received.lock().unwrap().push(bytes);
                seen.notify_one();
                release.notified().await;
                let body = r#"{"success":true,"result_id":123,"queued":false,"sync_task_id":0,"protocol":"v2","result_status":"synced"}"#;
                use base64::Engine;
                use hmac::Mac;
                let key = crate::crypto::derive_signing_key("owned-token");
                let mut mac = <hmac::Hmac<sha2::Sha256> as Mac>::new_from_slice(&key).unwrap();
                mac.update(body.as_bytes());
                let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(mac.finalize().into_bytes());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };
    let relation = sample_relation();
    let source = ContentItem {
        object_type: "post_type".into(),
        subtype: "post".into(),
        object_id: 800,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title":"owned frozen source"}),
    };
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":relation.id,"business_line":"post_content","object_type":"post_type",
        "post_type":"post","object_id":800,"translated_fields":{"post_title":"owned original result"},
        "translated_meta":{},"media_mappings":[],"client_task_id":"owned-discovery-original",
        "worker_id":"owned","source_lang":relation.source_lang,"target_lang":relation.target_lang,
        "execution_time_ms":1,"source_revision":"owned-r1"
    }))
    .unwrap();
    let file = root.join("owned-translated.json");
    crate::bindings::save_encrypted_file(
        &file,
        &json!({"idempotency_key":"owned-discovery-original","payload":payload}).to_string(),
    )
    .unwrap();
    let bytes = std::fs::read(&file).unwrap();
    let (job, item) = {
        let conn = db.lock().await;
        let job = crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: base.clone(),
                relation_id: relation.id,
                business_line: "post_content".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        let item = crate::db::jobs::record_item_outcome(
            &conn,
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
                client_task_id: "owned-discovery-original".into(),
                max_retries: 2,
            },
            file.to_str().unwrap(),
            "translated",
            None,
        )
        .unwrap();
        add_pending_callback(
            &conn,
            &PendingCallbackEntry {
                api_base_url: base.clone(),
                idempotency_key: "owned-discovery-original".into(),
                payload: payload.clone(),
                route_secret: Some("route_test".into()),
                created_at: 1,
                retry_count: 0,
                last_retry_at: 0,
                relation_id: relation.id,
                object_id: 800,
                object_type: "post_type".into(),
            },
        )
        .unwrap();
        (job, item)
    };
    let source_for_receipt = source.clone();
    let work = {
        let db = db.clone();
        let base = base.clone();
        let relation = relation.clone();
        tokio::spawn(async move {
            translate_content_item_owned(
                Client::builder().no_proxy().build().unwrap(),
                base,
                "owned-token".into(),
                "/dev/null".into(),
                source,
                relation,
                Arc::new(vec![]),
                None,
                None,
                "".into(),
                vec![],
                None,
                None,
                discovery_test_worker_config(),
                Some("route_test".into()),
                Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap())),
                Some(db),
                Arc::new(Semaphore::new(1)),
                None,
                None,
                Arc::new(AdaptiveRateControl::new(false, 0)),
                Some(job),
                DiscoveryTaskParams::default(),
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), seen.notified())
        .await
        .unwrap();
    let original_pending = {
        let conn = db.lock().await;
        let pending = find_pending_callback(&conn, &base, relation.id, "post_type", 800)
            .unwrap()
            .unwrap();
        assert_eq!(pending.idempotency_key, "owned-discovery-original");
        let physical: (String, String) = conn
            .query_row(
                "SELECT payload_json,route_secret_enc FROM pending_callbacks WHERE idempotency_key=?1",
                ["owned-discovery-original"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(physical.0.starts_with("V1BUQw") && physical.1.starts_with("V1BUQw"));
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        physical
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
    release.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&result, Ok(true)),
        "original signed discovery ack must close its saved pending/item/receipt with prior DB credit: root={} result={result:?} original_pending_encrypted={}",
        root.display(),
        original_pending.0.starts_with("V1BUQw")
    );
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert!(root.join("owned.sqlite-wal").metadata().unwrap().len() > 0);
    let conn = db.lock().await;
    let saved = crate::db::jobs::get_item_checked(&conn, item)
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "done");
    assert!(saved
        .sync_response_json
        .as_deref()
        .is_some_and(|raw| raw.contains("\"result_id\":123")));
    assert!(
        find_pending_callback(&conn, &base, relation.id, "post_type", 800)
            .unwrap()
            .is_none()
    );
    let receipt = super::super::receipts::completed(&conn, &base, relation.id, &source_for_receipt)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.identity, "owned-discovery-original");
    assert_eq!(
        serde_json::to_value(receipt.payload).unwrap(),
        serde_json::to_value(payload).unwrap()
    );
    assert_eq!(received.lock().unwrap().len(), 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

async fn discovery_ack_boundary(changed: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/route_test/client",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = {
        let seen = seen.clone();
        let release = release.clone();
        let requests = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                    assert!(request.len() < 64 * 1024);
                }
                requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                seen.notify_one();
                release.notified().await;
                let body =
                    r#"{"success":true,"result_id":123,"protocol":"v2","result_status":"synced"}"#;
                use base64::Engine;
                use hmac::Mac;
                let key = crate::crypto::derive_signing_key("owned-token");
                let mut mac = <hmac::Hmac<sha2::Sha256> as Mac>::new_from_slice(&key).unwrap();
                mac.update(body.as_bytes());
                let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(mac.finalize().into_bytes());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };
    let relation = sample_relation();
    let source = ContentItem {
        object_type: "post_type".into(),
        subtype: "post".into(),
        object_id: 800,
        needs_resync: changed == "fresh",
        mapping_id: None,
        complete_data: json!({"post_title":"owned source"}),
    };
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":relation.id,"business_line":"post_content","object_type":"post_type",
        "post_type":"post","object_id":800,"translated_fields":{"post_title":"owned result"},
        "translated_meta":{},"media_mappings":[],"client_task_id":"owned-discovery-boundary",
        "worker_id":"owned","source_lang":relation.source_lang,"target_lang":relation.target_lang,
        "execution_time_ms":1,"source_revision":"owned-r1",
    }))
    .unwrap();
    let file = root.path().join("owned-result.json");
    let envelope = json!({"idempotency_key":"owned-discovery-boundary","payload":payload});
    crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap();
    let (job, item) =
        {
            let conn = db.lock().await;
            let job = crate::db::jobs::create_job(
                &conn,
                &crate::db::jobs::CreateJobRequest {
                    domain: base.clone(),
                    relation_id: relation.id,
                    business_line: "post_content".into(),
                    triggered_by: "auto".into(),
                },
            )
            .unwrap();
            let item = crate::db::jobs::record_item_outcome(
                &conn,
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
                    client_task_id: "owned-discovery-boundary".into(),
                    max_retries: 2,
                },
                file.to_str().unwrap(),
                "translated",
                None,
            )
            .unwrap();
            if changed == "fresh" {
                super::super::receipts::save(
                &conn,&base,"owned-discovery-boundary",&payload,&source,
                &json!({"success":true,"result_id":123,"protocol":"v2","result_status":"synced"}),
            ).unwrap();
            } else {
                add_pending_callback(
                    &conn,
                    &PendingCallbackEntry {
                        api_base_url: base.clone(),
                        idempotency_key: "owned-discovery-boundary".into(),
                        payload: payload.clone(),
                        route_secret: Some("route_test".into()),
                        created_at: 1,
                        retry_count: 0,
                        last_retry_at: 0,
                        relation_id: relation.id,
                        object_id: 800,
                        object_type: "post_type".into(),
                    },
                )
                .unwrap();
            }
            (job, item)
        };
    let lease =
        crate::db::unit_lock::ContentExecution::acquire(&db, &base, relation.id, "post_type", 800)
            .await
            .unwrap();
    if changed == "fresh" {
        let conn = db.lock().await;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        std::fs::write(
            root.path().join(".wptsall-storage-v1.json"),
            br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
        )
        .unwrap();
    }
    let ambient =
        crate::storage_capacity::database_recovery_credit(&root.path().join("owned.sqlite"), false)
            .unwrap();
    let work = {
        let db = db.clone();
        let base = base.clone();
        tokio::spawn(async move {
            crate::storage_capacity::with_result_credit(
                ambient,
                translate_content_item(
                    &Client::builder().no_proxy().build().unwrap(),
                    &base,
                    "owned-token",
                    "/dev/null",
                    &source,
                    &relation,
                    &[],
                    None,
                    None,
                    "",
                    &[],
                    None,
                    None,
                    &discovery_test_worker_config(),
                    Some("route_test"),
                    &Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap())),
                    Some(db),
                    &Arc::new(Semaphore::new(1)),
                    None,
                    None,
                    Arc::new(AdaptiveRateControl::new(false, 0)),
                    Some(job),
                    &DiscoveryTaskParams::default(),
                    Some(lease),
                ),
            )
            .await
        })
    };
    let mut held = None;
    if changed != "fresh" {
        tokio::time::timeout(std::time::Duration::from_secs(5), seen.notified())
            .await
            .unwrap();
        let conn = db.lock().await;
        match changed {
            "pending" => {
                let raw = crate::db::system::encrypt_config_value(
                    &serde_json::to_string(&payload).unwrap(),
                )
                .unwrap();
                conn.execute(
                    "UPDATE pending_callbacks SET payload_json=?1 WHERE idempotency_key=?2",
                    rusqlite::params![raw, "owned-discovery-boundary"],
                )
                .unwrap();
            }
            "result" => crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap(),
            "owner" => {
                let key = format!("review-execution-claim-v1:{item}");
                crate::db::system::set_encrypted_config(
                    &conn,
                    &key,
                    &json!({"format":"review-execution-claim-v1","item_id":item,
                        "owner":uuid::Uuid::new_v4().to_string()})
                    .to_string(),
                )
                .unwrap();
            }
            "cancelled" => work.abort(),
            "busy" => {}
            _ => unreachable!(),
        }
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        if changed == "busy" {
            held = Some(crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap());
        }
        std::fs::write(
            root.path().join(".wptsall-storage-v1.json"),
            br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
        )
        .unwrap();
    }
    let before = {
        let conn = db.lock().await;
        let pending:Option<(String,Option<String>)>=conn.query_row(
            "SELECT payload_json,route_secret_enc FROM pending_callbacks WHERE idempotency_key=?1",
            ["owned-discovery-boundary"],|row|Ok((row.get(0)?,row.get(1)?)),
        ).optional().unwrap();
        let saved = crate::db::jobs::get_item_checked(&conn, item)
            .unwrap()
            .unwrap();
        let job = crate::db::jobs::get_job_checked(&conn, job)
            .unwrap()
            .unwrap();
        (
            pending,
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(job).unwrap(),
        )
    };
    let bytes = std::fs::read(&file).unwrap();
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    release.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap();
    drop(held);
    if changed == "cancelled" {
        assert!(result.unwrap_err().is_cancelled());
    } else {
        assert!(
            result.unwrap().is_err(),
            "changed discovery authority cannot borrow credit: {changed}"
        );
    }
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let conn = db.lock().await;
    let pending: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT payload_json,route_secret_enc FROM pending_callbacks WHERE idempotency_key=?1",
            ["owned-discovery-boundary"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap();
    let saved = crate::db::jobs::get_item_checked(&conn, item)
        .unwrap()
        .unwrap();
    let job = crate::db::jobs::get_job_checked(&conn, saved.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            pending,
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(job).unwrap()
        ),
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
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        usize::from(changed != "fresh")
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn physical_sqlite_discovery_changed_authority_cancelled_or_busy_ack_cannot_borrow_credit() {
    for changed in ["pending", "result", "owner", "cancelled", "busy"] {
        discovery_ack_boundary(changed).await;
    }
}

#[tokio::test]
async fn physical_sqlite_discovery_new_pending_cannot_borrow_ambient_db_credit() {
    discovery_ack_boundary("fresh").await;
}
