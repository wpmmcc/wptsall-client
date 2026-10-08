use super::*;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

// catalog: WEBUI-API-POST-api-components-bindings-delete
// catalog: WEBUI-API-POST-api-components-bindings-upsert
// catalog: WEBUI-API-POST-api-domain-tokens-delete
// catalog: WEBUI-API-POST-api-domain-tokens-upsert
// catalog: WEBUI-API-POST-api-task-type-components-delete
// catalog: WEBUI-API-POST-api-task-type-components-upsert
// catalog: WEBUI-API-POST-api-rule-component-bindings-delete
// catalog: WEBUI-API-POST-api-rule-component-bindings-upsert
// catalog: WEBUI-MOD-web-ui-routes-bindings-rs
// oracle: L2
#[tokio::test]
async fn cli03_binding_writes_fail_explicitly_without_changing_state_or_files() {
    let _guard = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _proxies = EnvVarGuard::set(
        "WPTSALL_PROXY_PROFILES_FILE",
        root.path().join("proxies.json").display().to_string(),
    );
    let _mode = EnvVarGuard::set("WPTSALL_WEB_UI", "true");
    let _local = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0");
    let _secret = EnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
    let runtime_path = root.path().join("runtime.db");
    let _runtime = EnvVarGuard::set("WPTSALL_DB_PATH", runtime_path.to_str().unwrap());
    let runtime = crate::db::open_db(runtime_path.to_str().unwrap()).unwrap();
    mark_json_migration_done(&runtime);
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": { "original": {
            "name": "CLI03 fixture", "template_id": "cli03-text",
            "vendor_id": "cli03-test", "vendor_name": "CLI03 fixture",
            "kind": "text", "enabled": true, "created_at": "1", "versions": {},
            "template_json": {
                "id": "cli03-text", "name": "CLI03 fixture", "version": "1.0.0",
                "type": "text_translation", "auth": null,
                "request": { "method": "POST", "url": "http://127.0.0.1:1/translate" },
                "response": { "translated_text_path": "data.text" }
            }
        }}
    }))
    .unwrap();
    crate::db::components::save_local_components_doc(&runtime, &local_doc).unwrap();
    let components_before =
        crate::db::system::get_system_config(&runtime, "local_components_doc").unwrap();
    drop(runtime);
    for key in [
        "component_bindings_doc",
        "domain_token_bindings_doc",
        "task_type_component_bindings_doc",
        "rule_component_bindings_doc",
    ] {
        for (failure, operation) in ["corrupt", "write-denied", "sql-unavailable"]
            .into_iter()
            .flat_map(|failure| ["delete", "upsert"].map(|operation| (failure, operation)))
        {
            let state = build_test_web_ui_state("", None);
            let sentinel_file = root.path().join(format!("{key}-{failure}.json"));
            std::fs::write(&sentinel_file, b"cli03-original-file").unwrap();
            let db = {
                let mut guard = state.lock().await;
                guard.component_bindings_path = sentinel_file.to_str().unwrap().into();
                guard.domain_token_bindings_path = sentinel_file.to_str().unwrap().into();
                guard.task_type_component_bindings_path = sentinel_file.to_str().unwrap().into();
                guard.rule_component_bindings_path = sentinel_file.to_str().unwrap().into();
                guard
                    .component_bindings
                    .components
                    .insert("original".into(), ComponentBindingEntry::default());
                guard.domain_token_bindings.domains.insert(
                    "https://cli03.invalid".into(),
                    DomainTokenBindingEntry {
                        wp_client_token: "cli03-private-sentinel".into(),
                        ..Default::default()
                    },
                );
                guard.task_type_component_bindings.task_types.insert(
                    "text".into(),
                    TaskTypeComponentBindingEntry {
                        component_id: "original".into(),
                    },
                );
                guard
                    .rule_component_bindings
                    .global_defaults
                    .insert("plain_text".into(), "original".into());
                guard.last_error = "original-error".into();
                Arc::clone(&guard.db)
            };
            let state_before = {
                let guard = state.lock().await;
                json!([
                    guard.component_bindings,
                    guard.domain_token_bindings,
                    guard.task_type_component_bindings,
                    guard.rule_component_bindings,
                    guard.domains,
                    guard.last_event,
                    guard.last_error,
                    guard.updated_at
                ])
            };
            let row_before = {
                let conn = db.lock().await;
                let raw = match key {
                    "component_bindings_doc" => {
                        serde_json::to_string(&state.lock().await.component_bindings).unwrap()
                    }
                    "domain_token_bindings_doc" => {
                        serde_json::to_string(&state.lock().await.domain_token_bindings).unwrap()
                    }
                    "task_type_component_bindings_doc" => {
                        serde_json::to_string(&state.lock().await.task_type_component_bindings)
                            .unwrap()
                    }
                    _ => {
                        serde_json::to_string(&state.lock().await.rule_component_bindings).unwrap()
                    }
                };
                crate::db::system::set_encrypted_config(&conn, key, &raw).unwrap();
                match failure {
                    "corrupt" => crate::db::system::set_system_config(&conn, key, "{").unwrap(),
                    "write-denied" => conn.execute_batch(&format!(
                        "CREATE TRIGGER refuse_binding BEFORE INSERT ON system_config WHEN NEW.key = '{key}'
                         BEGIN SELECT RAISE(ABORT, 'cli03 fixture'); END;")).unwrap(),
                    _ => conn.execute_batch("DROP TABLE system_config").unwrap(),
                }
                crate::db::system::get_system_config_checked(&conn, key)
                    .ok()
                    .flatten()
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let reader = tokio::spawn(async move {
                let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
                let mut bytes = Vec::new();
                client.read_to_end(&mut bytes).await.unwrap();
                String::from_utf8(bytes).unwrap()
            });
            let (mut socket, _) = listener.accept().await.unwrap();
            match (key, operation) {
                ("component_bindings_doc", "upsert") => handle_bindings_upsert(&mut socket, &state,
                    br#"{"component_id":"original","auth":{"api_key":"new-fixture-value"}}"#).await.unwrap(),
                ("domain_token_bindings_doc", "upsert") => handle_domain_tokens_upsert(&mut socket, &state,
                    br#"{"api_base_url":"https://cli03.invalid","route_secret":"new-fixture-secret"}"#).await.unwrap(),
                ("task_type_component_bindings_doc", "upsert") => handle_task_type_components_upsert(&mut socket, &state,
                    br#"{"task_type":"text","component_id":"original"}"#).await.unwrap(),
                ("rule_component_bindings_doc", "upsert") => handle_rule_component_bindings_upsert(&mut socket, &state,
                    br#"{"scope":"global","slot_key":"plain_text","component_id":"original"}"#).await.unwrap(),
                ("component_bindings_doc", _) => {
                    handle_bindings_delete(&mut socket, &state, br#"{"component_id":"original"}"#)
                        .await
                        .unwrap()
                }
                ("domain_token_bindings_doc", _) => handle_domain_tokens_delete(
                    &mut socket,
                    &state,
                    br#"{"api_base_url":"https://cli03.invalid"}"#,
                )
                .await
                .unwrap(),
                ("task_type_component_bindings_doc", _) => handle_task_type_components_delete(
                    &mut socket,
                    &state,
                    br#"{"task_type":"text"}"#,
                )
                .await
                .unwrap(),
                _ => handle_rule_component_bindings_delete(
                    &mut socket,
                    &state,
                    br#"{"scope":"global","slot_key":"plain_text"}"#,
                )
                .await
                .unwrap(),
            }
            drop(socket);
            let response = reader.await.unwrap();
            assert!(
                response.starts_with("HTTP/1.1 500"),
                "{key}/{failure}/{operation} must fail explicitly: {response}"
            );
            assert_eq!(
                parse_http_json_body(&response)["error"]["code"],
                "BINDINGS_SAVE_FAILED"
            );
            assert!(!response.contains("cli03-private-sentinel"));
            let guard = state.lock().await;
            let after = json!([
                guard.component_bindings,
                guard.domain_token_bindings,
                guard.task_type_component_bindings,
                guard.rule_component_bindings,
                guard.domains,
                guard.last_event,
                guard.last_error,
                guard.updated_at
            ]);
            assert_eq!(
                after, state_before,
                "{key}/{failure} must leave runtime state unchanged"
            );
            drop(guard);
            let conn = db.lock().await;
            let row_after = crate::db::system::get_system_config_checked(&conn, key);
            if failure == "sql-unavailable" {
                assert!(row_after.is_err());
            } else {
                assert_eq!(row_after.unwrap(), row_before);
            }
            assert_eq!(
                std::fs::read(&sentinel_file).unwrap(),
                b"cli03-original-file"
            );
            let runtime = crate::db::open_db(runtime_path.to_str().unwrap()).unwrap();
            assert_eq!(
                crate::db::system::get_system_config(&runtime, "local_components_doc").as_deref(),
                Some(components_before.as_str()),
                "failed bindings writes must not save component overrides"
            );
        }
    }
}

#[tokio::test]
async fn domain_tokens_upsert_allows_same_site_edit_without_reentering_token() {
    let _key = crate::db::owned_mock_bindings_key();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = temp
        .path()
        .join("domain-token-bindings.json")
        .display()
        .to_string();
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
        guard.domain_token_bindings.domains.insert(
            "https://blog.wpmm.cc".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wptc1.saved.token".to_string(),
                route_secret: "secret-old".to_string(),
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
    let _key = crate::db::owned_mock_bindings_key();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = temp
        .path()
        .join("domain-token-bindings.json")
        .display()
        .to_string();
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
        guard.domain_token_bindings.domains.insert(
            "https://old.wpmm.cc".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wptc1.saved.token".to_string(),
                route_secret: "secret-old".to_string(),
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

#[tokio::test]
async fn domain_tokens_upsert_preserves_existing_plugin_identity_and_verified_metadata() {
    let _key = crate::db::owned_mock_bindings_key();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = temp
        .path()
        .join("domain-token-bindings.json")
        .display()
        .to_string();
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
        guard.domain_token_bindings.domains.insert(
            "https://site-wpmmcc.example.com".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wptc1.initial.token".to_string(),
                route_secret: "secret-1".to_string(),
                plugin_identity: Some(PluginIdentity::Wpmmcc),
                identity_verified_at: Some("2026-09-16T12:00:00Z".to_string()),
                identity_capabilities: Some(IdentityCapabilities {
                    plugin_version: "1.0.0".to_string(),
                    protocol_min: Some(1),
                    protocol_current: Some(1),
                }),
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
    // Update route_secret only, omitting wp_client_token and plugin_identity
    let body = serde_json::to_vec(&json!({
        "api_base_url": "https://site-wpmmcc.example.com",
        "existing_api_base_url": "https://site-wpmmcc.example.com",
        "route_secret": "secret-2"
    }))
    .unwrap();
    handle_domain_tokens_upsert(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {}", response);
    assert!(response.contains("\"plugin_identity\":\"wpmmcc\""));
    assert!(response.contains("\"identity_verified_at\":\"2026-09-16T12:00:00Z\""));

    let guard = state.lock().await;
    let binding = guard
        .domain_token_bindings
        .domains
        .get("https://site-wpmmcc.example.com")
        .expect("binding exists");
    assert_eq!(binding.plugin_identity, Some(PluginIdentity::Wpmmcc));
    assert_eq!(
        binding.identity_verified_at.as_deref(),
        Some("2026-09-16T12:00:00Z")
    );
    assert_eq!(binding.route_secret, "secret-2");
    assert_eq!(binding.wp_client_token, "wptc1.initial.token");

    let _ = std::fs::remove_file(&temp_path);
}

#[tokio::test]
async fn domain_tokens_upsert_accepts_and_stores_explicit_plugin_identity() {
    let _key = crate::db::owned_mock_bindings_key();
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let state = build_test_web_ui_state("https://www.wpmm.cc", Some("sess_test"));
    let temp_path = temp
        .path()
        .join("domain-token-bindings.json")
        .display()
        .to_string();
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
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
        "api_base_url": "https://new-ats.example.com",
        "wp_client_token": "wptc1.new.token",
        "route_secret": "secret-new",
        "plugin_identity": "wpmmcc_ats"
    }))
    .unwrap();
    handle_domain_tokens_upsert(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {}", response);
    assert!(response.contains("\"plugin_identity\":\"wpmmcc_ats\""));

    let guard = state.lock().await;
    let binding = guard
        .domain_token_bindings
        .domains
        .get("https://new-ats.example.com")
        .expect("binding exists");
    assert_eq!(binding.plugin_identity, Some(PluginIdentity::WpmmccAts));
    assert_eq!(binding.route_secret, "secret-new");

    let _ = std::fs::remove_file(&temp_path);
}
