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
