use super::*;

#[tokio::test]
async fn worker_run_once_preserves_structured_server_auth_errors() {
    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body =
            r#"{"success":false,"error":{"code":"SESSION_EXPIRED","message":"Session expired"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            format!("http://{}", upstream_addr),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: "route_test".to_string(),
            },
        );
    }

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_run_once(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-run-once-auth-error-test.log",
        b"{}",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "worker run-once should preserve upstream auth status instead of rewriting it as WORKER_RUN_FAILED: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "worker run-once should preserve upstream auth error code: {}",
        response
    );
}

#[tokio::test]
async fn worker_start_check_allows_default_local_mode_without_server_session() {
    // Other worker tests explicitly exercise the legacy control-plane lane;
    // keep this local-first assertion isolated from process-global env state.
    let _local_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "false".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);
    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-local-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "default local worker preflight should not require server session: {}",
        response
    );
    assert!(
        response.contains("\"can_start\":true"),
        "default local worker preflight should allow start: {}",
        response
    );
}

#[tokio::test]
async fn worker_start_requires_server_session() {
    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            "https://example.com".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: "route_test".to_string(),
            },
        );
    }
    let runtime_control = WebUiRuntimeControl::new();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start(
        &mut socket,
        &state,
        &runtime_control,
        "/tmp/wptsall-worker-start-test.log",
        b"{}",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "worker start should reject missing server session before spawning loop: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_REQUIRED\""),
        "worker start should report explicit session requirement: {}",
        response
    );
    assert!(
        !runtime_control.worker_running.load(Ordering::SeqCst),
        "worker loop must remain stopped when start is rejected"
    );
}

#[tokio::test]
async fn worker_config_partial_update_preserves_existing_poll_seconds() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let db_arc = {
        let mut guard = state.lock().await;
        guard.worker_loop_poll_seconds = 45;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::system::set_system_config(&conn, "worker_poll_seconds", "45").unwrap();
    }

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_config(&mut socket, &state, br#"{"review_mode":true}"#)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let body = parse_http_json_body(&response);
    assert_eq!(body["success"], serde_json::json!(true));
    assert_eq!(body["data"]["poll_seconds"], serde_json::json!(45));

    {
        let guard = state.lock().await;
        assert_eq!(
            guard.worker_loop_poll_seconds, 45,
            "partial worker config updates must not reset poll_seconds"
        );
    }

    let conn = db_arc.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "worker_poll_seconds"),
        Some("45".to_string())
    );
    assert_eq!(
        crate::db::system::get_system_config(&conn, "review_mode"),
        Some("true".to_string())
    );
}

// ---- preflight hang regressions (2026-09-08 hazard fix) ----
//
// The worker preflight must never hang forever: the component registry load
// is bounded and degrades, and the whole preflight collection is bounded and
// answers with a structured 504. Black-hole servers (accept, never respond)
// reproduce the stalled-catalog / stalled-endpoint conditions.

async fn spawn_blackhole_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hold = tokio::spawn(async move {
        // Accept every connection and keep it alive: never read, never
        // respond. Dropping the connection would let the client error out
        // immediately instead of stalling.
        let mut held: Vec<tokio::net::TcpStream> = Vec::new();
        while let Ok((conn, _)) = listener.accept().await {
            held.push(conn);
        }
    });
    (format!("http://{}", addr), hold)
}

fn seed_preflight_local_docs(component: serde_json::Value) -> (String, String) {
    let db_path = format!(
        "/tmp/wptsall-preflight-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let components_path = format!(
        "/tmp/wptsall-preflight-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let doc: crate::types::ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-preflight-stall": component
        }
    }))
    .unwrap();
    {
        let conn = crate::db::open_db(&db_path).unwrap();
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::components::save_local_components_doc(&conn, &doc).unwrap();
    }
    std::fs::write(&components_path, serde_json::to_vec(&doc).unwrap()).unwrap();
    (db_path, components_path)
}

#[tokio::test]
async fn worker_start_check_degrades_when_component_catalog_stalls() {
    let _env_scope = components_env_lock().lock().unwrap();
    let _server_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let _component_timeout_guard =
        EnvVarGuard::set("WPTSALL_PREFLIGHT_COMPONENT_TIMEOUT_SECS", "1".to_string());
    let _skip_sig_guard =
        EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());

    // A local component that still needs a server-side template alias
    // download forces the loader to contact the (stalled) catalog.
    let (db_path, components_path) = seed_preflight_local_docs(json!({
        "name": "Stalled Template",
        "template_id": "stalled-template-v1",
        "vendor_id": "local-vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {}
    }));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let (server_base, hold) = spawn_blackhole_server().await;
    let state = build_test_web_ui_state(&server_base, Some("sess_stall_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&mut response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-stall-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    hold.abort();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "stalled component catalog must degrade preflight instead of hanging: {}",
        response
    );
    assert!(
        response.contains("\"can_start\":true"),
        "degraded preflight should default to allow-start: {}",
        response
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn worker_start_check_returns_504_when_preflight_exceeds_total_bound() {
    let _env_scope = components_env_lock().lock().unwrap();
    let _server_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let _total_timeout_guard =
        EnvVarGuard::set("WPTSALL_PREFLIGHT_TIMEOUT_SECS", "1".to_string());

    // Inline local template: the catalog fetch is skipped entirely, so the
    // registry builds locally; the server-mode domain list fetch then stalls
    // on the black-hole server and the total bound must answer with a 504.
    let (db_path, components_path) = seed_preflight_local_docs(json!({
        "name": "Inline Local",
        "template_id": "",
        "vendor_id": "local-vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {},
        "template_json": {
            "id": "inline-template",
            "name": "Inline Template",
            "version": "1.0.0",
            "type": "text",
            "auth": null,
            "request": {
                "method": "POST",
                "url": "http://127.0.0.1:1/translate",
                "headers": null,
                "body": {"text": "{{input.text}}"}
            },
            "response": {
                "translated_text_path": "data.text",
                "error_path": null,
                "translated_ref_path": null,
                "translated_media_ref_path": null,
                "translated_image_ref_path": null,
                "translated_video_ref_path": null,
                "translated_audio_ref_path": null,
                "translated_document_ref_path": null
            }
        }
    }));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let (server_base, hold) = spawn_blackhole_server().await;
    let state = build_test_web_ui_state(&server_base, Some("sess_total_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&mut response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-total-timeout-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    hold.abort();

    assert!(
        response.starts_with("HTTP/1.1 504 Gateway Timeout"),
        "preflight exceeding the total bound must answer with a structured 504: {}",
        response
    );
    assert!(
        response.contains("\"WORKER_START_PREFLIGHT_TIMEOUT\""),
        "preflight timeout must carry a stable error code: {}",
        response
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn worker_start_returns_504_when_preflight_exceeds_total_bound() {
    let _env_scope = components_env_lock().lock().unwrap();
    let _server_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let _total_timeout_guard =
        EnvVarGuard::set("WPTSALL_PREFLIGHT_TIMEOUT_SECS", "1".to_string());
    let _wp_token_guard =
        EnvVarGuard::set("WPTSALL_WP_CLIENT_TOKEN", "wp_token_test".to_string());

    let (db_path, components_path) = seed_preflight_local_docs(json!({
        "name": "Inline Local",
        "template_id": "",
        "vendor_id": "local-vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {},
        "template_json": {
            "id": "inline-template",
            "name": "Inline Template",
            "version": "1.0.0",
            "type": "text",
            "auth": null,
            "request": {
                "method": "POST",
                "url": "http://127.0.0.1:1/translate",
                "headers": null,
                "body": {"text": "{{input.text}}"}
            },
            "response": {
                "translated_text_path": "data.text",
                "error_path": null,
                "translated_ref_path": null,
                "translated_media_ref_path": null,
                "translated_image_ref_path": null,
                "translated_video_ref_path": null,
                "translated_audio_ref_path": null,
                "translated_document_ref_path": null
            }
        }
    }));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let (server_base, hold) = spawn_blackhole_server().await;
    let state = build_test_web_ui_state(&server_base, Some("sess_total_test"));
    let runtime_control = WebUiRuntimeControl::new();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&mut response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start(
        &mut socket,
        &state,
        &runtime_control,
        "/tmp/wptsall-worker-start-total-timeout-test.log",
        b"{}",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    hold.abort();

    assert!(
        response.starts_with("HTTP/1.1 504 Gateway Timeout"),
        "worker start preflight timeout must answer with a structured 504: {}",
        response
    );
    assert!(
        response.contains("\"WORKER_START_PREFLIGHT_TIMEOUT\""),
        "worker start timeout must carry a stable error code: {}",
        response
    );
    assert!(
        !runtime_control.worker_running.load(std::sync::atomic::Ordering::SeqCst),
        "worker loop must remain stopped when preflight times out"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}
