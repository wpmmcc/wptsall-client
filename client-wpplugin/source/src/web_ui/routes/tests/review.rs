use super::*;
use crate::web_ui::routes::components::generate_openai_compatible_template;
use std::collections::HashMap;

#[tokio::test]
async fn build_runtime_registry_for_retranslate_loads_server_component_runtime() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());
    let component_bindings_path = format!(
        "/tmp/wptsall-component-bindings-retranslate-{}.json",
        uuid::Uuid::new_v4()
    );

    let component_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let component_addr = component_server.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = component_server.accept().await.unwrap();
            let mut request = vec![0u8; 8192];
            let read = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..read]);
            let first_line = request_text.lines().next().unwrap_or_default().to_string();
            let body = if first_line.contains("/api/v1/client/components?page=") {
                json!({
                    "success": true,
                    "data": {
                        "items": [{
                            "id": "srv-image",
                            "name": "Server Image Component",
                            "type": "image",
                            "supported_types": ["image"],
                            "supported_content_formats": ["media_ref"],
                            "vendor_id": "test-vendor",
                            "status": "active",
                            "updated_at": "2026-03-10T00:00:00Z"
                        }],
                        "page": 1,
                        "per_page": 200,
                        "total": 1,
                        "total_pages": 1
                    }
                })
            } else {
                json!({
                    "success": true,
                    "data": {
                        "component_id": "srv-image",
                        "version": "1.0.0",
                        "owner_type": "vendor",
                        "template_json": {
                            "id": "srv-image",
                            "name": "Server Image Component",
                            "version": "1.0.0",
                            "type": "image",
                            "request": {
                                "method": "POST",
                                "url": "http://127.0.0.1:9090/mock-image",
                                "body_type": "json",
                                "body": {
                                    "source_ref": "{{input.source_ref}}"
                                }
                            },
                            "response": {
                                "translated_ref_path": "data.url"
                            }
                        }
                    }
                })
            };
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", component_addr), Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.component_bindings_path = component_bindings_path.clone();
    }

    let registry = super::super::review::build_runtime_registry_for_retranslate(
        &state,
        "srv-image",
        None,
        "/dev/null",
    )
    .await
    .expect("server component runtime should load for retranslate");

    let runtime = registry
        .runtimes
        .get("srv-image")
        .expect("registry should contain server runtime");
    assert_eq!(runtime.template.id, "srv-image");
    assert!(
        runtime
            .supported_content_formats
            .iter()
            .any(|format| format == "media_ref"),
        "server runtime should preserve media_ref capability"
    );
}

#[tokio::test]
async fn item_resubmit_preserves_wp_callback_error_details() {
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

    let _env_guard = components_env_lock().lock().unwrap();
    let _retry_max_guard = EnvVarGuard::set("WPTSALL_RETRY_MAX", "0".to_string());
    let translated_path = format!("/tmp/wptsall-resubmit-test-{}.json", uuid::Uuid::new_v4());
    write_test_translation_envelope(&translated_path, "ctask-resubmit");

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
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        &domain,
        &translated_path,
        "translated",
        "ctask-resubmit",
    )
    .await;

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
    handle_item_resubmit(&mut socket, &state, &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let _ = std::fs::remove_file(&translated_path);

    assert!(
        response.starts_with("HTTP/1.1 403 Forbidden"),
        "item resubmit should preserve WP callback error status, got response: {}",
        response
    );
    assert!(
        response.contains("wptsall_pro_required"),
        "item resubmit should preserve WP callback error code: {}",
        response
    );
}
#[tokio::test]
async fn item_override_save_preserves_existing_lang_overrides_when_fields_are_omitted() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "https://example.com",
        "/tmp/translated-unused.json",
        "pending_review",
        "ctask-override-preserve",
    )
    .await;

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::jobs::update_item_task_override(
            &conn,
            item_id,
            None,
            Some("fr_FR"),
            Some("de_DE"),
            None,
        )
        .unwrap();
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
        "source_lang": "it_IT"
    }))
    .unwrap();
    handle_item_override_save(&mut socket, &state, body.as_slice(), &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "item override save should succeed, got response: {}",
        response
    );
    assert_eq!(payload["data"]["source_lang"], json!("it_IT"));
    assert_eq!(
        payload["data"]["target_lang"],
        json!("de_DE"),
        "omitted target_lang should preserve stored override instead of falling back to item target"
    );

    let saved_item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, item_id).expect("item exists after override save")
    };
    assert_eq!(saved_item.effective_source_lang.as_deref(), Some("it_IT"));
    assert_eq!(saved_item.effective_target_lang.as_deref(), Some("de_DE"));
}

#[tokio::test]
async fn job_items_list_exposes_component_ids_trace() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "https://example.com",
        "/tmp/translated-unused.json",
        "pending_review",
        "ctask-component-trace",
    )
    .await;

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let job_id = {
        let conn = db_arc.lock().await;
        crate::db::jobs::update_item_component_trace(
            &conn,
            item_id,
            "comp-text",
            &["comp-text".to_string(), "comp-image".to_string()],
        )
        .unwrap();
        crate::db::jobs::get_item(&conn, item_id)
            .expect("item should exist")
            .job_id
    };

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
    handle_job_items(&mut socket, &state, &job_id.to_string(), "")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "unexpected response: {}",
        response
    );
    assert!(
        response.contains("\"component_id\":\"comp-text\""),
        "response should include primary component id: {}",
        response
    );
    assert!(
        response.contains("\"component_ids\":[\"comp-text\",\"comp-image\"]"),
        "response should include full component trace: {}",
        response
    );
}
#[tokio::test]
async fn item_override_save_allows_explicit_clear_to_null() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "https://example.com",
        "/tmp/translated-unused.json",
        "pending_review",
        "ctask-override-clear",
    )
    .await;

    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::jobs::update_item_task_override(
            &conn,
            item_id,
            Some("comp-legacy"),
            Some("fr_FR"),
            Some("de_DE"),
            None,
        )
        .unwrap();
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
        "component_id": null,
        "source_lang": null,
        "target_lang": null
    }))
    .unwrap();
    handle_item_override_save(&mut socket, &state, body.as_slice(), &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "item override save should allow explicit clear, got response: {}",
        response
    );
    assert!(payload["data"]["component_id"].is_null());
    assert!(payload["data"]["source_lang"].is_null());
    assert!(payload["data"]["target_lang"].is_null());

    let saved_item = {
        let conn = db_arc.lock().await;
        crate::db::jobs::get_item(&conn, item_id).expect("item exists after override clear")
    };
    assert!(saved_item.selected_component_id.is_none());
    assert!(saved_item.effective_source_lang.is_none());
    assert!(saved_item.effective_target_lang.is_none());
}
#[tokio::test]
async fn item_approve_preserves_wp_callback_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let request_text = String::from_utf8_lossy(&request[..read]);
        let first_line = request_text.lines().next().unwrap_or_default();
        assert!(
            first_line.contains("/route_test/client/"),
            "item approve should recover route_secret from server cache instead of using secretless path: {}",
            first_line
        );
        let body = r#"{"code":"wptsall_pro_required","message":"Client API disabled"}"#;
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let _env_guard = components_env_lock().lock().unwrap();
    // P0-LF-03 5.4: recovering a route_secret from the cached SERVER domain
    // list is legacy behaviour; pin the gate explicitly (local mode resolves
    // local state only). NOTE: EnvVarGuard (test_env_lock) must stay nested
    // INSIDE components_env_lock to keep a single lock order with the
    // WebUiTestHarness tests (components -> env).
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let _retry_max_guard = EnvVarGuard::set("WPTSALL_RETRY_MAX", "0".to_string());
    let translated_path = format!("/tmp/wptsall-approve-test-{}.json", uuid::Uuid::new_v4());
    write_test_translation_envelope(&translated_path, "ctask-approve");

    let domain = format!("http://{}", upstream_addr);
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            domain.clone(),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: String::new(),
            },
        );
        guard.domains = vec![DomainStatusItem {
            api_base_url: domain.clone(),
            site_status: "active".to_string(),
            route_secret: Some("route_test".to_string()),
            max_relations: None,
            plan_expires_at: None,
        }];
    }
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        &domain,
        &translated_path,
        "pending_review",
        "ctask-approve",
    )
    .await;

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
    handle_item_approve(&mut socket, &state, &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let _ = std::fs::remove_file(&translated_path);

    assert!(
        response.starts_with("HTTP/1.1 403 Forbidden"),
        "item approve should preserve WP callback error status, got response: {}",
        response
    );
    assert!(
        response.contains("wptsall_pro_required"),
        "item approve should preserve WP callback error code: {}",
        response
    );
}
#[tokio::test]
async fn items_batch_approve_preserves_wp_callback_error_details() {
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

    let _env_guard = components_env_lock().lock().unwrap();
    let _retry_max_guard = EnvVarGuard::set("WPTSALL_RETRY_MAX", "0".to_string());
    let translated_path = format!(
        "/tmp/wptsall-batch-approve-test-{}.json",
        uuid::Uuid::new_v4()
    );
    write_test_translation_envelope(&translated_path, "ctask-batch-approve");

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
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        &domain,
        &translated_path,
        "pending_review",
        "ctask-batch-approve",
    )
    .await;

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
    handle_items_batch_approve(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "ids": [item_id]
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    let _ = std::fs::remove_file(&translated_path);

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "batch approve should still return aggregated response, got: {}",
        response
    );
    assert_eq!(
        payload["data"]["failed"][0]["code"], "wptsall_pro_required",
        "batch approve should preserve structured WP callback error code in failed item: {}",
        response
    );
    assert_eq!(
        payload["data"]["failed"][0]["status"], "403 Forbidden",
        "batch approve should preserve structured WP callback status in failed item: {}",
        response
    );
}
#[tokio::test]
async fn item_retranslate_preserves_wp_relations_error_details() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let request_text = String::from_utf8_lossy(&request[..read]);
        let first_line = request_text.lines().next().unwrap_or_default();
        assert!(
            first_line.contains("/route_test/client/site-relations"),
            "item retranslate should use resolved route_secret path: {}",
            first_line
        );
        let body = r#"{"code":"signature_invalid","message":"Signature invalid"}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-test-components-retranslate-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_path_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-test": {
                "name": "Retranslate Test Component",
                "template_id": "",
                "source_template_id": "",
                "kind": "openai_compatible",
                "enabled": true,
                "created_at": "1",
                "template_json": generate_openai_compatible_template(
                    "http://127.0.0.1:9090",
                    "mock-model",
                    None,
                    None,
                    None,
                    None
                ),
                "versions": {}
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &local_doc).unwrap();

    let raw_path = "/tmp/raw-test.json";
    std::fs::write(raw_path, br#"{"post_title":"Hello"}"#).unwrap();

    let domain = format!("http://{}", upstream_addr);
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.component_bindings.components.insert(
            "comp-test".to_string(),
            ComponentBindingEntry {
                auth: HashMap::from([("api_key".to_string(), "dummy-key".to_string())]),
                ..Default::default()
            },
        );
        guard.domain_token_bindings.domains.insert(
            domain.clone(),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: "route_test".to_string(),
            },
        );
    }
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        &domain,
        "/tmp/translated-unused.json",
        "translated",
        "ctask-retranslate",
    )
    .await;

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let state_for_server = Arc::clone(&state);
    let runtime_control = WebUiRuntimeControl::new();
    let access_control = AccessControl::new(false, &[]);
    let server = tokio::spawn(async move {
        let (socket, _) = downstream.accept().await.unwrap();
        handle_web_ui_connection(
            socket,
            state_for_server,
            runtime_control,
            "/tmp/wptsall-retranslate-test.log",
            0,
            std::time::Instant::now(),
            access_control,
        )
        .await
    });

    let mut client = tokio::net::TcpStream::connect(downstream_addr)
        .await
        .unwrap();
    let request = format!(
        "POST /api/items/{}/retranslate HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        item_id
    );
    client.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8_lossy(&response).to_string();
    let server_result = server.await.unwrap();

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(raw_path);

    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "item retranslate should preserve WP site-relations auth status instead of dropping the response: {}",
        response
    );
    assert!(
        response.contains("signature_invalid"),
        "item retranslate should preserve WP site-relations auth code: {}",
        response
    );
    assert!(
        server_result.is_ok(),
        "web UI handler should return a response instead of bubbling the transport error: {:?}",
        server_result.err()
    );
}

#[tokio::test]
async fn item_override_save_rejects_non_pending_review_status() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "https://example.com",
        "/tmp/translated-unused.json",
        "done",
        "ctask-override-invalid-status",
    )
    .await;

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
        "source_lang": "it_IT"
    }))
    .unwrap();
    handle_item_override_save(&mut socket, &state, body.as_slice(), &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "expected 400, got: {}",
        response
    );
    assert!(
        response.contains("INVALID_STATUS"),
        "expected INVALID_STATUS, got: {}",
        response
    );
}

#[tokio::test]
async fn item_translated_save_rejects_non_pending_review_status() {
    let translated_path = format!(
        "/tmp/wptsall-translated-save-test-{}.json",
        uuid::Uuid::new_v4()
    );
    write_test_translation_envelope(&translated_path, "ctask-save-invalid-status");
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "https://example.com",
        &translated_path,
        "done",
        "ctask-save-invalid-status",
    )
    .await;

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
        "content": { "post_title": "Edited" }
    }))
    .unwrap();
    handle_item_translated_save(&mut socket, &state, body.as_slice(), &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let saved = std::fs::read_to_string(&translated_path).unwrap();
    let _ = std::fs::remove_file(&translated_path);

    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "expected 400, got: {}",
        response
    );
    assert!(
        response.contains("INVALID_STATUS"),
        "expected INVALID_STATUS, got: {}",
        response
    );
    assert!(
        saved.contains("idempotency_key"),
        "translated envelope should remain intact"
    );
}

#[tokio::test]
async fn item_resubmit_rejects_done_status() {
    let translated_path = format!(
        "/tmp/wptsall-resubmit-done-test-{}.json",
        uuid::Uuid::new_v4()
    );
    write_test_translation_envelope(&translated_path, "ctask-resubmit-done");
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "https://example.com",
        &translated_path,
        "done",
        "ctask-resubmit-done",
    )
    .await;

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
    handle_item_resubmit(&mut socket, &state, &item_id.to_string())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let _ = std::fs::remove_file(&translated_path);
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "expected 400, got: {}",
        response
    );
    assert!(
        response.contains("INVALID_STATUS"),
        "expected INVALID_STATUS, got: {}",
        response
    );
}
