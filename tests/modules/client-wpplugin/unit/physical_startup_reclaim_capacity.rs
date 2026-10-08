use super::*;

async fn owned_startup_reclaim_case(i18n: bool, test: &str) {
    if let Ok(root) = std::env::var("WPTSALL_OWNED_QUOTA_STARTUP_CHILD") {
        let root = std::path::Path::new(&root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("sqlite-startup-claim-quota-"));
        crate::worker::run_worker_cli(tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();
        return;
    }
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-startup-claim-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", upstream.local_addr().unwrap());
    let wp_base = format!("{origin}/wp-json/wptsall/v2/owned/client");
    let callbacks = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server = {
        let callbacks = callbacks.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = upstream.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                let (end, length) = loop {
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
                            break (end + 4, length);
                        }
                    }
                };
                let headers = String::from_utf8_lossy(&request[..end]);
                let target = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let callback = target.ends_with("/translation-callback");
                let body = if callback {
                    callbacks
                        .lock()
                        .await
                        .push(serde_json::from_slice(&request[end..end + length]).unwrap());
                    json!({"success":true,"result_id":123,"protocol":"v2","result_status":"synced",
                        "entries_updated":usize::from(i18n),"entries_rejected":0})
                } else if target.ends_with("/site-relations") {
                    json!({"relations":[{"id":7,"source_site_id":1,"source_lang":"en",
                        "target_site_id":2,"target_site_type":"site","target_lang":"zh",
                        "sync_mode":"push","models":[]}]})
                } else if target.contains("/content-changes") {
                    json!({"success":true,"data":{"items":[],"schema_version":1}})
                } else if target.contains("/rules?") {
                    json!({"rules":[]})
                } else if target.contains("/content?") {
                    json!({"items":[],"total":0,"page":1,"per_page":20})
                } else {
                    panic!("no new provider or unknown endpoint is allowed: {target}");
                };
                let body = serde_json::to_string(&body).unwrap();
                let signature = if callback {
                    format!(
                        "X-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\n",
                        crate::web_ui::test_support::sign_wp_plaintext_response(
                            "owned-token",
                            body.as_bytes()
                        )
                    )
                } else {
                    String::new()
                };
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{signature}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        })
    };
    let path = root.join("owned.sqlite");
    let runtime = crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let db = Arc::new(Mutex::new(
        crate::db::open_db(path.to_str().unwrap()).unwrap(),
    ));
    let job = crate::db::jobs::create_job(
        &*db.lock().await,
        &crate::db::jobs::CreateJobRequest {
            domain: wp_base.clone(),
            relation_id: 7,
            business_line: "discovery".into(),
            triggered_by: "auto".into(),
        },
    )
    .unwrap();
    crate::db::discovery_tasks::ensure_discovery_tasks(&*db.lock().await, &wp_base, &[7]).unwrap();
    let file = root.join("owned-original.json");
    let id = crate::db::jobs::record_item_outcome(
        &*db.lock().await,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: wp_base.clone(),
            relation_id: 7,
            business_line: if i18n { "plugin_i18n" } else { "post_content" }.into(),
            object_type: if i18n { "language_pack" } else { "post_type" }.into(),
            wp_object_id: 42,
            wp_object_subtype: if i18n { "plugin" } else { "post" }.into(),
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
            client_task_id: "owned-original-startup".into(),
            max_retries: 3,
        },
        file.to_str().unwrap(),
        "translated",
        None,
    )
    .unwrap();
    let content:TranslationCallbackPayload=serde_json::from_value(json!({
        "relation_id":7,"business_line":"post_content","object_type":"post_type","post_type":"post",
        "object_id":42,"translated_fields":{"post_title":"Owned paid result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"owned-original-startup","worker_id":"owned-worker",
        "source_lang":"en","target_lang":"zh","execution_time_ms":1,"source_revision":"owned-startup-r1",
    })).unwrap();
    let envelope = if i18n {
        serde_json::to_value(serde_json::from_value::<I18nTranslatedEnvelope>(json!({
            "payload_type":"i18n_language_pack","idempotency_key":"owned-original-startup",
            "route_secret":"owned","persisted_at":1,
            "payload":{"business_line":"plugin_i18n","relation_id":7,
                "client_task_id":"owned-original-startup","worker_id":"owned-worker",
                "source_lang":"en","target_lang":"zh","entries":[{"entry_id":42,"msgstr":"Owned paid result"}]},
        })).unwrap()).unwrap()
    } else {
        json!({"idempotency_key":"owned-original-startup","route_secret":"owned","payload":content})
    };
    crate::bindings::save_encrypted_file(&file, &envelope.to_string()).unwrap();
    let lease = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    if i18n {
        let mut conn = db.lock().await;
        let tx = conn.savepoint().unwrap();
        let item = crate::db::jobs::get_item_checked(&tx, id).unwrap().unwrap();
        let envelope =
            crate::task_engine::pipeline::language_pack::read_envelope(&envelope, &item).unwrap();
        crate::task_engine::pipeline::language_pack::prepare_delivery(
            &tx,
            &db,
            &lease,
            &item,
            &wp_base,
            &envelope.payload,
            &envelope.idempotency_key,
        )
        .unwrap();
        tx.commit().unwrap();
    } else {
        crate::db::pending_callbacks::add_pending_callback(
            &*db.lock().await,
            &crate::db::pending_callbacks::PendingCallbackEntry {
                api_base_url: wp_base.clone(),
                idempotency_key: "owned-original-startup".into(),
                payload: content,
                route_secret: Some("owned".into()),
                created_at: 1,
                retry_count: 0,
                last_retry_at: 0,
                relation_id: 7,
                object_id: 42,
                object_type: "post_type".into(),
            },
        )
        .unwrap();
    }
    let bindings = json!({"version":3,"domains":{origin:{
        "wp_client_token":"owned-token","route_secret":"owned","plugin_identity":"wpmmcc_ats",
        "identity_verified_at":crate::bindings::format_rfc3339_utc(crate::bindings::now_unix()),
    }}});
    crate::db::system::set_encrypted_config(
        &*db.lock().await,
        "domain_token_bindings_doc",
        &bindings.to_string(),
    )
    .unwrap();
    crate::db::system::set_system_config(&*db.lock().await, "json_migration_done", "1").unwrap();
    crate::db::system::set_system_config(&*db.lock().await, "log_enabled", "false").unwrap();
    crate::bindings::save_encrypted_file(
        &root.join("WPTSALL_DOMAIN_TOKEN_BINDINGS_FILE"),
        &bindings.to_string(),
    )
    .unwrap();
    drop(lease);
    drop(runtime);
    let bytes = std::fs::read(&file).unwrap();
    {
        let conn = db.lock().await;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    eprintln!("OWNED_STARTUP_RECLAIM_ROOT={}", root.display());
    let output = std::fs::File::create(root.join("owned-child.log")).unwrap();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear().kill_on_drop(true)
        .args(["--exact",&format!("task_engine::discoverer::tests::startup_resume::physical_startup_reclaim_capacity::{test}"),"--nocapture"])
        .current_dir(&root).env("HOME",&root)
        .env("RUST_BACKTRACE","1")
        .env("WPTSALL_OWNED_QUOTA_STARTUP_CHILD",&root)
        .env("WPTSALL_COMPONENT_BINDINGS_SECRET",std::env::var("WPTSALL_COMPONENT_BINDINGS_SECRET").unwrap())
        .env("WPTSALL_DB_PATH",&path).env("WPTSALL_DATA_DIR",&root)
        .env("WPTSALL_USE_SERVER_CONTROL_PLANE","0").env("WPTSALL_ONESHOT","1")
        .env("WPTSALL_COMPONENT_RUNTIME","0").env("WPTSALL_COMPONENT_FALLBACK","1")
        .env("WPTSALL_WP_TRANSPORT_ENCRYPT","off").env("WPTSALL_WP_DEVICE_ID","owned-worker")
        .env("WPTSALL_EVENT_WAIT_ENABLED","0").env("WPTSALL_LOG_ENABLED","0")
        .env("WPTSALL_LOG_FILE",root.join("owned.log")).env("WPTSALL_SERVER_BASE","http://127.0.0.1:1")
        .env("NO_PROXY","*").stdout(output.try_clone().unwrap()).stderr(output);
    for name in [
        "WPTSALL_COMPONENT_BINDINGS_FILE",
        "WPTSALL_DOMAIN_TOKEN_BINDINGS_FILE",
        "WPTSALL_TASK_TYPE_COMPONENT_BINDINGS_FILE",
        "WPTSALL_RULE_COMPONENT_BINDINGS_FILE",
        "WPTSALL_VENDOR_KEYS_FILE",
        "WPTSALL_VENDOR_OAUTH_FILE",
        "WPTSALL_PROXY_PROFILES_FILE",
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        "WPTSALL_SIGNING_PUBLIC_KEY_FILE",
    ] {
        command.env(name, root.join(name));
    }
    let mut child = command.spawn().unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.success(),
        "{}",
        std::fs::read_to_string(root.join("owned-child.log")).unwrap()
    );
    let saved = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert_eq!(
        saved.status, "done",
        "actual fresh worker must finish only the original saved delivery"
    );
    assert_eq!(saved.job_id, job);
    assert_eq!(saved.client_task_id, "owned-original-startup");
    assert_eq!(saved.translated_path, file.to_str().unwrap());
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert_eq!(callbacks.lock().await.len(), 1);
    assert_eq!(
        callbacks.lock().await[0]["client_task_id"],
        "owned-original-startup"
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn physical_sqlite_startup_original_content_reacquires_claim_at_quota() {
    owned_startup_reclaim_case(
        false,
        "physical_sqlite_startup_original_content_reacquires_claim_at_quota",
    )
    .await;
}

#[tokio::test]
async fn physical_sqlite_startup_original_i18n_reacquires_claim_at_quota() {
    owned_startup_reclaim_case(
        true,
        "physical_sqlite_startup_original_i18n_reacquires_claim_at_quota",
    )
    .await;
}
