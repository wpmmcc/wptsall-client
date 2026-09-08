use super::*;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

#[tokio::test]
async fn domain_tokens_upsert_allows_same_site_edit_without_reentering_token() {
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = format!(
        "/tmp/test-domain-token-bindings-{}.json",
        uuid::Uuid::new_v4()
    );
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
        guard.domain_token_bindings.domains.insert(
            "https://blog.wpmm.cc".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wptc1.saved.token".to_string(),
                route_secret: "secret-old".to_string(),
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
    let body = serde_json::to_vec(&json!({
        "api_base_url": "https://blog.wpmm.cc",
        "existing_api_base_url": "https://blog.wpmm.cc",
        "route_secret": "secret-new"
    }))
    .unwrap();
    handle_domain_tokens_upsert(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;
    let binding = guard
        .domain_token_bindings
        .domains
        .get("https://blog.wpmm.cc")
        .expect("binding exists");

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected success response, got: {}",
        response
    );
    assert_eq!(binding.wp_client_token, "wptc1.saved.token");
    assert_eq!(binding.route_secret, "secret-new");
    assert_eq!(guard.domains.len(), 1);
    assert_eq!(guard.domains[0].api_base_url, "https://blog.wpmm.cc");
    assert_eq!(guard.domains[0].site_status, "active");

    let _ = std::fs::remove_file(&temp_path);
}

#[tokio::test]
async fn domain_tokens_upsert_moves_binding_key_when_domain_changes() {
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = format!(
        "/tmp/test-domain-token-bindings-{}.json",
        uuid::Uuid::new_v4()
    );
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
        guard.domain_token_bindings.domains.insert(
            "https://old.wpmm.cc".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wptc1.saved.token".to_string(),
                route_secret: "secret-old".to_string(),
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
    let body = serde_json::to_vec(&json!({
        "api_base_url": "https://new.wpmm.cc",
        "existing_api_base_url": "https://old.wpmm.cc",
        "wp_client_token": "wptc1.saved.token",
        "route_secret": "secret-new"
    }))
    .unwrap();
    handle_domain_tokens_upsert(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let guard = state.lock().await;

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected success response, got: {}",
        response
    );
    assert!(
        !guard
            .domain_token_bindings
            .domains
            .contains_key("https://old.wpmm.cc"),
        "old binding key should be removed after edit"
    );
    let binding = guard
        .domain_token_bindings
        .domains
        .get("https://new.wpmm.cc")
        .expect("new binding exists");
    assert_eq!(binding.wp_client_token, "wptc1.saved.token");
    assert_eq!(binding.route_secret, "secret-new");
    assert_eq!(guard.domains.len(), 1);
    assert_eq!(guard.domains[0].api_base_url, "https://new.wpmm.cc");
    assert_eq!(guard.domains[0].route_secret.as_deref(), Some("secret-new"));

    let _ = std::fs::remove_file(&temp_path);
}

#[tokio::test]
async fn task_type_binding_upsert_accepts_matching_server_component() {
    // P0-LF-03 5.3: binding a server-only component is legacy behaviour and
    // must run with the server control plane explicitly enabled.
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = format!("/tmp/test-task-type-bindings-{}.json", uuid::Uuid::new_v4());
    {
        let mut guard = state.lock().await;
        guard.task_type_component_bindings_path = temp_path.clone();
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
    let body = serde_json::to_vec(&json!({
        "task_type": "text",
        "business_line": "post_content",
        "component_id": "official-test-text-v1"
    }))
    .unwrap();
    handle_task_type_components_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "matching server component binding should succeed, got: {}",
        response
    );
    assert_eq!(payload["data"]["task_type"], json!("text"));
    assert_eq!(
        payload["data"]["component_id"],
        json!("official-test-text-v1")
    );

    let _ = std::fs::remove_file(&temp_path);
}

#[tokio::test]
async fn task_type_binding_upsert_rejects_local_component_with_wrong_kind() {
    let _env_guard = components_env_lock().lock().unwrap();
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_bindings_path = format!("/tmp/test-task-type-bindings-{}.json", uuid::Uuid::new_v4());
    let components_path = format!(
        "/tmp/test-task-type-local-components-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-image": {
                "name": "Image Component",
                "template_id": "official-image-v1",
                "kind": "image",
                "enabled": true,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "official-image-v1",
                    "name": "Image Template",
                    "version": "1.0.0",
                    "type": "image_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/image",
                        "body": {}
                    },
                    "response": {
                        "translated_image_ref_path": "data.url"
                    }
                }
            }
        }
    }))
    .unwrap();
    crate::bindings::save_components_local(&components_path, &local_doc).unwrap();
    {
        let mut guard = state.lock().await;
        guard.task_type_component_bindings_path = temp_bindings_path.clone();
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
    let body = serde_json::to_vec(&json!({
        "task_type": "text",
        "component_id": "comp-image"
    }))
    .unwrap();
    handle_task_type_components_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "mismatched local component kind must be rejected, got: {}",
        response
    );
    assert!(
        response.contains("cannot bind to task_type 'text'"),
        "expected kind mismatch details, got: {}",
        response
    );

    let _ = std::fs::remove_file(&temp_bindings_path);
    let _ = std::fs::remove_file(&components_path);
}
