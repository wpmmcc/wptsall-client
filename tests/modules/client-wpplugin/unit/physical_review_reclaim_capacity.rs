use super::*;

const REQUEST: &str = "c1b05b00-4af3-4a5f-9d88-13b6dc674f11";

async fn dispatch_review_http(
    state: &Arc<Mutex<WebUiState>>,
    id: i64,
    name: &str,
    body: serde_json::Value,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = state.clone();
    let handler = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        crate::web_ui::routes::handle_web_ui_connection(
            socket,
            state,
            crate::types::WebUiRuntimeControl::new(),
            "/dev/null",
            crate::logging::unix_ts(),
            std::time::Instant::now(),
            crate::web_ui::AccessControl::new(false, &[]),
        )
        .await
        .unwrap();
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let headers = format!(
        "POST /api/items/{id}/{name} HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1:8977\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        bytes.len()
    );
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(&bytes).await.unwrap();
    socket.shutdown().await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    handler.await.unwrap();
    String::from_utf8(response).unwrap()
}

async fn original_review_reclaim_at_quota(kind: &str) {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::Builder::new()
        .prefix("sqlite-review-claim-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let _transport = EnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let _log = EnvVarGuard::set("WPTSALL_LOG_FILE", root.join("owned.log").to_str().unwrap());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", upstream.local_addr().unwrap());
    let wp_base = format!("{origin}/wp-json/wptsall/v2/owned/client");
    let requests = Arc::new(AtomicUsize::new(0));
    let server = {
        let requests = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = upstream.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    assert!(request.len() < 64 * 1024);
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
                            assert!(headers
                                .lines()
                                .next()
                                .unwrap()
                                .contains("/translation-callback "));
                            break;
                        }
                    }
                }
                requests.fetch_add(1, Ordering::SeqCst);
                let body =
                    br#"{"success":true,"result_id":123,"protocol":"v2","result_status":"synced"}"#;
                let signature =
                    crate::web_ui::test_support::sign_wp_plaintext_response("owned-token", body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            }
        })
    };
    let state = build_test_web_ui_state("", None);
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    {
        let mut guard = state.lock().await;
        guard.db = db.clone();
        guard.domain_token_bindings_path = root.join("bindings.json").display().to_string();
        guard.domain_token_bindings.domains.insert(
            origin.clone(),
            DomainTokenBindingEntry {
                wp_client_token: "owned-token".into(),
                route_secret: "owned".into(),
                ..Default::default()
            },
        );
        guard.component_bindings_path = root.join("components-bindings.json").display().to_string();
        guard.task_type_component_bindings_path =
            root.join("task-types.json").display().to_string();
        guard.rule_component_bindings_path = root.join("rules.json").display().to_string();
    }
    let path = root.join("owned-paid-candidate.json");
    let id = seed_translation_item_for_web_ui_test(
        &state,
        &origin,
        path.to_str().unwrap(),
        "pending_review",
        "owned-original-review",
    )
    .await;
    if kind == "manual-i18n" {
        db.lock()
            .await
            .execute(
                "UPDATE translation_items SET object_type='language_pack',
             business_line='plugin_i18n',wp_object_subtype='plugin' WHERE id=?1",
                [id],
            )
            .unwrap();
    }
    let content: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":1,"business_line":"post_content","object_type":"post_type",
        "post_type":"post","object_id":88,"translated_fields":{"post_title":"Owned paid result"},
        "translated_meta":{},"media_mappings":[],"client_task_id":"owned-original-review",
        "worker_id":"device-test","source_lang":"en_US","target_lang":"zh_CN",
        "execution_time_ms":1,"source_revision":"owned-review-r1",
    }))
    .unwrap();
    let envelope = if kind == "manual-i18n" {
        serde_json::to_value(
            serde_json::from_value::<I18nTranslatedEnvelope>(json!({
                "payload_type":"i18n_language_pack","idempotency_key":"owned-original-review",
                "route_secret":"owned","persisted_at":1,
                "payload":{"business_line":"plugin_i18n","relation_id":1,
                    "client_task_id":"owned-original-review","worker_id":"device-test",
                    "source_lang":"en_US","target_lang":"zh_CN",
                    "entries":[{"entry_id":88,"msgstr":"Owned paid result"}]},
            }))
            .unwrap(),
        )
        .unwrap()
    } else {
        json!({"idempotency_key":"owned-original-review","payload":content,"route_secret":"owned"})
    };
    let lease = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    if kind.starts_with("manual") {
        let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap();
        let scope = crate::db::review_attempts::scope(
            &*db.lock().await,
            &db,
            &lease,
            &item,
            &json!({"owned":"paid source"}),
            REQUEST,
        )
        .unwrap();
        let encoded = crate::db::review_attempts::prepare_result(
            &db,
            &lease,
            &scope,
            id,
            &envelope,
            path.to_str().unwrap(),
        )
        .await
        .unwrap();
        crate::task_engine::pipeline::install_json_snapshot(&path, &envelope, &encoded).unwrap();
    } else {
        crate::bindings::save_encrypted_file(&path, &envelope.to_string()).unwrap();
        crate::db::pending_callbacks::add_pending_callback(
            &*db.lock().await,
            &crate::db::pending_callbacks::PendingCallbackEntry {
                api_base_url: wp_base.clone(),
                idempotency_key: "owned-original-review".into(),
                payload: content.clone(),
                route_secret: Some("owned".into()),
                created_at: 1,
                retry_count: 0,
                last_retry_at: 0,
                relation_id: 1,
                object_id: 88,
                object_type: "post_type".into(),
            },
        )
        .unwrap();
    }
    drop(lease);
    let bytes = std::fs::read(&path).unwrap();
    let job = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap()
        .job_id;
    {
        let conn = db.lock().await;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    eprintln!("OWNED_REVIEW_RECLAIM_ROOT={}", root.display());
    let name = if kind.starts_with("manual") {
        "retranslate"
    } else {
        "approve"
    };
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        dispatch_review_http(
            &state,
            id,
            name,
            json!({"request_id":REQUEST,"resume_only":true}),
        ),
    )
    .await
    .unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "original {kind} HTTP recovery must reacquire its claim: {response}"
    );
    let saved = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.job_id, job);
    assert_eq!(saved.client_task_id, "owned-original-review");
    assert_eq!(saved.translated_path, path.to_str().unwrap());
    assert_eq!(
        saved.status,
        if kind.starts_with("manual") {
            "pending_review"
        } else {
            "done"
        }
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        requests.load(Ordering::SeqCst),
        usize::from(!kind.starts_with("manual"))
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn physical_sqlite_review_http_original_manual_content_reacquires_claim_at_quota() {
    original_review_reclaim_at_quota("manual-content").await;
}

#[tokio::test]
async fn physical_sqlite_review_http_original_manual_i18n_reacquires_claim_at_quota() {
    original_review_reclaim_at_quota("manual-i18n").await;
}

#[tokio::test]
async fn physical_sqlite_review_http_original_pending_ack_reacquires_claim_at_quota() {
    original_review_reclaim_at_quota("pending-ack").await;
}
