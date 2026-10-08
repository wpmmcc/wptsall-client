use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/frozen_manual_fixture_isolation.rs"]
mod frozen_manual_fixture_isolation;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/physical_review_reclaim_capacity.rs"]
mod physical_review_reclaim_capacity;
mod process;

struct OwnedUpstream {
    task: tokio::task::JoinHandle<()>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    requests: Arc<AtomicUsize>,
    base: String,
}

impl Drop for OwnedUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OwnedUpstream {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let requests = Arc::new(AtomicUsize::new(0));
        let (ready, unblock, count) = (entered.clone(), release.clone(), requests.clone());
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (ready, unblock, count) = (ready.clone(), unblock.clone(), count.clone());
                handlers.spawn(async move {
                    let mut request = vec![0; 32 * 1024];
                    let _ = socket.read(&mut request).await.unwrap();
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        ready.notify_one();
                        unblock.notified().await;
                    }
                    let body = r#"{"code":"owned_refusal","message":"owned callback refuses"}"#;
                    let response = format!(
                        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            task,
            entered,
            release,
            requests,
            base,
        }
    }
}

async fn action(state: &Arc<Mutex<WebUiState>>, id: i64, name: &str, body: &[u8]) -> String {
    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    let id = id.to_string();
    match name {
        "approve" => handle_item_approve(&mut socket, state, &id).await,
        "reject" => handle_item_reject(&mut socket, state, &id, body).await,
        "edit" => handle_item_translated_save(&mut socket, state, body, &id).await,
        "override" => handle_item_override_save(&mut socket, state, body, &id).await,
        "content" => handle_item_content(&mut socket, state, &id).await,
        "batch-reject" => handle_items_batch_reject(&mut socket, state, body).await,
        "retranslate" => {
            handle_item_retranslate(
                &mut socket,
                state,
                &id,
                if body.is_empty() {
                    br#"{"request_id":"c1b05b00-4af3-4a5f-9d88-13b6dc674f11"}"#
                } else {
                    body
                },
            )
            .await
        }
        _ => unreachable!(),
    }
    .unwrap();
    drop(socket);
    reader.await.unwrap()
}

async fn competing_review_action(name: &'static str) {
    let _env = components_env_lock().lock().unwrap();
    let _retry = EnvVarGuard::set("WPTSALL_RETRY_MAX", "0");
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-result.json");
    write_test_translation_envelope(path.to_str().unwrap(), "owned-review-attempt");
    let original = std::fs::read(&path).unwrap();
    let upstream = OwnedUpstream::start().await;
    let state = build_test_web_ui_state("", None);
    state.lock().await.domain_token_bindings.domains.insert(
        upstream.base.clone(),
        DomainTokenBindingEntry {
            wp_client_token: "owned-mock-token".into(),
            route_secret: "owned-mock-route".into(),
            ..Default::default()
        },
    );
    let id = seed_translation_item_for_web_ui_test(
        &state,
        &upstream.base,
        path.to_str().unwrap(),
        "pending_review",
        "owned-review-attempt",
    )
    .await;
    let first_state = state.clone();
    let first = tokio::spawn(async move { action(&first_state, id, "approve", b"").await });
    tokio::time::timeout(Duration::from_secs(5), upstream.entered.notified())
        .await
        .unwrap();
    let body = serde_json::to_vec(&json!({
        "content": {"translated_fields": {"post_title": "competitor"}},
        "reason": "competitor",
        "source_lang": "fr"
    }))
    .unwrap();
    let competing = tokio::time::timeout(Duration::from_secs(3), action(&state, id, name, &body))
        .await
        .unwrap();
    upstream.release.notify_one();
    let original_response = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .unwrap()
        .unwrap();
    let count = upstream.requests.load(Ordering::SeqCst);
    let saved = std::fs::read(&path).unwrap();
    drop(upstream);
    assert!(
        original_response.contains("owned_refusal"),
        "{original_response}"
    );
    assert!(competing.starts_with("HTTP/1.1 409"), "{name}: {competing}");
    assert_eq!(
        count, 1,
        "a competing action must not send another callback"
    );
    assert_eq!(
        saved, original,
        "a competing edit changed the in-flight intent"
    );
}

#[tokio::test]
async fn remaining_review_approve_excludes_other_approve() {
    competing_review_action("approve").await;
}

#[tokio::test]
async fn remaining_review_approve_excludes_reject() {
    competing_review_action("reject").await;
}

#[tokio::test]
async fn remaining_review_approve_excludes_edit() {
    competing_review_action("edit").await;
}

#[tokio::test]
async fn remaining_review_approve_excludes_override() {
    competing_review_action("override").await;
}

#[tokio::test]
async fn remaining_review_corrupt_envelope_is_retained_not_replaced() {
    let state = build_test_web_ui_state("", None);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-damaged-result.json");
    std::fs::write(&path, "{damaged").unwrap();
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "https://owned.invalid",
        path.to_str().unwrap(),
        "pending_review",
        "owned-damaged",
    )
    .await;
    let response = action(&state, id, "edit", br#"{"content":{"post_title":"new"}}"#).await;
    assert!(!response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(std::fs::read(&path).unwrap(), b"{damaged");
}

#[tokio::test]
async fn remaining_review_done_item_does_not_enter_retranslate() {
    let _key = crate::db::owned_mock_bindings_key();
    let state = build_test_web_ui_state("", None);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-raw.json");
    std::fs::write(&path, r#"{"post_title":"owned source"}"#).unwrap();
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "https://owned.invalid",
        "",
        "done",
        "owned-done",
    )
    .await;
    let db = state.lock().await.db.clone();
    db.lock()
        .await
        .execute(
            "UPDATE translation_items SET raw_path=?1 WHERE id=?2",
            rusqlite::params![path.to_str().unwrap(), id],
        )
        .unwrap();
    let response = action(&state, id, "retranslate", b"").await;
    assert!(response.contains("INVALID_STATUS"), "{response}");
}

#[tokio::test]
async fn remaining_manual_resume_only_unknown_uuid_refuses_before_source_or_configuration() {
    let _key = crate::db::owned_mock_bindings_key();
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:9",
        "",
        "pending_review",
        "owned",
    )
    .await;
    let response = action(
        &state,
        id,
        "retranslate",
        br#"{"request_id":"c1b05b00-4af3-4a5f-9d88-13b6dc674f11","resume_only":true}"#,
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 409"), "{response}");
    assert!(response.contains("MANUAL_REQUEST_UNKNOWN"),
        "a browser-only or foreign-database UUID must not silently create a new paid request: {response}");
    let db = state.lock().await.db.clone();
    assert!(
        crate::db::review_attempts::pending_request(&*db.lock().await, id)
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn remaining_review_cancelled_delivery_preserves_intent_against_edit() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-result.json");
    write_test_translation_envelope(path.to_str().unwrap(), "owned-cancelled-review");
    let original = std::fs::read(&path).unwrap();
    let upstream = OwnedUpstream::start().await;
    let state = build_test_web_ui_state("", None);
    state.lock().await.domain_token_bindings.domains.insert(
        upstream.base.clone(),
        DomainTokenBindingEntry {
            wp_client_token: "owned-token".into(),
            route_secret: "owned-route".into(),
            ..Default::default()
        },
    );
    let id = seed_translation_item_for_web_ui_test(
        &state,
        &upstream.base,
        path.to_str().unwrap(),
        "pending_review",
        "owned-cancelled-review",
    )
    .await;
    let first_state = state.clone();
    let owner = tokio::spawn(async move { action(&first_state, id, "approve", b"").await });
    tokio::time::timeout(Duration::from_secs(5), upstream.entered.notified())
        .await
        .unwrap();
    owner.abort();
    assert!(owner.await.unwrap_err().is_cancelled());
    let response = action(
        &state,
        id,
        "edit",
        br#"{"content":{"post_title":"changed after cancel"}}"#,
    )
    .await;
    upstream.release.notify_one();
    drop(upstream);
    assert!(!response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let db = state.lock().await.db.clone();
    assert_eq!(
        db.lock()
            .await
            .query_row("SELECT COUNT(*) FROM pending_callbacks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn remaining_review_damaged_saved_policy_is_not_silently_overridden() {
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "https://owned.invalid",
        "",
        "pending_review",
        "owned-policy",
    )
    .await;
    let db = state.lock().await.db.clone();
    db.lock()
        .await
        .execute(
            "UPDATE translation_items SET editable_overrides_json='{damaged' WHERE id=?1",
            [id],
        )
        .unwrap();
    let response = action(&state, id, "override", br#"{"source_lang":"fr"}"#).await;
    assert!(!response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        db.lock()
            .await
            .query_row(
                "SELECT editable_overrides_json FROM translation_items WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        "{damaged"
    );
}

#[tokio::test]
async fn remaining_review_rejection_projection_is_terminal() {
    let _env = components_env_lock().lock().unwrap();
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "https://owned.invalid",
        "",
        "pending_review",
        "owned-reject",
    )
    .await;
    let response = action(&state, id, "reject", br#"{"reason":"owned"}"#).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let db = state.lock().await.db.clone();
    let conn = db.lock().await;
    let item = crate::db::jobs::get_item(&conn, id).unwrap();
    assert_eq!(
        crate::db::jobs::count_pending_items(&conn, item.job_id).unwrap(),
        0
    );
    assert_eq!(
        crate::db::jobs::project_job_from_items(&conn, item.job_id, true, "failed").unwrap(),
        "failed"
    );
}

#[tokio::test]
async fn remaining_review_closed_delivery_budget_is_not_permission_to_send() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result.json");
    write_test_translation_envelope(path.to_str().unwrap(), "owned-closed-budget");
    let upstream = OwnedUpstream::start().await;
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        &upstream.base,
        path.to_str().unwrap(),
        "translated",
        "owned-closed-budget",
    )
    .await;
    let db = state.lock().await.db.clone();
    let device = state.lock().await.device_id.clone();
    let config = crate::worker::build_worker_config(&device);
    let budget = Arc::new(tokio::sync::Semaphore::new(1));
    budget.close();
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        crate::task_engine::pipeline::sync_item_to_wp(
            &db,
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            id,
            path.to_str().unwrap(),
            &format!("{}/wp-json/wptsall/v2/owned/client", upstream.base),
            "owned-token",
            &config,
            Some("owned-route"),
            "/dev/null",
            &budget,
        ),
    )
    .await;
    let requests = upstream.requests.load(Ordering::SeqCst);
    drop(upstream);
    assert!(result.is_ok(), "closed budget must refuse before egress");
    assert!(result.unwrap().is_err());
    assert_eq!(requests, 0);
}

#[tokio::test]
async fn remaining_review_edit_commits_new_identity_without_replacing_original_evidence() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result.json");
    write_test_translation_envelope(path.to_str().unwrap(), "owned-edit");
    let original = std::fs::read(&path).unwrap();
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "https://owned.invalid",
        path.to_str().unwrap(),
        "pending_review",
        "owned-edit",
    )
    .await;
    let response = action(
        &state,
        id,
        "edit",
        br#"{"content":{"post_title":"owned-edited"}}"#,
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let db = state.lock().await.db.clone();
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    let bytes = std::fs::read(&item.translated_path).unwrap();
    assert!(bytes.starts_with(b"WPTC"));
    assert!(!bytes
        .windows("owned-edited".len())
        .any(|window| window == b"owned-edited"));
    let saved: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&item.translated_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["payload"]["client_task_id"].as_str().unwrap(),
        item.client_task_id
    );
    assert_ne!(item.client_task_id, "owned-edit");
    assert_eq!(
        saved["payload"]["translated_fields"]["post_title"],
        "owned-edited"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn remaining_review_edit_projection_failure_preserves_original_pointer_and_evidence() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result.json");
    write_test_translation_envelope(path.to_str().unwrap(), "owned-edit-refused");
    let original = std::fs::read(&path).unwrap();
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "https://owned.invalid",
        path.to_str().unwrap(),
        "pending_review",
        "owned-edit-refused",
    )
    .await;
    let db = state.lock().await.db.clone();
    db.lock().await.execute_batch(
        "CREATE TRIGGER deny_edit BEFORE UPDATE ON translation_items
         WHEN NEW.client_task_id != OLD.client_task_id BEGIN SELECT RAISE(ABORT,'owned edit refusal'); END;"
    ).unwrap();
    let response = action(
        &state,
        id,
        "edit",
        br#"{"content":{"post_title":"not-committed"}}"#,
    )
    .await;
    assert!(!response.starts_with("HTTP/1.1 200"), "{response}");
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert_eq!(item.client_task_id, "owned-edit-refused");
    assert_eq!(item.translated_path, path.to_str().unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn remaining_manual_complete_file_after_db_refusal_recovers_before_wp_or_provider() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = build_test_web_ui_state("", None);
    let original = root.path().join("original.json");
    write_test_translation_envelope(original.to_str().unwrap(), "owned-original");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&original).unwrap()).unwrap();
    let payload: crate::types::TranslationCallbackPayload =
        serde_json::from_value(saved["payload"].clone()).unwrap();
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:9",
        original.to_str().unwrap(),
        "pending_review",
        "owned-original",
    )
    .await;
    let db = state.lock().await.db.clone();
    let lease = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    let request = "c1b05b00-4af3-4a5f-9d88-13b6dc674f11";
    let scope = crate::db::review_attempts::scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"owned":"source"}),
        request,
    )
    .unwrap();
    db.lock().await.execute_batch("CREATE TRIGGER deny_manual_result BEFORE UPDATE ON translation_items
        WHEN NEW.status='pending_review' BEGIN SELECT RAISE(ABORT,'owned manual result refusal'); END;").unwrap();
    let path = root.path().join("owned-paid-result.json");
    let error = crate::task_engine::pipeline::persist_translated_claimed(
        &lease,
        &db,
        id,
        &payload,
        "owned-original",
        None,
        path.to_str().unwrap(),
        "/dev/null",
        Some(&scope),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("owned manual result refusal"));
    assert!(
        path.is_file(),
        "a complete paid-result orphan must survive projection failure"
    );
    db.lock()
        .await
        .execute_batch("DROP TRIGGER deny_manual_result;")
        .unwrap();
    drop(lease);
    let response = action(&state, id, "retranslate", b"").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        parse_http_json_body(&response)["data"]["translated_path"],
        path.to_str().unwrap()
    );
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .translated_path,
        path.to_str().unwrap()
    );
}

#[tokio::test]
async fn remaining_manual_frozen_request_survives_missing_source_offline_wp_and_lost_reply() {
    owned_frozen_manual_request(None, false).await;
}

#[tokio::test]
async fn remaining_manual_missing_frozen_proxy_cannot_fall_back_to_direct_provider() {
    owned_frozen_manual_request(Some("owned-missing-proxy".into()), false).await;
}

#[tokio::test]
async fn remaining_manual_frozen_proxy_works_without_current_configuration_and_replays() {
    owned_frozen_manual_request(Some("owned-frozen-proxy".into()), false).await;
}

#[tokio::test]
async fn remaining_manual_partial_fields_do_not_close_the_request_or_allow_a_new_fee_generation() {
    owned_frozen_manual_request(None, true).await;
}

async fn owned_frozen_manual_request(proxy_id: Option<String>, partial: bool) {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    // Failed-field fixtures must not arm another fixture's component circuit.
    let component_id = format!("owned-frozen-{}", uuid::Uuid::new_v4());
    let state = build_test_web_ui_state("", None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = format!("http://{}", listener.local_addr().unwrap());
    let proxy_port = listener.local_addr().unwrap().port();
    let use_frozen_proxy = proxy_id.as_deref() == Some("owned-frozen-proxy");
    let requests = Arc::new(AtomicUsize::new(0));
    let count = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 8192];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0, "owned request cannot end before its complete body");
                request.extend_from_slice(&chunk[..n]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let len = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    if request.len() >= end + 4 + len {
                        break;
                    }
                }
                assert!(request.len() < 65536);
            }
            assert!(String::from_utf8_lossy(&request).contains("owned-mock-pool-key"));
            if use_frozen_proxy {
                assert!(String::from_utf8_lossy(&request)
                    .starts_with("POST http://127.0.0.1:9/owned-provider "));
            }
            count.fetch_add(1, Ordering::SeqCst);
            let failed =
                partial && String::from_utf8_lossy(&request).contains("owned failed source");
            let body = if failed {
                r#"{"error":"owned definite refusal"}"#
            } else {
                r#"{"text":"owned frozen translation"}"#
            };
            let status = if failed { "403 Forbidden" } else { "200 OK" };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:9",
        "",
        "pending_review",
        "owned-unfinished",
    )
    .await;
    let db = state.lock().await.db.clone();
    db.lock()
        .await
        .execute(
            "UPDATE translation_items SET raw_path=?1,component_id=?2,component_ids_json=?3,selected_component_id=?2 WHERE id=?4",
            rusqlite::params![
                root.path().join("missing-source.json").to_str().unwrap(),
                component_id,
                serde_json::to_string(&vec![component_id.clone()]).unwrap(),
                id
            ],
        )
        .unwrap();
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    let template = serde_json::from_value(json!({
        "id":component_id,"name":"Owned frozen fixture","version":"1.0","type":"text",
        "request":{"method":"POST","url":if use_frozen_proxy { "http://127.0.0.1:9/owned-provider" } else { &provider },"headers":{"X-Owned-Key":"{{auth.api_key}}"},
            "body_type":"json","body":{"text":"{{input.text}}"}},"response":{"translated_text_path":"text"}
    })).unwrap();
    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        language_map: HashMap::new(),
        supported_business_lines: vec![],
        supported_content_formats: vec!["plain_text".into()],
        supported_formats: vec![],
        key_pool: Some(Arc::new(crate::component_rt::key_pool::KeyPool::new(
            vec![(
                "owned-manual-key".into(),
                HashMap::from([("api_key".into(), "owned-mock-pool-key".into())]),
                1,
                1,
            )],
            KeySelectionStrategy::RoundRobin,
        ))),
        oauth_pool: None,
        oauth_manager: None,
        proxy_profile_id: proxy_id.clone(),
        runtime_max_concurrent_requests: 1,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: Some(Arc::new(tokio::sync::Semaphore::new(1))),
        runtime_last_request_at: None,
    };
    let registry = ComponentRuntimeRegistry {
        runtimes: HashMap::from([(component_id.clone(), runtime)]),
        ordered_ids: vec![component_id],
    };
    let plan = crate::db::review_attempts::plan::ManualPlan {
        format:"manual-plan-v1".into(),wp_base:"http://127.0.0.1:9/wp-json/wptsall/v2/owned/client".into(),
        content:json!({"post_title":"owned frozen source","__wptsall_job_snapshot":{"source_revision":"owned-revision","policy_version":"owned-policy"}}),
        relation:serde_json::from_value(json!({"id":1,"source_lang":"en_US","target_lang":"zh_CN","target_site_type":"virtual","sync_mode":"one_way"})).unwrap(),
        rules:vec![serde_json::from_value(json!({"id":1,"model_id":1,"data_type":"post","object_name":"post",
            "translate_fields":["post_title"],"field_content_formats":{"post_title":"plain_text"},"field_storage_map":{"post_title":"post_column"}})).unwrap()],
        runtimes:crate::db::review_attempts::plan::ManualPlan::freeze_runtimes(&registry).await.unwrap(),
        ordered_ids:registry.ordered_ids,rule_bindings:Default::default(),task_type_bindings:Default::default(),
        proxy_profiles:if use_frozen_proxy { HashMap::from([("owned-frozen-proxy".into(),ProxyProfile {
            name:"owned".into(),protocol:"http".into(),host:"127.0.0.1".into(),port:proxy_port,
            username:String::new(),password:String::new(),enabled:true,
        })]) } else { HashMap::new() },
    };
    let mut plan = serde_json::to_value(plan).unwrap();
    if partial {
        plan["content"]["post_excerpt"] = json!("owned failed source");
        plan["rules"][0]["translate_fields"] = json!(["post_title", "post_excerpt"]);
        plan["rules"][0]["field_content_formats"]["post_excerpt"] = json!("plain_text");
        plan["rules"][0]["field_storage_map"]["post_excerpt"] = json!("post_column");
    }
    let lease = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    crate::db::review_attempts::scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &plan,
        "c1b05b00-4af3-4a5f-9d88-13b6dc674f11",
    )
    .unwrap();
    drop(lease);
    let content = action(&state, id, "content", b"").await;
    assert_eq!(
        parse_http_json_body(&content)["data"]["raw"]["post_title"],
        "owned frozen source",
        "review reload must show frozen inputs even after the source file disappears"
    );
    let response = action(&state, id, "retranslate", b"").await;
    let replay = action(&state, id, "retranslate", b"").await;
    server.abort();
    if partial {
        assert!(
            !response.starts_with("HTTP/1.1 200"),
            "partial manual work is not a completed request: {response}"
        );
        assert!(!replay.starts_with("HTTP/1.1 200"));
        assert_eq!(
            requests.load(Ordering::SeqCst),
            2,
            "only the original success and original refusal may submit"
        );
        assert!(crate::db::review_attempts::unfinished(&*db.lock().await, id).unwrap());
        assert!(crate::db::review_attempts::replay(
            &*db.lock().await,
            id,
            "c1b05b00-4af3-4a5f-9d88-13b6dc674f11"
        )
        .unwrap()
        .is_none());
        return;
    }
    if proxy_id.is_some() && !use_frozen_proxy {
        assert!(
            !response.starts_with("HTTP/1.1 200"),
            "missing frozen proxy must refuse, not silently send direct: {response}"
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "missing frozen proxy must have zero direct egress"
        );
        assert!(
            crate::db::review_attempts::pending_request(&*db.lock().await, id)
                .unwrap()
                .is_some()
        );
        return;
    }
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(replay.starts_with("HTTP/1.1 200"), "{replay}");
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "lost reply must replay saved result without a second paid POST"
    );
    let result = parse_http_json_body(&replay);
    assert_eq!(result["data"]["replayed"], true);
    let path = result["data"]["translated_path"].as_str().unwrap();
    let bytes = std::fs::read(path).unwrap();
    assert!(bytes.starts_with(b"WPTC"));
    assert!(!bytes
        .windows("owned frozen translation".len())
        .any(|window| window == b"owned frozen translation"));
    let saved: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(path)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["payload"]["translated_fields"]["post_title"],
        "owned frozen translation"
    );
    assert_eq!(
        saved["payload"]["client_task_id"],
        "manual-c1b05b00-4af3-4a5f-9d88-13b6dc674f11"
    );
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .status,
        "pending_review"
    );
    let content = action(&state, id, "content", b"").await;
    assert_eq!(
        parse_http_json_body(&content)["data"]["raw"]["post_title"],
        "owned frozen source"
    );
    assert_eq!(
        parse_http_json_body(&content)["data"]["translated"]["payload"]["translated_fields"]
            ["post_title"],
        "owned frozen translation",
        "a completed result must remain reviewable without the original source file"
    );
}

#[tokio::test]
async fn remaining_manual_unfinished_request_cannot_be_rejected_or_batch_rejected() {
    let _env = components_env_lock().lock().unwrap();
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:9",
        "",
        "pending_review",
        "owned",
    )
    .await;
    let db = state.lock().await.db.clone();
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    let lease = crate::db::unit_lock::UnitLease::item(&db, id)
        .await
        .unwrap();
    crate::db::review_attempts::scope(
        &*db.lock().await,
        &db,
        &lease,
        &item,
        &json!({"owned":"frozen"}),
        "c1b05b00-4af3-4a5f-9d88-13b6dc674f11",
    )
    .unwrap();
    drop(lease);
    let rejected = action(&state, id, "reject", br#"{"reason":"owned"}"#).await;
    assert!(
        rejected.starts_with("HTTP/1.1 409"),
        "unfinished request cannot change intent: {rejected}"
    );
    let rejected = action(
        &state,
        id,
        "batch-reject",
        json!({"ids":[id],"reason":"owned"}).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        parse_http_json_body(&rejected)["data"]["rejected"],
        json!([])
    );
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, id)
            .unwrap()
            .unwrap()
            .status,
        "pending_review"
    );
}
