// catalog: WEBUI-MOD-task-engine-discoverer-rs
// catalog: WEBUI-MOD-db-jobs-rs
// catalog: WEBUI-MOD-task-engine-pipeline-rs
// catalog: WEBUI-MOD-db-translations-rs
// catalog: WEBUI-MOD-worker-rs
// oracle: L2
use super::*;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/physical_startup_reclaim_capacity.rs"]
mod physical_startup_reclaim_capacity;

fn sign_fixture_body(body: &str) -> String {
    use base64::Engine;
    use hmac::Mac;
    let key = crate::crypto::derive_signing_key("fixture-token");
    let mut mac = <hmac::Hmac<sha2::Sha256> as Mac>::new_from_slice(&key).unwrap();
    mac.update(body.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

#[derive(Default)]
struct ResumeCase {
    legacy: bool,
    callback_rejected: bool,
    ack_override: Option<Value>,
    disabled: bool,
    no_relations: bool,
    i18n: bool,
    completion_rejected: bool,
    cli_child_test: Option<&'static str>,
}

#[tokio::test]
async fn cli05_saved_discovery_payload_resumes_with_its_database_domain_not_filename_slug() {
    saved_payload_case(ResumeCase::default()).await;
}

#[tokio::test]
async fn cli05_legacy_slug_requires_the_owning_jobs_exact_wordpress_base() {
    saved_payload_case(ResumeCase {
        legacy: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_callback_failure_keeps_saved_result_and_retry_budget_without_retranslation() {
    saved_payload_case(ResumeCase {
        callback_rejected: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_disabled_relation_keeps_saved_result_and_review_pending() {
    saved_payload_case(ResumeCase {
        disabled: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_empty_relations_do_not_implicitly_authorize_a_saved_callback() {
    saved_payload_case(ResumeCase {
        no_relations: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_saved_i18n_entries_resume_without_translating_or_approving_review_items() {
    saved_payload_case(ResumeCase {
        i18n: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_failed_i18n_callback_throttles_full_base_backlog_without_scanning() {
    saved_payload_case(ResumeCase {
        i18n: true,
        callback_rejected: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn callback_receipt_pending_signed_ack_keeps_original_content_and_queue() {
    saved_payload_case(ResumeCase {
        ack_override: Some(json!({
            "success":true,"result_id":123,"protocol":"v2","result_status":"pending",
            "queued":true,"sync_task_id":7
        })),
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn callback_receipt_failed_signed_writeback_keeps_original_content_and_queue() {
    saved_payload_case(ResumeCase {
        ack_override: Some(json!({
            "success":true,"result_id":123,"protocol":"v2","result_status":"synced",
            "sync_result":{"success":false}
        })),
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn callback_receipt_missing_terminal_evidence_keeps_original_content_and_queue() {
    saved_payload_case(ResumeCase {
        ack_override: Some(json!({"success":true,"result_id":123,"protocol":"v2"})),
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn callback_receipt_incomplete_signed_i18n_count_keeps_saved_paid_entries() {
    saved_payload_case(ResumeCase {
        i18n: true,
        ack_override: Some(json!({
            "success":true,"result_id":123,"protocol":"v2","result_status":"synced",
            "entries_updated":0,"entries_rejected":0
        })),
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_saved_callback_completion_failure_keeps_item_job_queue_and_files() {
    saved_payload_case(ResumeCase {
        completion_rejected: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
async fn cli05_saved_i18n_completion_failure_keeps_item_job_queue_and_files() {
    saved_payload_case(ResumeCase {
        completion_rejected: true,
        i18n: true,
        ..Default::default()
    })
    .await;
}
#[tokio::test]
async fn cli05_real_local_worker_process_resumes_saved_result_from_its_runtime_database() {
    if let Ok(root) = std::env::var("WPTSALL_CLI05_RESUME_CHILD") {
        let root = std::path::Path::new(&root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("cli05-resume-"));
        crate::worker::run_worker_cli(tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();
        return;
    }
    saved_payload_case(ResumeCase {
        cli_child_test: Some(
            "cli05_real_local_worker_process_resumes_saved_result_from_its_runtime_database",
        ),
        ..Default::default()
    })
    .await;
}

async fn saved_payload_case(case: ResumeCase) {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::Builder::new()
        .prefix("cli05-resume-")
        .tempdir()
        .unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let wp_base = format!(
        "http://{}/wp-json/wptsall/v2/secret/client",
        listener.local_addr().unwrap()
    );
    let callbacks = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let seen_callbacks = Arc::clone(&callbacks);
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_requests = Arc::clone(&requests);
    let no_relations = case.no_relations;
    let rejected = case.callback_rejected;
    let callback_failed = rejected || case.ack_override.is_some();
    let ack_override = case.ack_override.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0; 8192];
            let end = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                raw.extend_from_slice(&buffer[..count]);
                assert!(raw.len() < 65536);
                if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8_lossy(&raw[..end]).to_string();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while raw.len() < end + length {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                raw.extend_from_slice(&buffer[..count]);
            }
            let first = headers.lines().next().unwrap();
            let target = first.split_whitespace().nth(1).unwrap();
            seen_requests.lock().await.push(target.to_string());
            let callback = target.ends_with("/translation-callback");
            let body = if callback {
                let idempotency = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("idempotency-key")
                            .then(|| value.trim().to_string())
                    })
                    .unwrap();
                let payload = serde_json::from_slice::<Value>(&raw[end..end + length]).unwrap();
                let entries = payload
                    .get("entries")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                seen_callbacks
                    .lock()
                    .await
                    .push(json!({"idempotency":idempotency, "body":payload}));
                ack_override.clone().unwrap_or_else(|| {
                    json!({
                        "success":!rejected,"result_id":123,"protocol":"v2",
                        "result_status":"synced","entries_updated":entries,"entries_rejected":0
                    })
                })
            } else if target.ends_with("/site-relations") {
                if no_relations {
                    json!({"relations":[]})
                } else {
                    json!({"relations":[{"id":7,"source_site_id":1,"source_lang":"en",
                        "target_site_id":2,"target_site_type":"site","target_lang":"zh","sync_mode":"push","models":[]}]})
                }
            } else if target.contains("/content-changes") {
                json!({"success":true,"data":{"items":[],"schema_version":1}})
            } else if target.contains("/rules?") {
                json!({"rules":[{"id":1,"model_id":1,"name":"fixture","data_type":"post","object_name":"post","translate_fields":[]}]})
            } else if target.contains("/content?") {
                json!({"items":[],"total":0,"page":1,"per_page":20})
            } else {
                panic!("unexpected request, no provider or retranslation allowed: {first}");
            };
            let body = serde_json::to_string(&body).unwrap();
            let signed = if callback {
                format!(
                    "X-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\n",
                    sign_fixture_body(&body)
                )
            } else {
                String::new()
            };
            let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{signed}Content-Length: {}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        }
    });
    let db_path = root.path().join("resume.db");
    let lease = crate::db::runtime::RuntimeLease::acquire(db_path.to_str().unwrap()).unwrap();
    let conn = crate::db::open_db(db_path.to_str().unwrap()).unwrap();
    let job = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: wp_base.clone(),
            relation_id: 7,
            business_line: "discovery".to_string(),
            triggered_by: "auto".to_string(),
        },
    )
    .unwrap();
    crate::db::jobs::update_job_status(&conn, job, "running").unwrap();
    let content_payload = json!({
        "schema_version":crate::config::TASK_CALLBACK_SCHEMA_VERSION,"source_revision":"fixture-source",
        "relation_id":7,"business_line":"post_content","object_type":"post_type","post_type":"post",
        "object_id":42,"translated_fields":{"post_title":"saved paid result"},"translated_meta":{},
        "media_mappings":[],"client_task_id":"fixture-saved-id","worker_id":"fixture-worker",
        "source_lang":"en","target_lang":"zh","execution_time_ms":1
    });
    let payload = if case.i18n {
        json!({"business_line":"plugin_i18n","relation_id":7,"client_task_id":"fixture-saved-id",
            "worker_id":"fixture-worker","source_lang":"en","target_lang":"zh",
            "entries":[{"entry_id":42,"msgstr":"saved paid result"}]})
    } else {
        content_payload
    };
    if case.i18n {
        serde_json::from_value::<I18nCallbackPayload>(payload.clone()).unwrap();
    } else {
        serde_json::from_value::<TranslationCallbackPayload>(payload.clone()).unwrap();
    }
    let legacy_key = pipeline_sanitize_domain_key(&wp_base);
    let mut ids = Vec::new();
    let mut paths = Vec::new();
    for (index, status) in ["translated", "pending_review", "translated"]
        .into_iter()
        .enumerate()
    {
        let foreign_job = if index == 2 {
            crate::db::jobs::create_job(
                &conn,
                &crate::db::jobs::CreateJobRequest {
                    domain: wp_base.replace("/wp-json/", "/other-site/wp-json/"),
                    relation_id: 7,
                    business_line: "discovery".to_string(),
                    triggered_by: "auto".to_string(),
                },
            )
            .unwrap()
        } else {
            job
        };
        let path = root.path().join(format!("saved-{index}.json"));
        std::fs::write(&path, serde_json::to_vec(&json!({
            "idempotency_key":"fixture-saved-id","route_secret":"secret","payload":payload,"persisted_at":1
        })).unwrap()).unwrap();
        let id = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id: foreign_job,
                domain: if case.legacy || index == 2 {
                    legacy_key.clone()
                } else {
                    wp_base.clone()
                },
                relation_id: 7,
                business_line: if case.i18n {
                    "plugin_i18n"
                } else {
                    "post_content"
                }
                .to_string(),
                object_type: if case.i18n {
                    "language_pack"
                } else {
                    "post_type"
                }
                .to_string(),
                wp_object_id: 42 + index as i64,
                wp_object_subtype: "post".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: String::new(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: String::new(),
                client_task_id: "fixture-saved-id".to_string(),
                max_retries: 3,
            },
        )
        .unwrap();
        crate::db::jobs::update_item_translated_path(&conn, id, path.to_str().unwrap()).unwrap();
        crate::db::jobs::update_item_status(&conn, id, status, None).unwrap();
        ids.push(id);
        paths.push(path);
    }
    if case.disabled {
        conn.execute(
            "INSERT INTO discovery_tasks (domain,relation_id,enabled,created_at,updated_at)
             VALUES (?1,7,0,1,1)",
            rusqlite::params![wp_base],
        )
        .unwrap();
    }
    if callback_failed || case.completion_rejected {
        crate::db::system::set_system_config(&conn, "relation_max_pending_callbacks", "1").unwrap();
    }
    if case.completion_rejected {
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_saved_completion BEFORE UPDATE OF status ON translation_items
             WHEN OLD.id = {} AND NEW.status = 'done'
             BEGIN SELECT RAISE(ABORT, 'fixture saved completion failure'); END;",
            ids[0],
        ))
        .unwrap();
    }
    let review_bytes = std::fs::read(&paths[1]).unwrap();
    let saved_bytes = std::fs::read(&paths[0]).unwrap();
    let foreign_bytes = std::fs::read(&paths[2]).unwrap();
    if case.cli_child_test.is_none() {
        crate::db::runtime::recover_interrupted_work(&conn, &lease).unwrap();
    } else {
        // Test-only plaintext at-rest record; the child resolves its own device
        // identity. No real binding or key is copied into this isolated process.
        let origin = wp_base.split("/wp-json/").next().unwrap();
        let verified_at = crate::bindings::format_rfc3339_utc(crate::bindings::now_unix());
        let doc = serde_json::to_string(&json!({"version":3,"domains":{origin:{
            "wp_client_token":"fixture-token","route_secret":"secret","plugin_identity":"wpmmcc_ats",
            "identity_verified_at":verified_at}}})).unwrap();
        crate::db::system::set_system_config(&conn, "domain_token_bindings_doc", &doc).unwrap();
        // CLI's existing file-binding reader consumes the normal mirror too.
        std::fs::write(root.path().join("WPTSALL_DOMAIN_TOKEN_BINDINGS_FILE"), doc).unwrap();
    }
    let db = Arc::new(Mutex::new(conn));
    let pending = Arc::new(Mutex::new(
        PendingCallbackStore::open(db_path.to_str().unwrap()).unwrap(),
    ));
    let report = if let Some(test) = case.cli_child_test {
        drop(lease);
        let output = std::fs::File::create(root.path().join("local-worker-process.log")).unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .env_clear()
            .kill_on_drop(true)
            .args([
                "--exact",
                &format!("task_engine::discoverer::tests::startup_resume::{test}"),
                "--nocapture",
            ])
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("WPTSALL_CLI05_RESUME_CHILD", root.path())
            .env("WPTSALL_DB_PATH", &db_path)
            .env("WPTSALL_DATA_DIR", root.path())
            .env("WPTSALL_USE_SERVER_CONTROL_PLANE", "0")
            .env("WPTSALL_ONESHOT", "1")
            .env("WPTSALL_COMPONENT_RUNTIME", "0")
            .env("WPTSALL_COMPONENT_FALLBACK", "1")
            .env("WPTSALL_WP_TRANSPORT_ENCRYPT", "off")
            .env("WPTSALL_WP_DEVICE_ID", "fixture-worker")
            .env("WPTSALL_EVENT_WAIT_ENABLED", "0")
            .env("WPTSALL_LOG_ENABLED", "0")
            .env("WPTSALL_SERVER_BASE", "http://127.0.0.1:1")
            .env("NO_PROXY", "*")
            .stdout(output.try_clone().unwrap())
            .stderr(output);
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
            command.env(name, root.path().join(name));
        }
        let mut child = command.spawn().unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(15), child.wait())
            .await
            .expect("owned one-shot CLI must finish")
            .unwrap();
        assert!(
            status.success(),
            "{}",
            std::fs::read_to_string(root.path().join("local-worker-process.log")).unwrap()
        );
        None
    } else {
        Some(
            discover_and_translate(
                &Client::builder().no_proxy().build().unwrap(),
                &wp_base,
                "fixture-token",
                root.path().join("resume.log").to_str().unwrap(),
                None,
                None,
                "",
                &[],
                None,
                None,
                &discovery_test_worker_config(),
                Some("secret"),
                &pending,
                None,
                None,
                None,
                Some(Arc::clone(&db)),
                crate::types::PluginIdentity::WpmmccAts,
            )
            .await,
        )
    };
    if case.cli_child_test.is_none()
        && !case.disabled
        && !case.no_relations
        && !callback_failed
        && !case.completion_rejected
    {
        let second = discover_and_translate(
            &Client::builder().no_proxy().build().unwrap(),
            &wp_base,
            "fixture-token",
            root.path().join("resume.log").to_str().unwrap(),
            None,
            None,
            "",
            &[],
            None,
            None,
            &discovery_test_worker_config(),
            Some("secret"),
            &pending,
            None,
            None,
            None,
            Some(Arc::clone(&db)),
            crate::types::PluginIdentity::WpmmccAts,
        )
        .await
        .unwrap();
        assert_eq!(
            second.completed, 0,
            "retained done files must not repeat callback or paid work"
        );
    }
    server.abort();
    let server_result = server.await;
    assert!(
        server_result
            .as_ref()
            .err()
            .is_none_or(|error| error.is_cancelled()),
        "the owned fixture server must not hide panics: {server_result:?}"
    );
    if callback_failed || case.completion_rejected {
        let seen = requests.lock().await;
        assert!(!seen.iter().any(|target|target.contains("/content?")),
            "a full translated backlog must block new content, not just return an empty scan: {seen:?}");
    }
    let expected_callback = !case.disabled && !case.no_relations;
    let success = expected_callback && !callback_failed && !case.completion_rejected;
    if let Some(report) = report {
        let report = report.unwrap();
        assert_eq!(
            report.failed,
            usize::from(expected_callback && (callback_failed || case.completion_rejected))
        );
        assert_eq!(
            report.completed,
            usize::from(success),
            "restart must submit the saved payload, not silently miss the persisted URL key"
        );
    }
    let seen = callbacks.lock().await;
    assert_eq!(
        seen.len(),
        usize::from(expected_callback),
        "pending-review content must not be submitted automatically"
    );
    if expected_callback {
        assert_eq!(seen[0]["idempotency"], "fixture-saved-id");
        assert_eq!(seen[0]["body"]["client_task_id"], "fixture-saved-id");
        if case.i18n {
            assert_eq!(seen[0]["body"]["entries"][0]["msgstr"], "saved paid result");
        } else {
            assert_eq!(
                seen[0]["body"]["translated_fields"]["post_title"],
                "saved paid result"
            );
            assert_eq!(seen[0]["body"]["source_revision"], "fixture-source");
        }
    }
    let conn = db.lock().await;
    if success {
        assert!(
            crate::db::translations::has_materialized_success_record(
                &conn,
                &wp_base,
                7,
                42,
                if case.i18n {
                    "language_pack"
                } else {
                    "post_type"
                },
            ),
            "an acknowledged saved result must remain materialized for the next scan"
        );
        assert_eq!(
            crate::db::pending_callbacks::count_pending_for_relation(&conn, &wp_base, 7).unwrap(),
            0,
            "acknowledging the saved result must remove its parallel pending queue entry"
        );
    }
    if callback_failed {
        assert!(
            !crate::db::translations::has_materialized_success_record(
                &conn,
                &wp_base,
                7,
                42,
                if case.i18n {
                    "language_pack"
                } else {
                    "post_type"
                },
            ),
            "refused signed receipts must not create a false success marker"
        );
        assert_eq!(
            crate::db::pending_callbacks::count_pending_for_relation(&conn, &wp_base, 7).unwrap(),
            i64::from(!case.i18n),
            "content queue must survive an accepted-but-unapplied or damaged receipt"
        );
    }
    if case.completion_rejected {
        assert!(
            !crate::db::translations::has_materialized_success_record(
                &conn,
                &wp_base,
                7,
                42,
                if case.i18n {
                    "language_pack"
                } else {
                    "post_type"
                },
            ),
            "failed completion must not leave a false materialized cache entry"
        );
        assert_eq!(
            crate::db::jobs::get_item(&conn, ids[0])
                .unwrap()
                .sync_response_json,
            None,
            "failed completion must roll back its saved acknowledgement"
        );
        assert_eq!(
            crate::db::pending_callbacks::count_pending_for_relation(&conn, &wp_base, 7).unwrap(),
            i64::from(!case.i18n),
            "content's pending queue entry must survive failed completion"
        );
    }
    assert_eq!(
        crate::db::jobs::get_item(&conn, ids[0]).unwrap().status,
        if success { "done" } else { "translated" }
    );
    assert_eq!(
        crate::db::jobs::get_item(&conn, ids[1]).unwrap().status,
        "pending_review"
    );
    assert_eq!(std::fs::read(&paths[1]).unwrap(), review_bytes);
    assert_eq!(std::fs::read(&paths[2]).unwrap(), foreign_bytes);
    assert_eq!(
        crate::db::jobs::get_item(&conn, ids[2]).unwrap().status,
        "translated",
        "another WP installation on the same host is not this client's database origin"
    );
    assert_eq!(
        std::fs::read(&paths[0]).unwrap(),
        saved_bytes,
        "callback success is not authorization to delete or mutate the paid result"
    );
    assert_eq!(
        crate::db::jobs::get_item(&conn, ids[0])
            .unwrap()
            .retry_count,
        i64::from(expected_callback && callback_failed)
    );
    assert_eq!(
        crate::db::jobs::get_job(&conn, job).unwrap().done_items,
        i64::from(success)
    );
    assert_eq!(
        crate::db::jobs::get_job(&conn, job).unwrap().status,
        "partial",
        "the saved result's owner remains partial while review is still pending"
    );
    let backlog =
        crate::db::jobs::count_items_by_status_for_relation(&conn, &wp_base, 7, "translated")
            .unwrap()
            + crate::db::jobs::count_legacy_items_by_status_for_relation(
                &conn,
                &wp_base,
                &legacy_key,
                7,
                "translated",
            )
            .unwrap();
    assert_eq!(
        backlog,
        i64::from(!success),
        "backlog uses the same full-base ownership as resume"
    );
}
