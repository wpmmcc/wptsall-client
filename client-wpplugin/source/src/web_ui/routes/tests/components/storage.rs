use super::*;

#[tokio::test]
async fn local_component_detail_prefers_runtime_db_over_json_file_in_web_ui_mode() {
    let _env_guard = components_env_lock().lock().unwrap();
    let db_path = format!(
        "/tmp/wptsall-web-ui-local-components-{}.db",
        uuid::Uuid::new_v4()
    );
    let components_path = format!(
        "/tmp/wptsall-web-ui-local-components-{}.json",
        uuid::Uuid::new_v4()
    );
    let _web_ui_guard = EnvVarGuard::set("WPTSALL_WEB_UI", "true".to_string());
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let mismatched_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-json": {
                "name": "JSON Component",
                "template_id": "official-json-template-v1",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {}
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &mismatched_doc).unwrap();

    let conn = crate::db::open_db(&db_path).unwrap();
    mark_json_migration_done(&conn);
    let runtime_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-db": {
                "name": "DB Component",
                "template_id": "official-db-template-v1",
                "source_template_id": "official-db-template-v1",
                "source_template_updated_at": "2026-03-07T00:00:00Z",
                "vendor_id": "vendor-db",
                "vendor_name": "Vendor DB",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "updated_at": "2",
                "versions": {},
                "template_json": {
                    "id": "official-db-template-v1",
                    "name": "DB Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/db"
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .unwrap();
    crate::db::components::save_local_components_doc(&conn, &runtime_doc).unwrap();
    drop(conn);

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
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
    handle_local_component_detail(&mut socket, &state, "comp-db")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "local component detail should read runtime DB data in Web UI mode, got response: {}",
        response
    );
    assert_eq!(payload["data"]["id"], json!("comp-db"));
    assert_eq!(payload["data"]["name"], json!("DB Component"));
    assert_eq!(
        payload["data"]["template_id"],
        json!("official-db-template-v1")
    );
    assert_eq!(payload["data"]["vendor_name"], json!("Vendor DB"));

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn bindings_upsert_persists_to_runtime_db_without_touching_json_file_in_web_ui_mode() {
    let _env_guard = components_env_lock().lock().unwrap();
    let db_path = format!(
        "/tmp/wptsall-web-ui-component-bindings-{}.db",
        uuid::Uuid::new_v4()
    );
    let bindings_path = format!(
        "/tmp/wptsall-web-ui-component-bindings-{}.json",
        uuid::Uuid::new_v4()
    );
    let sentinel_json = "{\"sentinel\":true}";
    std::fs::write(&bindings_path, sentinel_json).unwrap();
    let _web_ui_guard = EnvVarGuard::set("WPTSALL_WEB_UI", "true".to_string());
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    // P0-LF-03 5.3: bindings target LOCAL components in local mode, so the
    // bound component must exist. In Web UI mode the local components doc
    // lives in the runtime SQLite DB, not the JSON file.
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-db-bind": {
                "name": "DB Bound Component",
                "template_id": "tpl-db-bind-v1",
                "vendor_id": "vendor-db",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "tpl-db-bind-v1",
                    "name": "DB Bound Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {"method": "POST", "url": "https://example.com/translate"},
                    "response": {"translated_text_path": "data.text"}
                }
            }
        }
    }))
    .unwrap();
    crate::db::components::save_runtime_local_components_doc(&local_doc)
        .expect("seed local components into the runtime DB");

    let conn = crate::db::open_db(&db_path).unwrap();
    mark_json_migration_done(&conn);
    crate::db::bindings::save_component_bindings_doc(&conn, &ComponentBindingsDoc::default())
        .unwrap();
    drop(conn);

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.component_bindings_path = bindings_path.clone();
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
        "component_id": "comp-db-bind",
        "auth": {
            "api_key": "secret-from-runtime-db"
        }
    }))
    .unwrap();
    handle_bindings_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "bindings upsert should succeed in Web UI mode, got response: {}",
        response
    );
    assert_eq!(payload["data"]["component_id"], json!("comp-db-bind"));

    let conn = crate::db::open_db(&db_path).unwrap();
    let saved_doc = crate::db::bindings::load_component_bindings_doc(&conn);
    let saved_entry = saved_doc
        .components
        .get("comp-db-bind")
        .expect("runtime DB binding entry must exist");
    assert_eq!(
        saved_entry.auth.get("api_key"),
        Some(&"secret-from-runtime-db".to_string())
    );
    assert_eq!(
        std::fs::read_to_string(&bindings_path).unwrap(),
        sentinel_json,
        "Web UI runtime binding save must not rewrite JSON binding files"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bindings_path);
}

#[tokio::test]
async fn bindings_upsert_rejects_vendor_mismatched_auth_pool_resources() {
    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-components-local-{}.json",
        uuid::Uuid::new_v4()
    );
    let vendor_keys_path_value = format!("/tmp/wptsall-vendor-keys-{}.json", uuid::Uuid::new_v4());
    let vendor_oauth_path_value =
        format!("/tmp/wptsall-vendor-oauth-{}.json", uuid::Uuid::new_v4());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _vendor_keys_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", vendor_keys_path_value.clone());
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path_value.clone());

    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-openai": {
                "name": "Comp OpenAI",
                "template_id": "official-openai-text-v1",
                "source_template_id": "official-openai-text-v1",
                "source_template_api_version": "1.0.0",
                "vendor_id": "openai",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "official-openai-text-v1",
                    "name": "OpenAI Text",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {"method":"POST","url":"https://api.openai.com/v1/chat/completions"},
                    "response": {"translated_text_path":"choices.0.message.content"}
                }
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &local_doc).unwrap();

    let mut keys_doc = VendorKeysDoc::default();
    keys_doc.keys.insert(
        "azure-key".to_string(),
        VendorKey {
            vendor_id: "azure".to_string(),
            label: "Azure".to_string(),
            auth_values: HashMap::from([("api_key".to_string(), "sk-azure".to_string())]),
            max_concurrent: 5,
            requests_per_second: 3.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
        },
    );
    save_vendor_keys(&vendor_keys_path_value, &keys_doc).unwrap();

    let mut oauth_doc = VendorOAuthDoc::default();
    oauth_doc.configs.insert(
        "azure-oauth".to_string(),
        OAuthConfig {
            vendor_id: "azure".to_string(),
            label: "Azure OAuth".to_string(),
            grant_type: "client_credentials".to_string(),
            auth_url: String::new(),
            token_url: "https://login.microsoft.com/token".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("tok".to_string()),
            cached_token_expires_at: 0,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".to_string(),
        },
    );
    save_vendor_oauth(&vendor_oauth_path_value, &oauth_doc).unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
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
        "component_id": "comp-openai",
        "key_ids": ["azure-key"],
        "oauth_ids": ["azure-oauth"]
    }))
    .unwrap();
    handle_bindings_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "bindings upsert should reject mismatched vendor auth pools, got response: {}",
        response
    );
    assert!(
        response.contains("\"INVALID_COMPONENT_AUTH_POOL\""),
        "bindings upsert should return explicit auth-pool validation code: {}",
        response
    );

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&vendor_keys_path_value);
    let _ = std::fs::remove_file(&vendor_oauth_path_value);
}

#[tokio::test]
async fn local_component_create_rejects_unsupported_source_api_version() {
    // P0-LF-03 5.5: creating from a SERVER template snapshot (and its api
    // version gate) is legacy behaviour; pin the gate explicitly.
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.components = serde_json::from_value(json!([
            {
                "id": "official-future-text-v2",
                "name": "Future Text",
                "type": "text_translation",
                "vendor_id": "openai",
                "api_version": "3.0.0",
                "supported_types": ["text"]
            }
        ]))
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
    handle_local_component_create_v2(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "id": "local-future-text",
            "name": "Future Text Local",
            "template_id": "official-future-text-v2",
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
        "local component create should reject incompatible source api_version, got response: {}",
        response
    );
    assert!(
        response.contains("\"COMPONENT_API_VERSION_UNSUPPORTED\""),
        "local component create should return api-version validation code: {}",
        response
    );
}

#[tokio::test]
async fn local_component_create_openai_compatible_does_not_require_template_id() {
    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-components-local-openai-create-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
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
            "id": "comp-openai-local",
            "name": "OpenAI Local",
            "kind": "openai_compatible",
            "api_base": "https://api.openai.com",
            "model": "gpt-4o-mini"
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "openai_compatible create should succeed without template_id, got response: {}",
        response
    );

    let saved_doc = load_components_local(&components_path).unwrap();
    let comp = saved_doc
        .components
        .get("comp-openai-local")
        .expect("local openai component exists");
    assert_eq!(comp.template_id, "");
    assert_eq!(comp.source_template_id, "");
    assert_eq!(comp.source_template_updated_at, None);
    assert_eq!(comp.source_template_api_version, None);
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("request"))
            .and_then(|value| value.get("body"))
            .and_then(|value| value.get("model"))
            .and_then(|value| value.as_str()),
        Some("gpt-4o-mini")
    );

    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn local_component_create_accepts_inline_template_json_without_server_snapshot() {
    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-components-local-inline-create-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
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
            "id": "comp-inline-local",
            "name": "Inline Local",
            "kind": "text",
            "vendor_id": "local-vendor",
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
        }))
        .unwrap()
        .as_slice(),
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "inline template_json create should not require server snapshot: {}",
        response
    );

    let saved_doc = load_components_local(&components_path).unwrap();
    let comp = saved_doc
        .components
        .get("comp-inline-local")
        .expect("inline local component exists");
    assert_eq!(comp.template_id, "");
    assert_eq!(comp.source_template_id, "");
    assert!(comp.template_json.is_some());

    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn local_component_update_openai_compatible_clears_legacy_template_refs() {
    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-components-local-openai-update-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-openai-local": {
                "name": "OpenAI Local",
                "template_id": "official-openai-text-v1",
                "source_template_id": "official-openai-text-v1",
                "source_template_updated_at": "2026-03-07T12:00:00Z",
                "source_template_api_version": "1.0.0",
                "vendor_id": "openai",
                "kind": "openai_compatible",
                "enabled": true,
                "created_at": "1",
                "versions": {},
                "template_json": generate_openai_compatible_template(
                    "https://api.openai.com",
                    "gpt-4o-mini",
                    Some("Old prompt"),
                    Some(0.3),
                    Some(128),
                    Some("choices.0.message.content")
                )
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &local_doc).unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
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
    handle_local_component_update_v2(
        &mut socket,
        &state,
        serde_json::to_vec(&json!({
            "system_prompt": "New prompt",
            "temperature": 0.7,
            "response_path": "data.translation"
        }))
        .unwrap()
        .as_slice(),
        "comp-openai-local",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "openai_compatible update should succeed while clearing legacy refs, got response: {}",
        response
    );

    let saved_doc = load_components_local(&components_path).unwrap();
    let comp = saved_doc
        .components
        .get("comp-openai-local")
        .expect("updated openai component exists");
    assert_eq!(comp.template_id, "");
    assert_eq!(comp.source_template_id, "");
    assert_eq!(comp.source_template_updated_at, None);
    assert_eq!(comp.source_template_api_version, None);
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("request"))
            .and_then(|value| value.get("url"))
            .and_then(|value| value.as_str()),
        Some("https://api.openai.com/v1/chat/completions")
    );
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("request"))
            .and_then(|value| value.get("body"))
            .and_then(|value| value.get("model"))
            .and_then(|value| value.as_str()),
        Some("gpt-4o-mini")
    );
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("request"))
            .and_then(|value| value.get("body"))
            .and_then(|value| value.get("messages"))
            .and_then(|value| value.as_array())
            .and_then(|messages| messages.first())
            .and_then(|value| value.get("content"))
            .and_then(|value| value.as_str()),
        Some("New prompt")
    );
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("request"))
            .and_then(|value| value.get("body"))
            .and_then(|value| value.get("temperature"))
            .and_then(|value| value.as_f64()),
        Some(0.7)
    );
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("response"))
            .and_then(|value| value.get("translated_text_path"))
            .and_then(|value| value.as_str()),
        Some("data.translation")
    );

    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn local_component_refresh_snapshot_persists_source_api_version() {
    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-components-local-refresh-{}.json",
        uuid::Uuid::new_v4()
    );
    let db_path = format!(
        "/tmp/wptsall-components-local-refresh-{}.db",
        uuid::Uuid::new_v4()
    );
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());

    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-openai": {
                "name": "Comp OpenAI",
                "template_id": "official-openai-text-v1",
                "source_template_id": "official-openai-text-v1",
                "source_template_api_version": "1.0.0",
                "vendor_id": "openai",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "official-openai-text-v1",
                    "name": "Old Snapshot",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {"method":"POST","url":"https://example.com/old"},
                    "response": {"translated_text_path":"data.text"}
                }
            }
        }
    }))
    .unwrap();
    save_components_local(&components_path, &local_doc).unwrap();

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let body = if step == 0 {
                assert!(
                    request_text.starts_with(
                        "GET /api/v1/client/components/official-openai-text-v1/download"
                    ),
                    "first request should download refreshed snapshot, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                serde_json::to_string(&json!({
                    "success": true,
                    "data": {
                        "component_id": "official-openai-text-v1",
                        "version": "1.1.0",
                        "owner_type": "official",
                        "template_json": {
                            "id": "official-openai-text-v1",
                            "name": "New Snapshot",
                            "version": "1.1.0",
                            "type": "text_translation",
                            "request": {"method":"POST","url":"https://example.com/new"},
                            "response": {"translated_text_path":"data.text"}
                        },
                        "encrypted_payload": null,
                        "nonce": null,
                        "algorithm": null,
                        "kdf_version": null,
                        "signature": null,
                        "signing_key_id": null
                    }
                }))
                .unwrap()
            } else {
                assert!(
                    request_text.starts_with("GET /api/v1/client/signing-public-key"),
                    "second request should fetch signing key, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                r#"{"success":true,"data":{"public_key_pem":"-----BEGIN PUBLIC KEY-----\nplaceholder\n-----END PUBLIC KEY-----","key_id":"kid-test"}}"#.to_string()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.db = Arc::new(Mutex::new(crate::db::open_db(&db_path).unwrap()));
        guard.components = serde_json::from_value(json!([
            {
                "id": "official-openai-text-v1",
                "name": "OpenAI Text",
                "type": "text_translation",
                "vendor_id": "openai",
                "api_version": "1.1.0",
                "updated_at": "2026-03-07T12:00:00Z",
                "supported_types": ["text"]
            }
        ]))
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
    handle_local_component_refresh_snapshot(&mut socket, &state, "comp-openai")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "snapshot refresh should succeed for compatible templates, got response: {}",
        response
    );
    assert_eq!(
        payload["data"]["source_template_api_version"],
        json!("1.1.0")
    );

    let saved_doc = load_components_local(&components_path).unwrap();
    let comp = saved_doc.components.get("comp-openai").unwrap();
    assert_eq!(comp.source_template_api_version.as_deref(), Some("1.1.0"));
    assert_eq!(
        comp.source_template_updated_at.as_deref(),
        Some("2026-03-07T12:00:00Z")
    );
    assert_eq!(
        comp.template_json
            .as_ref()
            .and_then(|value| value.get("version"))
            .and_then(|value| value.as_str()),
        Some("1.1.0")
    );

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&db_path);
}
