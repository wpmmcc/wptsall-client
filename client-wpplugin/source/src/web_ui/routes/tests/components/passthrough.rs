use super::*;

#[tokio::test]
async fn components_template_preserves_component_download_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body = r#"{"success":false,"error":{"code":"COMPONENT_DISABLED","message":"Component template is disabled"}}"#;
        let response = format!(
            "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.components = vec![make_test_server_component(
            "official-test-image-v1",
            "image_translation",
            "image",
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
    handle_components_template(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({"component_id":"official-test-text-v1"}))
            .unwrap()
            .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "component template endpoint should preserve upstream component status, got response: {}",
        response
    );
    assert!(
        response.contains("\"COMPONENT_DISABLED\""),
        "component template endpoint should preserve upstream component error code: {}",
        response
    );
}

#[tokio::test]
async fn local_component_test_preserves_component_download_error_details() {
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body = r#"{"success":false,"error":{"code":"COMPONENT_DISABLED","message":"Component template is disabled"}}"#;
        let response = format!(
            "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.component_bindings.components.insert(
            "test-download-binding".to_string(),
            ComponentBindingEntry {
                template_id: Some("official-test-text-v1".to_string()),
                ..Default::default()
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
    handle_local_component_test(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "component_key": "test-download-binding",
            "text": "Hello world",
            "source_lang": "en_US",
            "target_lang": "zh_CN"
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "component test endpoint should preserve upstream component status, got response: {}",
        response
    );
    assert!(
        response.contains("\"COMPONENT_DISABLED\""),
        "component test endpoint should preserve upstream component error code: {}",
        response
    );
}

#[tokio::test]
async fn local_component_create_preserves_snapshot_download_error_details() {
    // P0-LF-03 5.5: implicit server snapshot download on the create path is
    // LEGACY behaviour; pin the gate explicitly (local mode serves local
    // data only and returns COMPONENT_TEMPLATE_REQUIRED instead).
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body = r#"{"success":false,"error":{"code":"COMPONENT_DISABLED","message":"Component template is disabled"}}"#;
        let response = format!(
            "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    handle_local_component_create_v2(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "id": "local-test-component-download",
            "name": "Local Test Component",
            "template_id": "official-test-text-v1",
            "kind": "text"
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "local component create should preserve upstream snapshot download status, got response: {}",
        response
    );
    assert!(
        response.contains("\"COMPONENT_DISABLED\""),
        "local component create should preserve upstream snapshot download error code: {}",
        response
    );
}

#[tokio::test]
async fn domain_tokens_test_preserves_domains_refresh_auth_errors() {
    // P0-LF-03 5.4: recovering a missing route_secret by refreshing website
    // domains is LEGACY behaviour and must run with the server control plane
    // explicitly enabled (local mode resolves local state only).
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
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
        guard.domain_token_bindings.domains.insert(
            "https://example.com".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: String::new(),
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
    handle_domain_tokens_test(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "api_base_url": "https://example.com"
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "domain token test should preserve domains refresh auth status instead of collapsing into route_secret error: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "domain token test should preserve server domains refresh error code: {}",
        response
    );
}

#[tokio::test]
async fn domain_tokens_test_preserves_wp_validate_token_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body = r#"{"code":"wptsall_pro_required","message":"Client API disabled"}"#;
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let domain = format!("http://{}", upstream_addr);
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            domain.clone(),
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
    handle_domain_tokens_test(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "api_base_url": domain
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 403 Forbidden"),
        "domain token test should preserve WP validate-token status, got response: {}",
        response
    );
    assert_eq!(
        payload["error"]["code"], "wptsall_pro_required",
        "domain token test should preserve WP validate-token error code: {}",
        response
    );
}

#[tokio::test]
async fn domain_tokens_test_falls_back_to_ping_when_validate_token_route_is_missing() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let upstream_task = tokio::spawn(async move {
        let (mut validate_socket, _) = upstream.accept().await.unwrap();
        let mut validate_request = [0u8; 4096];
        let validate_len = validate_socket.read(&mut validate_request).await.unwrap();
        let validate_text = String::from_utf8_lossy(&validate_request[..validate_len]);
        let validate_line = validate_text.lines().next().unwrap_or_default().to_string();
        let validate_body = r#"{"code":"rest_no_route","message":"No route was found matching the URL and request method."}"#;
        let validate_response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            validate_body.len(),
            validate_body
        );
        validate_socket
            .write_all(validate_response.as_bytes())
            .await
            .unwrap();

        let (mut ping_socket, _) = upstream.accept().await.unwrap();
        let mut ping_request = [0u8; 4096];
        let ping_len = ping_socket.read(&mut ping_request).await.unwrap();
        let ping_text = String::from_utf8_lossy(&ping_request[..ping_len]);
        let ping_line = ping_text.lines().next().unwrap_or_default().to_string();
        let ping_body = r#"{"success":true,"data":{"sync_execution_mode":"local","local_executor_enabled":true}}"#;
        let ping_sig = crate::web_ui::test_support::sign_wp_plaintext_response(
            "wp_token_test",
            ping_body.as_bytes(),
        );
        let ping_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            ping_sig,
            ping_body.len(),
            ping_body
        );
        ping_socket
            .write_all(ping_response.as_bytes())
            .await
            .unwrap();
        (validate_line, ping_line)
    });

    let domain = format!("http://{}", upstream_addr);
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            domain.clone(),
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
    handle_domain_tokens_test(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "api_base_url": domain
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let (validate_line, ping_line) = upstream_task.await.unwrap();
    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        validate_line.starts_with("GET /wp-json/wptsall/v2/route_test/client/validate-token"),
        "expected validate-token request first, got: {}",
        validate_line
    );
    assert!(
        ping_line.starts_with("GET /wp-json/wptsall/v2/route_test/client/ping"),
        "expected ping request after validate-token fallback, got: {}",
        ping_line
    );
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "domain token test should succeed via ping fallback, got response: {}",
        response
    );
    assert_eq!(
        payload["success"], true,
        "expected success payload: {}",
        response
    );
    assert_eq!(
        payload["data"]["validated_via"], "ping",
        "domain token test should report ping fallback: {}",
        response
    );
}

#[tokio::test]
async fn local_component_file_test_preserves_snapshot_download_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body = r#"{"success":false,"error":{"code":"COMPONENT_DISABLED","message":"Component template is disabled"}}"#;
        let response = format!(
            "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let _env_guard = components_env_lock().lock().unwrap();
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let components_path = format!(
        "/tmp/wptsall-test-components-file-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_path_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-file-component": {
                "name": "Local File Component",
                "template_id": "official-test-image-v1",
                "source_template_id": "official-test-image-v1",
                "kind": "image",
                "enabled": true,
                "created_at": "1",
                "versions": {}
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &local_doc).unwrap();

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
    handle_local_component_test_file(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "component_id": "local-file-component",
            "file_url": "https://example.com/file.jpg",
            "source_lang": "en_US",
            "target_lang": "zh_CN"
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let _ = std::fs::remove_file(&components_path);

    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "component file test endpoint should preserve upstream snapshot download status, got response: {}",
        response
    );
    assert!(
        response.contains("\"COMPONENT_DISABLED\""),
        "component file test endpoint should preserve upstream snapshot download error code: {}",
        response
    );
}

#[tokio::test]
async fn component_version_test_preserves_snapshot_download_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body = r#"{"success":false,"error":{"code":"COMPONENT_DISABLED","message":"Component template is disabled"}}"#;
        let response = format!(
            "HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let _env_guard = components_env_lock().lock().unwrap();
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let components_path = format!(
        "/tmp/wptsall-test-components-version-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_path_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-version-component": {
                "name": "Local Version Component",
                "template_id": "official-test-text-v1",
                "source_template_id": "official-test-text-v1",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {
                    "v1": {
                        "version": "v1",
                        "remarks": "",
                        "key_ids": [],
                        "key_selection_strategy": "first",
                        "auth_type": "key",
                        "created_at": "1"
                    }
                }
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &local_doc).unwrap();

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
    handle_component_version_test(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "text": "Hello world",
            "source_lang": "en_US",
            "target_lang": "zh_CN"
        }))
        .unwrap()
        .as_slice(),
        "local-version-component",
        "v1",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let _ = std::fs::remove_file(&components_path);

    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "component version test endpoint should preserve upstream snapshot download status, got response: {}",
        response
    );
    assert!(
        response.contains("\"COMPONENT_DISABLED\""),
        "component version test endpoint should preserve upstream snapshot download error code: {}",
        response
    );
}
