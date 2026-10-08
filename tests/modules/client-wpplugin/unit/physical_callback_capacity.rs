use super::*;
use rusqlite::OptionalExtension;

#[tokio::test]
async fn physical_sqlite_callback_original_ack_commits_after_quota_drop() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-callback-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
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
    let worker = {
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
                }
                received.lock().unwrap().push(bytes);
                seen.notify_one();
                release.notified().await;
                let body = r#"{"success":true,"result_id":123,"queued":false,"sync_task_id":0,"protocol":"v2","result_status":"synced"}"#;
                let signature = sign_plaintext_response_body("owned-token", body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };
    let translated = root.join("owned-translated.json");
    let envelope = json!({
        "idempotency_key":"owned-original-callback",
        "route_secret":"route_test",
        "payload":{
            "relation_id":1,"business_line":"ats","object_type":"post_type","post_type":"post",
            "object_id":77,"translated_fields":{"post_title":"Owned result"},"translated_meta":{},
            "media_mappings":[],"client_task_id":"owned-original-callback","worker_id":"test-worker",
            "source_lang":"en","target_lang":"zh","source_revision":"owned-r1","execution_time_ms":1
        }
    });
    crate::bindings::save_encrypted_file(&translated, &envelope.to_string()).unwrap();
    let original_bytes = std::fs::read(&translated).unwrap();
    let item = {
        let conn = db.lock().await;
        let job = crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: base.clone(),
                relation_id: 1,
                business_line: "ats".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        let item = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id: job,
                domain: base.clone(),
                relation_id: 1,
                business_line: "ats".into(),
                object_type: "post_type".into(),
                wp_object_id: 77,
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
                raw_path: "".into(),
                client_task_id: "owned-original-callback".into(),
                max_retries: 2,
            },
        )
        .unwrap();
        crate::db::jobs::update_item_translated_path(&conn, item, translated.to_str().unwrap())
            .unwrap();
        crate::db::jobs::update_item_status(&conn, item, "translated", None).unwrap();
        item
    };
    let work = {
        let db = db.clone();
        let base = base.clone();
        let translated = translated.clone();
        tokio::spawn(async move {
            sync_item_to_wp(
                &db,
                &Client::builder().no_proxy().build().unwrap(),
                item,
                translated.to_str().unwrap(),
                &base,
                "owned-token",
                &test_worker_config(),
                Some("route_test"),
                "/dev/null",
                &Arc::new(Semaphore::new(1)),
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), seen.notified())
        .await
        .unwrap();
    {
        let conn = db.lock().await;
        let pending =
            crate::db::pending_callbacks::find_pending_callback(&conn, &base, 1, "post_type", 77)
                .unwrap()
                .unwrap();
        assert_eq!(pending.idempotency_key, "owned-original-callback");
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
    release.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "original signed ack must commit receipt/item/job with prior DB credit: root={} result={result:?}",
        root.display()
    );
    assert_eq!(std::fs::read(&translated).unwrap(), original_bytes);
    assert!(root.join("owned.sqlite-wal").metadata().unwrap().len() > 0);
    let conn = db.lock().await;
    let saved = crate::db::jobs::get_item_checked(&conn, item)
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "done");
    assert!(saved
        .sync_response_json
        .as_deref()
        .is_some_and(|response| response.contains("\"result_id\":123")));
    assert!(
        crate::db::pending_callbacks::find_pending_callback(&conn, &base, 1, "post_type", 77,)
            .unwrap()
            .is_none()
    );
    assert_eq!(received.lock().unwrap().len(), 1);
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
}

async fn refused_callback_keeps_original_authority(changed: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
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
    let worker = {
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
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                seen.notify_one();
                release.notified().await;
                let body = r#"{"success":true,"result_id":123,"queued":false,"sync_task_id":0,"protocol":"v2","result_status":"synced"}"#;
                let signature = sign_plaintext_response_body("owned-token", body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };
    let file = root.path().join("owned-translated.json");
    let envelope = json!({
        "idempotency_key":"owned-retained-callback","route_secret":"route_test",
        "payload":{
            "relation_id":1,"business_line":"ats","object_type":"post_type","post_type":"post",
            "object_id":77,"translated_fields":{"post_title":"Owned result"},"translated_meta":{},
            "media_mappings":[],"client_task_id":"owned-retained-callback","worker_id":"test-worker",
            "source_lang":"en","target_lang":"zh","source_revision":"owned-r1","execution_time_ms":1
        }
    });
    crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap();
    let item = {
        let conn = db.lock().await;
        let job = crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: base.clone(),
                relation_id: 1,
                business_line: "ats".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        let item = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id: job,
                domain: base.clone(),
                relation_id: 1,
                business_line: "ats".into(),
                object_type: "post_type".into(),
                wp_object_id: 77,
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
                raw_path: String::new(),
                client_task_id: "owned-retained-callback".into(),
                max_retries: 2,
            },
        )
        .unwrap();
        crate::db::jobs::update_item_translated_path(&conn, item, file.to_str().unwrap()).unwrap();
        crate::db::jobs::update_item_status(&conn, item, "translated", None).unwrap();
        item
    };
    let lease = crate::db::unit_lock::UnitLease::item(&db, item)
        .await
        .unwrap();
    if changed == "fresh" {
        db.lock()
            .await
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|busy| assert_eq!(busy, 0))
            .unwrap();
        std::fs::write(
            root.path().join(".wptsall-storage-v1.json"),
            br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
        )
        .unwrap();
    }
    let work = {
        let db = db.clone();
        let base = base.clone();
        let file = file.clone();
        let credit = if changed == "fresh" {
            crate::storage_capacity::database_recovery_credit(
                &root.path().join("owned.sqlite"),
                false,
            )
            .unwrap()
        } else {
            None
        };
        tokio::spawn(async move {
            crate::storage_capacity::with_result_credit(
                credit,
                sync_item_to_wp_claimed(
                    &lease,
                    &db,
                    &Client::builder().no_proxy().build().unwrap(),
                    item,
                    file.to_str().unwrap(),
                    &base,
                    "owned-token",
                    &test_worker_config(),
                    Some("route_test"),
                    "/dev/null",
                    &Arc::new(Semaphore::new(1)),
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
            "ciphertext" => {
                let pending = crate::db::pending_callbacks::find_pending_callback(
                    &conn,
                    &base,
                    1,
                    "post_type",
                    77,
                )
                .unwrap()
                .unwrap();
                let raw = crate::db::system::encrypt_config_value(
                    &serde_json::to_string(&pending.payload).unwrap(),
                )
                .unwrap();
                conn.execute(
                    "UPDATE pending_callbacks SET payload_json=?1 WHERE idempotency_key=?2",
                    rusqlite::params![raw, pending.idempotency_key],
                )
                .unwrap();
            }
            "result" => {
                crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap();
            }
            "cancelled" => {
                work.abort();
            }
            "busy" => {}
            _ => unreachable!(),
        }
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|busy| assert_eq!(busy, 0))
        .unwrap();
        if changed == "busy" {
            held = Some(crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap());
        }
        std::fs::write(
            root.path().join(".wptsall-storage-v1.json"),
            br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
        )
        .unwrap();
    }
    let file_bytes = std::fs::read(&file).unwrap();
    let (pending_bytes, before_item, before_job) = {
        let conn = db.lock().await;
        let pending: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT payload_json,route_secret_enc FROM pending_callbacks
                 WHERE idempotency_key='owned-retained-callback'",
                [],
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
        (
            pending,
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(job).unwrap(),
        )
    };
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
            "{changed}: original authority must be retained"
        );
    }
    let conn = db.lock().await;
    let saved = crate::db::jobs::get_item_checked(&conn, item)
        .unwrap()
        .unwrap();
    let job = crate::db::jobs::get_job_checked(&conn, saved.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(saved).unwrap(), before_item);
    assert_eq!(serde_json::to_value(job).unwrap(), before_job);
    let after_pending: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT payload_json,route_secret_enc FROM pending_callbacks
             WHERE idempotency_key='owned-retained-callback'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap();
    assert_eq!(after_pending, pending_bytes);
    assert_eq!(std::fs::read(&file).unwrap(), file_bytes);
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
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        usize::from(changed != "fresh")
    );
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn physical_sqlite_callback_fresh_intent_is_not_financed_by_db_recovery() {
    refused_callback_keeps_original_authority("fresh").await;
}

#[tokio::test]
async fn physical_sqlite_callback_changed_authority_cannot_finance_original_ack() {
    for changed in ["ciphertext", "result", "cancelled", "busy"] {
        refused_callback_keeps_original_authority(changed).await;
    }
}

#[tokio::test]
async fn physical_sqlite_callback_legacy_authority_stays_readable_without_credit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let conn = crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap();
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":1,"business_line":"ats","object_type":"post_type","post_type":"post",
        "object_id":77,"translated_fields":{"post_title":"Owned legacy result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"owned-legacy-callback","worker_id":"owned",
        "source_lang":"en","target_lang":"zh","source_revision":"owned-r1","execution_time_ms":1
    }))
    .unwrap();
    let entry = crate::db::pending_callbacks::PendingCallbackEntry {
        api_base_url: "http://127.0.0.1:1234/owned".into(),
        idempotency_key: "owned-legacy-callback".into(),
        payload: payload.clone(),
        route_secret: Some("owned-route".into()),
        created_at: 1,
        retry_count: 0,
        last_retry_at: 0,
        relation_id: 1,
        object_id: 77,
        object_type: "post_type".into(),
    };
    crate::db::pending_callbacks::add_pending_callback(&conn, &entry).unwrap();
    let (_, encrypted) = crate::db::pending_callbacks::continuation_authority(
        &conn,
        &entry.api_base_url,
        &entry.idempotency_key,
        &payload,
        Some("owned-route"),
    )
    .unwrap();
    assert!(encrypted);
    let original_payload = serde_json::to_string(&payload).unwrap();
    conn.execute(
        "UPDATE pending_callbacks SET api_base_url=?1,payload_json=?2,route_secret_enc=?3
         WHERE idempotency_key=?4",
        rusqlite::params![
            entry.api_base_url,
            original_payload,
            entry.route_secret,
            entry.idempotency_key
        ],
    )
    .unwrap();
    let (authority, encrypted) = crate::db::pending_callbacks::continuation_authority(
        &conn,
        &entry.api_base_url,
        &entry.idempotency_key,
        &payload,
        Some("owned-route"),
    )
    .unwrap();
    assert!(!authority.is_empty());
    assert!(
        !encrypted,
        "legacy plaintext authority must not obtain a new DB loan"
    );
    let after: (String, String, String) = conn
        .query_row(
            "SELECT api_base_url,payload_json,route_secret_enc FROM pending_callbacks
             WHERE idempotency_key=?1",
            [&entry.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        after,
        (entry.api_base_url, original_payload, "owned-route".into())
    );
}
