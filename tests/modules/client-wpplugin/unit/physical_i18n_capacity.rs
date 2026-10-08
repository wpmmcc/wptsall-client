// Owned Linux physical quota oracle; no live site or provider is contacted.
use super::*;

#[tokio::test]
async fn physical_sqlite_i18n_original_delivery_ack_commits_after_quota_drop() {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::Builder::new()
        .prefix("sqlite-i18n-ack-quota-")
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
                let body = r#"{"success":true,"result_id":123,"protocol":"v2","result_status":"synced","entries_updated":1,"entries_rejected":0}"#;
                let signature = sign_plaintext_response_body("owned-token", body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };
    let translated = root.join("owned-i18n.json");
    let payload: I18nCallbackPayload = serde_json::from_value(json!({
        "business_line":"plugin_i18n","relation_id":1,
        "client_task_id":"owned-original-i18n","worker_id":"test-worker",
        "source_lang":"en","target_lang":"zh",
        "entries":[{"entry_id":77,"msgstr":"Owned original result"}]
    }))
    .unwrap();
    let envelope = json!({
        "payload_type":"i18n_language_pack","idempotency_key":"owned-original-i18n",
        "route_secret":"route_test","payload":payload,
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
                business_line: "plugin_i18n".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        crate::db::jobs::record_item_outcome(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id: job,
                domain: base.clone(),
                relation_id: 1,
                business_line: "plugin_i18n".into(),
                object_type: "language_pack".into(),
                wp_object_id: 77,
                wp_object_subtype: "plugin".into(),
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
                client_task_id: "owned-original-i18n".into(),
                max_retries: 2,
            },
            translated.to_str().unwrap(),
            "translated",
            None,
        )
        .unwrap()
    };
    let work = {
        let db = db.clone();
        let base = base.clone();
        let translated = translated.clone();
        tokio::spawn(async move {
            sync_i18n_item_to_wp(
                &db,
                &Client::builder().no_proxy().build().unwrap(),
                item,
                translated.to_str().unwrap(),
                &base,
                "owned-token",
                &test_worker_config(),
                "/dev/null",
                &Arc::new(Semaphore::new(1)),
            )
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), seen.notified())
        .await
        .unwrap();
    let original_delivery = {
        let conn = db.lock().await;
        let raw = crate::db::system::get_system_config_checked(
            &conn,
            &format!("language-pack-delivery-v1:{item}"),
        )
        .unwrap()
        .unwrap();
        assert!(raw.starts_with("V1BUQw"));
        let decoded: Value =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
        assert_eq!(decoded["payload"], serde_json::to_value(&payload).unwrap());
        assert_eq!(decoded["item_id"], item);
        assert_eq!(decoded["site"], base);
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        raw
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
        matches!(&result, Ok(1)),
        "original signed i18n ack must close its saved delivery/item/receipt with prior DB credit: root={} result={result:?} original_delivery_encrypted={}",
        root.display(),
        original_delivery.starts_with("V1BUQw")
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
        .is_some_and(|raw| raw.contains("\"result_id\":123")));
    assert!(crate::db::system::get_system_config_checked(
        &conn,
        &format!("language-pack-delivery-v1:{item}")
    )
    .unwrap()
    .is_none());
    let raw = crate::db::system::get_system_config_checked(
        &conn,
        &format!("language-pack-callback-v1:{item}"),
    )
    .unwrap()
    .unwrap();
    assert!(raw.starts_with("V1BUQw"));
    let receipt: Value =
        serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
    assert_eq!(receipt["payload"], serde_json::to_value(payload).unwrap());
    assert_eq!(receipt["idempotency_key"], "owned-original-i18n");
    assert_eq!(received.lock().unwrap().len(), 1);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

async fn i18n_delivery_boundary(changed: &str) {
    use rusqlite::OptionalExtension;
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
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
                let body = r#"{"success":true,"result_id":123,"protocol":"v2","result_status":"synced","entries_updated":1,"entries_rejected":0}"#;
                let signature = sign_plaintext_response_body("owned-token", body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };
    let file = root.path().join("owned-i18n.json");
    let payload = I18nCallbackPayload {
        business_line: "plugin_i18n".into(),
        relation_id: 1,
        client_task_id: "owned-i18n-boundary".into(),
        worker_id: "owned".into(),
        source_lang: "en".into(),
        target_lang: "zh".into(),
        entries: vec![I18nCallbackEntry {
            entry_id: 77,
            msgstr: "Owned result".into(),
        }],
    };
    let envelope = json!({
        "payload_type":"i18n_language_pack","idempotency_key":"owned-i18n-boundary",
        "route_secret":"route_test","payload":payload,"persisted_at":1,
    });
    crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap();
    let item = {
        let conn = db.lock().await;
        let job = crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: base.clone(),
                relation_id: 1,
                business_line: "plugin_i18n".into(),
                triggered_by: "auto".into(),
            },
        )
        .unwrap();
        crate::db::jobs::record_item_outcome(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id: job,
                domain: base.clone(),
                relation_id: 1,
                business_line: "plugin_i18n".into(),
                object_type: "language_pack".into(),
                wp_object_id: 77,
                wp_object_subtype: "plugin".into(),
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
                client_task_id: "owned-i18n-boundary".into(),
                max_retries: 2,
            },
            file.to_str().unwrap(),
            "translated",
            None,
        )
        .unwrap()
    };
    let lease = crate::db::unit_lock::UnitLease::item(&db, item)
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
        let file = file.clone();
        tokio::spawn(async move {
            crate::storage_capacity::with_result_credit(
                ambient,
                sync_i18n_item_to_wp_claimed(
                    &lease,
                    &db,
                    &Client::builder().no_proxy().build().unwrap(),
                    item,
                    file.to_str().unwrap(),
                    &base,
                    "owned-token",
                    &test_worker_config(),
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
            "intent" | "entry" => {
                let key: String = if changed == "intent" {
                    format!("language-pack-delivery-v1:{item}")
                } else {
                    conn.query_row(
                        "SELECT key FROM system_config WHERE key LIKE 'language-pack-delivery-entry-v1:%'",
                        [],|row|row.get(0),
                    ).unwrap()
                };
                let raw = crate::db::system::get_system_config_checked(&conn, &key)
                    .unwrap()
                    .unwrap();
                let value = crate::db::system::decrypt_config_value(&raw).unwrap();
                crate::db::system::set_encrypted_config(&conn, &key, &value).unwrap();
            }
            "result" => crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap(),
            "owner" => {
                crate::db::system::set_encrypted_config(
                    &conn,
                    &format!("review-execution-claim-v1:{item}"),
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
    let before =
        {
            let conn = db.lock().await;
            let records = conn.prepare(
            "SELECT key,value FROM system_config WHERE key LIKE 'language-pack-%' ORDER BY key"
        ).unwrap().query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))
            .unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
            let item = crate::db::jobs::get_item_checked(&conn, item)
                .unwrap()
                .unwrap();
            let job = crate::db::jobs::get_job_checked(&conn, item.job_id)
                .unwrap()
                .unwrap();
            (
                records,
                serde_json::to_value(item).unwrap(),
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
            "changed i18n authority cannot borrow credit: {changed}"
        );
    }
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let conn = db.lock().await;
    let records = conn
        .prepare(
            "SELECT key,value FROM system_config WHERE key LIKE 'language-pack-%' ORDER BY key",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let saved = crate::db::jobs::get_item_checked(&conn, item)
        .unwrap()
        .unwrap();
    let job = crate::db::jobs::get_job_checked(&conn, saved.job_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            records,
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(job).unwrap()
        ),
        before
    );
    let receipt: Option<String> = conn
        .query_row(
            "SELECT value FROM system_config WHERE key=?1",
            [format!("language-pack-callback-v1:{item}")],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert!(receipt.is_none());
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
async fn physical_sqlite_i18n_changed_authority_cancelled_or_busy_ack_cannot_borrow_credit() {
    for changed in ["intent", "entry", "result", "owner", "cancelled", "busy"] {
        i18n_delivery_boundary(changed).await;
    }
}

#[tokio::test]
async fn physical_sqlite_i18n_fresh_delivery_cannot_borrow_ambient_db_credit() {
    i18n_delivery_boundary("fresh").await;
}
