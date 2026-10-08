use super::*;

#[tokio::test]
async fn server_components_search_preserves_rate_limit_status() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body =
            r#"{"success":false,"error":{"code":"RATE_LIMITED","message":"Too many requests"}}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
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
    handle_server_components_search(&mut socket, &state, "")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 429 Too Many Requests"),
        "RATE_LIMITED should be preserved as 429 for Web UI control-plane callers, got response: {}",
        response
    );
    assert!(
        response.contains("\"RATE_LIMITED\""),
        "response should preserve server error code: {}",
        response
    );
}

#[tokio::test]
async fn server_components_search_forwards_locale_header() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let captured = Arc::new(tokio::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let size = socket.read(&mut request).await.unwrap();
        *captured_clone.lock().await = String::from_utf8_lossy(&request[..size]).to_string();
        let body = r#"{"success":true,"data":{"items":[],"page":1,"per_page":20,"total":0,"total_pages":1}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
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
    handle_server_components_search(&mut socket, &state, "locale=zh-CN")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected successful downstream response, got: {}",
        response
    );
    let request = captured.lock().await.clone();
    let request_lower = request.to_ascii_lowercase();
    assert!(
        request_lower.contains("x-locale: zh-cn\r\n"),
        "server components search should forward locale header, got request: {}",
        request
    );
}

#[tokio::test]
async fn domains_refresh_preserves_structured_server_auth_errors() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
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
    handle_domains_refresh(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "domains refresh should preserve structured server auth status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "domains refresh should preserve structured server error code: {}",
        response
    );
}
#[tokio::test]
async fn components_refresh_preserves_structured_server_auth_errors() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
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
    handle_components_refresh(&mut socket, &state)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "components refresh should preserve structured server auth status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "components refresh should preserve structured server error code: {}",
        response
    );
}
#[tokio::test]
async fn vendors_list_preserves_structured_server_auth_errors() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
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
        guard.components = vec![make_test_server_component(
            "official-test-text-v1",
            "text_translation",
            "text",
        )];
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
    handle_vendors_list(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "vendors list should preserve structured server auth status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "vendors list should preserve structured server error code: {}",
        response
    );
}
#[tokio::test]
async fn fetch_signing_key_from_server_preserves_structured_auth_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 2048];
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

    let client = Client::builder().no_proxy().build().unwrap();
    let err = fetch_signing_key_from_server(
        &client,
        &format!("http://{}", addr),
        "sess-test-signing-key",
    )
    .await
    .expect_err("structured auth errors should currently fail");
    let err_text = format!("{:#}", err);

    assert!(
        err_text.contains("SESSION_EXPIRED"),
        "expected structured server error code to survive signing-key fetch failure: {}",
        err_text
    );
    assert!(
        !err_text.contains("invalid json response"),
        "structured signing-key errors should not be collapsed into invalid json response: {}",
        err_text
    );
}
#[tokio::test]
async fn oauth_callback_reports_success_when_token_and_bootstrap_succeed() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..3 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..read]);
            let first_line = request_text.lines().next().unwrap_or_default();
            let body = if first_line.starts_with("POST /api/v1/oauth/token") {
                r#"{"session_token":"sess_success","expires_in":3600}"#.to_string()
            } else if first_line.starts_with("GET /api/v1/client/domains") {
                r#"{"items":[{"domain":"blog.wpmm.cc","api_base_url":"https://blog.wpmm.cc","site_status":"active","max_relations":2,"route_secret":"abc123"}]}"#.to_string()
            } else if first_line.starts_with("GET /api/v1/client/components?status=active") {
                r#"{"items":[],"page":1,"per_page":200,"total":0,"total_pages":1}"#.to_string()
            } else {
                format!("{{\"unexpected_request\":{:?}}}", first_line)
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), None);
    {
        let mut guard = state.lock().await;
        guard.oauth_code_verifier = Some("verifier_test".to_string());
        guard.oauth_state = Some("state_test".to_string());
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
    handle_oauth_callback(
        &mut socket,
        &state,
        "code=test_code&state=state_test",
        "/tmp/wptsall-oauth-callback-success-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "oauth callback should return success when token + bootstrap succeed: {}",
        response
    );
    assert_eq!(guard.session_token.as_deref(), Some("sess_success"));
    assert_eq!(guard.domains.len(), 1);
}

#[tokio::test]
async fn oauth_callback_rejects_when_bootstrap_domains_fail() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..3 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..read]);
            let first_line = request_text.lines().next().unwrap_or_default();
            let (status, body) = if first_line.starts_with("POST /api/v1/oauth/token") {
                (
                    "200 OK",
                    r#"{"session_token":"sess_bootstrap","expires_in":3600}"#.to_string(),
                )
            } else if first_line.starts_with("GET /api/v1/client/domains") {
                (
                    "401 Unauthorized",
                    r#"{"success":false,"error":{"code":"SESSION_EXPIRED","message":"Session expired"}}"#
                        .to_string(),
                )
            } else if first_line.starts_with("GET /api/v1/client/components?status=active") {
                (
                    "200 OK",
                    r#"{"success":true,"data":{"items":[],"page":1,"per_page":200,"total":0,"total_pages":1}}"#
                        .to_string(),
                )
            } else {
                (
                    "500 Internal Server Error",
                    format!("{{\"unexpected_request\":{:?}}}", first_line),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), None);
    {
        let mut guard = state.lock().await;
        guard.oauth_code_verifier = Some("verifier_test".to_string());
        guard.oauth_state = Some("state_test".to_string());
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
    handle_oauth_callback(
        &mut socket,
        &state,
        "code=test_code&state=state_test",
        "/tmp/wptsall-oauth-callback-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "oauth callback should keep session alive when domains bootstrap fails: {}",
        response
    );
    assert!(
        guard.session_token.is_some(),
        "oauth callback should keep session_token when bootstrap domains fetch fails"
    );
    assert!(
        guard.domains.is_empty(),
        "domains bootstrap failure should leave domains empty"
    );
    assert!(
        guard.last_event == "oauth.login_partial",
        "oauth callback should record partial login event, got: {}",
        guard.last_event
    );
    assert!(
        !guard.last_error.is_empty(),
        "oauth callback should preserve bootstrap domains warning"
    );
}

#[tokio::test]
async fn oauth_callback_keeps_success_when_bootstrap_components_fail() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..3 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..read]);
            let first_line = request_text.lines().next().unwrap_or_default();
            let (status, body) = if first_line.starts_with("POST /api/v1/oauth/token") {
                (
                    "200 OK",
                    r#"{"session_token":"sess_components_partial","expires_in":3600}"#.to_string(),
                )
            } else if first_line.starts_with("GET /api/v1/client/domains") {
                (
                    "200 OK",
                    r#"{"items":[{"domain":"blog.wpmm.cc","api_base_url":"https://blog.wpmm.cc","site_status":"active","max_relations":2,"route_secret":"abc123"}]}"#.to_string(),
                )
            } else if first_line.starts_with("GET /api/v1/client/components?status=active") {
                (
                    "500 Internal Server Error",
                    r#"{"success":false,"error":{"code":"COMPONENTS_TEMP_UNAVAILABLE","message":"temporary component failure"}}"#
                        .to_string(),
                )
            } else {
                (
                    "500 Internal Server Error",
                    format!("{{\"unexpected_request\":{:?}}}", first_line),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), None);
    {
        let mut guard = state.lock().await;
        guard.oauth_code_verifier = Some("verifier_test".to_string());
        guard.oauth_state = Some("state_test".to_string());
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
    handle_oauth_callback(
        &mut socket,
        &state,
        "code=test_code&state=state_test",
        "/tmp/wptsall-oauth-callback-components-warning-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "oauth callback should still report success when components bootstrap fails: {}",
        response
    );
    assert_eq!(
        guard.session_token.as_deref(),
        Some("sess_components_partial")
    );
    assert_eq!(guard.domains.len(), 1);
    assert!(guard.components.is_empty());
    assert_eq!(guard.last_event, "oauth.login_partial");
    assert!(
        guard.last_error.contains("COMPONENTS_TEMP_UNAVAILABLE"),
        "components bootstrap failure should be surfaced in state error, got: {}",
        guard.last_error
    );
}
#[tokio::test]
async fn oauth_callback_preserves_structured_token_exchange_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let request_text = String::from_utf8_lossy(&request[..read]);
        let first_line = request_text.lines().next().unwrap_or_default();
        assert!(
            first_line.starts_with("POST /api/v1/oauth/token"),
            "expected oauth token exchange request, got: {}",
            first_line
        );

        let body = r#"{"success":false,"error":{"code":"INVALID_CODE","message":"Authorization code is invalid"}}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), None);
    {
        let mut guard = state.lock().await;
        guard.oauth_code_verifier = Some("verifier_test".to_string());
        guard.oauth_state = Some("state_test".to_string());
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
    handle_oauth_callback(
        &mut socket,
        &state,
        "code=test_code&state=state_test",
        "/tmp/wptsall-oauth-callback-token-error-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "oauth callback should return token exchange failure as 400, got response: {}",
        response
    );
    assert!(
        response.contains("INVALID_CODE"),
        "oauth callback should preserve structured token exchange error code in HTML response: {}",
        response
    );
    assert!(
        guard.session_token.is_none(),
        "oauth callback must not persist session_token when token exchange fails"
    );
}
#[tokio::test]
async fn logout_must_not_report_success_when_server_invalidation_fails() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body =
            r#"{"success":false,"error":{"code":"LOCK_ERROR","message":"Store lock poisoned"}}"#;
        let response = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let _env_guard = components_env_lock().lock().unwrap();
    let db_path = format!("/tmp/wptsall-logout-test-{}.db", uuid::Uuid::new_v4());
    let _db_path_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
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
    handle_logout(&mut socket, &state, &runtime_control)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;
    let _ = std::fs::remove_file(&db_path);

    assert!(
        !response.starts_with("HTTP/1.1 200 OK"),
        "logout should not report success when upstream session invalidation fails: {}",
        response
    );
    assert!(
        response.contains("LOCK_ERROR"),
        "logout should preserve upstream structured error details: {}",
        response
    );
    assert_eq!(
        guard.session_token.as_deref(),
        Some("sess_test"),
        "local session should be preserved when upstream logout fails"
    );
}
