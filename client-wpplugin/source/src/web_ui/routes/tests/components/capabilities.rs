//! P0-LF-03 §5.2: component capabilities must be sourced from local data in
//! local mode and must never leak the server component cache
//! (`guard.components`). In legacy mode the server view is served with an
//! explicit `legacy_server` marker.

use super::*;

#[tokio::test]
async fn capabilities_local_mode_ignores_server_component_cache() {
    let _env_guard = components_env_lock().lock().unwrap();
    // Seed the server component cache as if the legacy lane had refreshed it.
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.components = vec![make_test_server_component(
            "server-only-text-v1",
            "text_translation",
            "text",
        )];
    }

    let local_components_path = "/tmp/test-capabilities-components.json";
    std::fs::remove_file(local_components_path).ok();
    let _file_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", local_components_path.to_string());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    handle_components_capabilities(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "capabilities should succeed, got: {}",
        response
    );
    assert_eq!(
        payload["data"]["source"], json!("local"),
        "default mode must serve the local capabilities view: {}",
        payload
    );
    let items = payload["data"]["components"].as_array().expect("components");
    assert!(
        !items.iter().any(|item| item["id"] == "server-only-text-v1"),
        "server component cache must never appear in local capabilities: {}",
        payload
    );

    let _ = std::fs::remove_file(local_components_path);
}

#[tokio::test]
async fn capabilities_local_mode_lists_local_components_with_templates() {
    let _env_guard = components_env_lock().lock().unwrap();
    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);

    let local_components_path = "/tmp/test-capabilities-local-components.json";
    let runtime_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "cap-active": {
                "name": "Capability Component",
                "template_id": "local-cap-template-v1",
                "vendor_id": "vendor-cap",
                "vendor_name": "Vendor Cap",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "local-cap-template-v1",
                    "name": "Capability Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/cap"
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            },
            "cap-disabled": {
                "name": "Disabled Component",
                "template_id": "local-cap-template-v1",
                "vendor_id": "vendor-cap",
                "vendor_name": "Vendor Cap",
                "kind": "text",
                "enabled": false,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "local-cap-template-v1",
                    "name": "Capability Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/disabled"
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .unwrap();
    std::fs::write(
        local_components_path,
        serde_json::to_vec(&runtime_doc).unwrap(),
    )
    .unwrap();
    let _file_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", local_components_path.to_string());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    handle_components_capabilities(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{}", response);
    assert_eq!(payload["data"]["source"], json!("local"));

    let items = payload["data"]["components"].as_array().expect("components");
    let active = items
        .iter()
        .find(|item| item["id"] == "cap-active")
        .expect("active component listed");
    assert_eq!(active["enabled"], json!(true));
    assert_eq!(active["available"], json!(true));
    assert_eq!(active["source"], json!("local"));
    assert_eq!(active["vendor_name"], json!("Vendor Cap"));

    // Disabled components may be listed but are never routing candidates.
    let disabled = items
        .iter()
        .find(|item| item["id"] == "cap-disabled")
        .expect("disabled component listed");
    assert_eq!(disabled["enabled"], json!(false));
    assert_eq!(disabled["available"], json!(false));

    let format_map = payload["data"]["format_component_map"]
        .as_object()
        .expect("format map");
    let contains_id = |value: &Value, id: &str| {
        value
            .as_array()
            .map(|items| items.iter().any(|item| item == id))
            .unwrap_or(false)
    };
    for (format, components) in format_map.iter() {
        assert!(
            !contains_id(components, "cap-disabled"),
            "disabled component must not be a routing candidate for {}: {}",
            format,
            components
        );
    }
    assert!(
        format_map
            .values()
            .any(|components| contains_id(components, "cap-active")),
        "the enabled component with a template must be a routing candidate: {:?}",
        format_map
    );

    let _ = std::fs::remove_file(local_components_path);
}

#[tokio::test]
async fn capabilities_legacy_mode_serves_server_view_with_marker() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.components = vec![make_test_server_component(
            "server-only-text-v1",
            "text_translation",
            "text",
        )];
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    handle_components_capabilities(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{}", response);
    assert_eq!(
        payload["data"]["source"], json!("legacy_server"),
        "legacy mode must label the server view explicitly: {}",
        payload
    );
    let items = payload["data"]["components"].as_array().expect("components");
    assert!(
        items.iter().any(|item| item["id"] == "server-only-text-v1"),
        "legacy mode shows the server component view: {}",
        payload
    );
}
