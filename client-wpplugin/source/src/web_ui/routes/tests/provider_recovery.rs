use super::*;
use crate::web_ui::test_support::WebUiTestHarness;
use std::collections::HashMap;

const OWNED_SITE: &str = "https://owned-wp.example";
const OWNED_ROUTE: &str = "owned-private-route";

fn runtime(base: &str) -> ComponentRuntime {
    ComponentRuntime {
        template: serde_json::from_value(json!({
            "id":"owned-provider", "name":"Owned", "version":"1", "type":"text_translation",
            "request":{"method":"POST","url":format!("{base}/submit"),
                "body":{"reference":"{{operation.attempt_id}}","binding":"{{operation.binding}}" }},
            "response":{"translated_text_path":"result"},
            "async_poll":{"job_id_path":"job_id","request":{"method":"GET","url":format!("{base}/poll/{{{{computed.job_id}}}}")},
                "reconcile":{"request":{"method":"GET","url":format!("{base}/evidence/{{{{operation.attempt_id}}}}"),
                    "headers":{"Authorization":"Bearer {{auth.token}}" }, "body_type":"none"},
                    "matches_path":"matches","attempt_id_path":"reference","binding_path":"binding",
                    "job_id_path":"job_id","status_path":"status","resumable_values":["running","done"]}}
        })).unwrap(),
        auth_values: HashMap::new(), supported_business_lines: vec![],
        language_map: HashMap::new(), supported_content_formats: vec![], supported_formats: vec![],
        key_pool: None, oauth_pool: None, oauth_manager: None, proxy_profile_id: None,
        runtime_max_concurrent_requests: 0, runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None, runtime_last_request_at: None,
    }
}

async fn seed(state: &Arc<Mutex<WebUiState>>, base: &str) -> (String, String) {
    let db = Arc::clone(&state.lock().await.db);
    let domain = format!("{OWNED_SITE}/wp-json/wptsall/v2/{OWNED_ROUTE}/client");
    let env = crate::db::async_jobs::AsyncJobEnv {
        db: Arc::clone(&db),
        domain,
        relation_id: 33,
        object_type: "post_type".into(),
        object_id: 77,
        field_name: "owned-field".into(),
        chunk_index: 0,
        lane: "text",
        source_snapshot: Some(json!({"revision":"owned-private-revision"})),
        resume_binding: None,
    };
    let runtime = runtime(base);
    let bound = env
        .for_runtime(&runtime, &json!("owned-private-source"), "en", "zh")
        .unwrap();
    let mut ctx = HashMap::from([("auth.token".into(), "owned-original-secret".into())]);
    crate::db::async_jobs::test_writes::begin_provider_runtime_intent(
        &bound,
        &runtime,
        &json!("owned-private-source"),
        &mut ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    crate::db::async_jobs::test_writes::commit_provider_submit_context(&bound, &ctx)
        .await
        .unwrap();
    (
        ctx["operation.attempt_id"].clone(),
        ctx["operation.binding"].clone(),
    )
}

async fn bind_site(state: &Arc<Mutex<WebUiState>>) {
    state.lock().await.domain_token_bindings.domains.insert(
        OWNED_SITE.into(),
        DomainTokenBindingEntry {
            wp_client_token: "owned-current-wp-token".into(),
            route_secret: OWNED_ROUTE.into(),
            ..Default::default()
        },
    );
}

#[tokio::test]
async fn provider_recovery_router_lists_redacted_evidence_and_applies_security_guards() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let (id, _) = seed(&harness.state, "http://127.0.0.1:1").await;
    let list = harness.get_json("/api/provider-recovery").await.unwrap();
    assert!(list.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(list.body["data"]["items"][0]["operation_id"], id);
    assert_eq!(list.body["data"]["items"][0]["site"], OWNED_SITE);
    assert_eq!(list.body["data"]["items"][0]["can_reconcile"], true);
    for private in [
        OWNED_ROUTE,
        "owned-private-source",
        "owned-original-secret",
        "owned-private-revision",
        "/submit",
        "binding",
        "\"ctx\"",
    ] {
        assert!(!list.raw_body.contains(private), "redacted field {private}");
    }
    let body = serde_json::to_vec(&json!({"operation_id":id})).unwrap();
    let (status, raw) = harness
        .send_raw_request(
            "POST",
            "/api/provider-recovery/reconcile",
            Some(&body),
            &[("Origin", "https://owned-attacker.invalid")],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 403") && raw.contains("CSRF_REJECTED"));
    let (status, raw) = harness
        .send_raw_request_without_host(
            "GET",
            "/api/provider-recovery",
            None,
            &[("Host", "owned-attacker.invalid")],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 403") && raw.contains("WEBUI_HOST_REJECTED"));
    let external = harness
        .post_json(
            "/api/access-control",
            json!({"external_access":true,"allowed_ips":["192.0.2.41"]}),
        )
        .await
        .unwrap();
    assert!(external.status_line.starts_with("HTTP/1.1 200"));
    let token = external.body["data"]["access_token"].as_str().unwrap();
    assert!(harness
        .get_json("/api/provider-recovery")
        .await
        .unwrap()
        .status_line
        .starts_with("HTTP/1.1 401"));
    let (status, raw) = harness
        .send_raw_request(
            "POST",
            "/api/provider-recovery/reconcile",
            Some(&body),
            &[("Origin", "http://127.0.0.1:8977")],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 401") && raw.contains("WEBUI_TOKEN_REQUIRED"));
    let (status, _) = harness
        .send_raw_request(
            "GET",
            "/api/provider-recovery",
            None,
            &[("X-WPTSALL-WebUI-Token", token)],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 200"));
    let db = Arc::clone(&harness.state.lock().await.db);
    assert_eq!(
        crate::db::async_jobs::list_provider_operations(&db)
            .await
            .unwrap()[0]
            .state,
        "submit_unknown"
    );
}

#[tokio::test]
async fn provider_recovery_router_refuses_unbound_active_worker_and_arbitrary_job_or_proof() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let (id, _) = seed(&harness.state, "http://127.0.0.1:1").await;
    let unbound = harness
        .post_json(
            "/api/provider-recovery/reconcile",
            json!({"operation_id":id}),
        )
        .await
        .unwrap();
    assert!(unbound.status_line.starts_with("HTTP/1.1 409"));
    bind_site(&harness.state).await;
    harness.state.lock().await.worker_loop_running = true;
    let active = harness
        .post_json(
            "/api/provider-recovery/reconcile",
            json!({"operation_id":id}),
        )
        .await
        .unwrap();
    assert!(active.status_line.starts_with("HTTP/1.1 409"));
    harness.state.lock().await.worker_loop_running = false;
    for body in [
        json!({"operation_id":id,"job_id":"caller-chosen-job"}),
        json!({"operation_id":id,"proof":{"matches":[]}}),
        json!({"operation_id":"00000000-0000-0000-0000-000000000000"}),
    ] {
        let refusal = harness
            .post_json("/api/provider-recovery/reconcile", body)
            .await
            .unwrap();
        assert!(refusal.status_line.starts_with("HTTP/1.1 409"));
        assert_eq!(refusal.body["error"]["code"], "PROVIDER_REVIEW_REQUIRED");
    }
    let db = Arc::clone(&harness.state.lock().await.db);
    assert_eq!(
        crate::db::async_jobs::list_provider_operations(&db)
            .await
            .unwrap()[0]
            .state,
        "submit_unknown"
    );
}

#[tokio::test]
async fn provider_recovery_router_saves_unique_existing_job_without_submit_or_poll() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", upstream.local_addr().unwrap());
    let (id, binding) = seed(&harness.state, &base).await;
    bind_site(&harness.state).await;
    let expected = id.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut header = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            header.extend_from_slice(&buffer[..n]);
            if header.windows(4).any(|b| b == b"\r\n\r\n") {
                break;
            }
        }
        let header = String::from_utf8(header).unwrap();
        assert!(header.starts_with(&format!("GET /evidence/{expected} ")));
        assert!(header
            .to_ascii_lowercase()
            .contains("authorization: bearer owned-original-secret"));
        let body = json!({"matches":[{"reference":expected,"binding":binding,"job_id":"owned-job-77","status":"done"}]}).to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), upstream.accept())
                .await
                .is_err(),
            "review must not submit or poll"
        );
    });
    let result = harness
        .post_json(
            "/api/provider-recovery/reconcile",
            json!({"operation_id":id}),
        )
        .await
        .unwrap();
    assert!(result.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(result.body["data"]["state"], "polling");
    server.await.unwrap();
    let db = Arc::clone(&harness.state.lock().await.db);
    assert_eq!(
        crate::db::async_jobs::list_provider_operations(&db)
            .await
            .unwrap()[0]
            .state,
        "polling"
    );
}

#[tokio::test]
async fn provider_recovery_router_reports_corruption_instead_of_empty_success() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let db = Arc::clone(&harness.state.lock().await.db);
    db.lock().await.execute("INSERT INTO system_config(key,value) VALUES('provider-operation-v1:owned-damaged','{}')",[]).unwrap();
    let list = harness.get_json("/api/provider-recovery").await.unwrap();
    assert!(list.status_line.starts_with("HTTP/1.1 409"));
    assert_eq!(list.body["error"]["code"], "PROVIDER_RECOVERY_UNREADABLE");
}
