use super::*;

#[tokio::test]
async fn components_template_preserves_signing_key_fetch_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body = serde_json::to_string(&json!({
            "success": true,
            "data": {
                "component_id": "official-test-text-v1",
                "version": "1.0.0",
                "owner_type": "official",
                "template_json": {
                    "id": "official-test-text-v1",
                    "name": "Signed Template",
                    "version": "1.0.0",
                    "type": "text",
                    "auth": { "fields": [] },
                    "request": {
                        "url": "https://example.com/translate",
                        "method": "POST",
                        "body": {}
                    },
                    "response": { "translated_text_path": "data.text" }
                },
                "encrypted_payload": null,
                "nonce": null,
                "algorithm": null,
                "kdf_version": null,
                "signature": "ZmFrZQ==",
                "signing_key_id": "test-signing-key"
            }
        }))
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();

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
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "component template endpoint should preserve signing-key fetch status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "component template endpoint should preserve signing-key fetch error code: {}",
        response
    );
}

#[tokio::test]
async fn local_component_create_preserves_signing_key_fetch_error_details() {
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body = serde_json::to_string(&json!({
            "success": true,
            "data": {
                "component_id": "official-test-text-v1",
                "version": "1.0.0",
                "owner_type": "official",
                "template_json": {
                    "id": "official-test-text-v1",
                    "name": "Signed Template",
                    "version": "1.0.0",
                    "type": "text",
                    "auth": { "fields": [] },
                    "request": {
                        "url": "https://example.com/translate",
                        "method": "POST",
                        "body": {}
                    },
                    "response": { "translated_text_path": "data.text" }
                },
                "encrypted_payload": null,
                "nonce": null,
                "algorithm": null,
                "kdf_version": null,
                "signature": "ZmFrZQ==",
                "signing_key_id": "test-signing-key"
            }
        }))
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();

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
            "id": "local-test-signing-key",
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
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "local component create should preserve signing-key fetch status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "local component create should preserve signing-key fetch error code: {}",
        response
    );
}

#[tokio::test]
async fn local_component_test_preserves_signing_key_fetch_error_details() {
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body = serde_json::to_string(&json!({
            "success": true,
            "data": {
                "component_id": "official-test-text-v1",
                "version": "1.0.0",
                "owner_type": "official",
                "template_json": {
                    "id": "official-test-text-v1",
                    "name": "Signed Template",
                    "version": "1.0.0",
                    "type": "text",
                    "auth": { "fields": [] },
                    "request": {
                        "url": "https://example.com/translate",
                        "method": "POST",
                        "body": {}
                    },
                    "response": { "translated_text_path": "data.text" }
                },
                "encrypted_payload": null,
                "nonce": null,
                "algorithm": null,
                "kdf_version": null,
                "signature": "ZmFrZQ==",
                "signing_key_id": "test-signing-key"
            }
        }))
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();

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
        guard.component_bindings.components.insert(
            "test-signing-binding".to_string(),
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
            "component_key": "test-signing-binding",
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
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "component test endpoint should preserve signing-key fetch status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "component test endpoint should preserve signing-key fetch error code: {}",
        response
    );
}

#[tokio::test]
async fn local_component_file_test_preserves_signing_key_fetch_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body = serde_json::to_string(&json!({
            "success": true,
            "data": {
                "component_id": "official-test-image-v1",
                "version": "1.0.0",
                "owner_type": "official",
                "template_json": {
                    "id": "official-test-image-v1",
                    "name": "Signed Template",
                    "version": "1.0.0",
                    "type": "image",
                    "auth": { "fields": [] },
                    "request": {
                        "url": "https://example.com/translate",
                        "method": "POST",
                        "body": {}
                    },
                    "response": { "translated_ref_path": "data.url" }
                },
                "encrypted_payload": null,
                "nonce": null,
                "algorithm": null,
                "kdf_version": null,
                "signature": "ZmFrZQ==",
                "signing_key_id": "test-signing-key"
            }
        }))
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();

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

    let _env_guard = components_env_lock().lock().unwrap();
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let components_path = format!(
        "/tmp/wptsall-test-components-file-signing-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_path_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-file-signing-component": {
                "name": "Local File Signing Component",
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
    handle_local_component_test_file(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "component_id": "local-file-signing-component",
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
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "component file test endpoint should preserve signing-key fetch status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "component file test endpoint should preserve signing-key fetch error code: {}",
        response
    );
}

#[tokio::test]
async fn component_version_test_preserves_signing_key_fetch_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body = serde_json::to_string(&json!({
            "success": true,
            "data": {
                "component_id": "official-test-text-v1",
                "version": "1.0.0",
                "owner_type": "official",
                "template_json": {
                    "id": "official-test-text-v1",
                    "name": "Signed Template",
                    "version": "1.0.0",
                    "type": "text",
                    "auth": { "fields": [] },
                    "request": {
                        "url": "https://example.com/translate",
                        "method": "POST",
                        "body": {}
                    },
                    "response": { "translated_text_path": "data.text" }
                },
                "encrypted_payload": null,
                "nonce": null,
                "algorithm": null,
                "kdf_version": null,
                "signature": "ZmFrZQ==",
                "signing_key_id": "test-signing-key"
            }
        }))
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();

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

    let _env_guard = components_env_lock().lock().unwrap();
    // P0-LF-03 5.5: implicit server snapshot/signing-key fetches on the
    // create/test paths are LEGACY behaviour; pin the gate explicitly (local
    // mode serves local data only).
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let components_path = format!(
        "/tmp/wptsall-test-components-version-signing-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_path_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-version-signing-component": {
                "name": "Local Version Signing Component",
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
        "local-version-signing-component",
        "v1",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let _ = std::fs::remove_file(&components_path);

    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "component version test endpoint should preserve signing-key fetch status, got response: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "component version test endpoint should preserve signing-key fetch error code: {}",
        response
    );
}
