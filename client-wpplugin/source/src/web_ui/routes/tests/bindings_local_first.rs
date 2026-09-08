//! P0-LF-03 §5.3: component binding and task-type binding validation must be
//! LOCAL-ONLY in local mode. Unknown component ids surface as
//! COMPONENT_NOT_FOUND without any website lookup, vendor alignment resolves
//! from the local component record, and version compatibility never consults
//! the cached server component list. In legacy mode
//! (`WPTSALL_USE_SERVER_CONTROL_PLANE=1`) the previous server-cache fallback
//! behaviour is preserved.

use super::*;
use crate::bindings::{save_vendor_keys, save_vendor_oauth};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Bind a loopback listener that counts accepted connections. Handlers under
/// test must never dial it in local mode; a zero counter is the no-network
/// proof (the address is also planted as `server_base` so any accidental dial
/// lands somewhere observable instead of the real internet).
async fn spawn_counting_listener() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((_socket, _)) => {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}"), counter)
}

/// A minimal local component whose template deserializes into a runtime-ready
/// `ComponentTemplate` (id/name/version/type/auth/request/response present).
fn local_text_component_json(vendor_id: &str, api_version: Option<&str>) -> serde_json::Value {
    let mut component = serde_json::json!({
        "name": "Local Text Component",
        "template_id": "tpl-local-text-v1",
        "vendor_id": vendor_id,
        "vendor_name": "Local Vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {},
        "template_json": {
            "id": "tpl-local-text-v1",
            "name": "Local Text Template",
            "version": "1.0.0",
            "type": "text_translation",
            "auth": null,
            "request": {"method": "POST", "url": "https://provider.example/translate"},
            "response": {"translated_text_path": "data.text"}
        }
    });
    if let Some(version) = api_version {
        component["source_template_api_version"] = serde_json::json!(version);
    }
    component
}

fn write_local_components_doc(path: &str, components: serde_json::Value) {
    let doc = serde_json::json!({"version": 1, "components": components});
    std::fs::write(path, serde_json::to_vec(&doc).unwrap()).unwrap();
}

/// Pair a handler invocation with a connected socket so the HTTP response can
/// be read back (same pattern as the other route tests).
async fn with_response_socket() -> (tokio::net::TcpStream, tokio::task::JoinHandle<String>) {
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
    let (socket, _) = downstream.accept().await.unwrap();
    (socket, reader)
}

#[tokio::test]
async fn bindings_upsert_local_mode_unknown_component_404_without_network() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let components_path = format!("/tmp/wptsall-bindings-local-{}.json", uuid::Uuid::new_v4());
    std::fs::remove_file(&components_path).ok();
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let (counting_base, counter) = spawn_counting_listener().await;
    {
        let mut guard = state.lock().await;
        guard.server_base = counting_base;
        guard.component_bindings_path = format!(
            "/tmp/wptsall-bindings-upsert-{}.json",
            uuid::Uuid::new_v4()
        );
    }

    let (mut socket, reader) = with_response_socket().await;
    let body = serde_json::to_vec(&serde_json::json!({
        "component_id": "ghost-component"
    }))
    .unwrap();
    handle_bindings_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "unknown component must be a 404-class error in local mode, got: {}",
        response
    );
    let payload = parse_http_json_body(&response);
    assert_eq!(
        payload["error"]["code"],
        serde_json::json!("COMPONENT_NOT_FOUND"),
        "expected COMPONENT_NOT_FOUND, got: {}",
        response
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "local mode must never dial the network for bindings validation"
    );
    {
        let guard = state.lock().await;
        assert!(
            guard.component_bindings.components.get("ghost-component").is_none(),
            "no binding may be recorded for an unknown component"
        );
    }

    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn bindings_upsert_local_mode_vendor_alignment_uses_local_record() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let components_path = format!("/tmp/wptsall-bindings-local-{}.json", uuid::Uuid::new_v4());
    let vendor_keys_path = format!("/tmp/wptsall-vendor-keys-{}.json", uuid::Uuid::new_v4());
    let vendor_oauth_path = format!("/tmp/wptsall-vendor-oauth-{}.json", uuid::Uuid::new_v4());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _keys_guard = EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", vendor_keys_path.clone());
    let _oauth_guard = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());

    write_local_components_doc(
        &components_path,
        serde_json::json!({
            "local-text-1": local_text_component_json("vendor-local", None)
        }),
    );

    let mut keys_doc = VendorKeysDoc::default();
    keys_doc.keys.insert(
        "local-key".to_string(),
        VendorKey {
            vendor_id: "vendor-local".to_string(),
            label: "Local Vendor Key".to_string(),
            auth_values: HashMap::from([("api_key".to_string(), "sk-local".to_string())]),
            max_concurrent: 5,
            requests_per_second: 3.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
        },
    );
    save_vendor_keys(&vendor_keys_path, &keys_doc).unwrap();
    save_vendor_oauth(&vendor_oauth_path, &VendorOAuthDoc::default()).unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let (counting_base, counter) = spawn_counting_listener().await;
    let bindings_path = format!("/tmp/wptsall-bindings-upsert-{}.json", uuid::Uuid::new_v4());
    {
        let mut guard = state.lock().await;
        guard.server_base = counting_base;
        guard.component_bindings_path = bindings_path.clone();
    }

    let (mut socket, reader) = with_response_socket().await;
    let body = serde_json::to_vec(&serde_json::json!({
        "component_id": "local-text-1",
        "key_ids": ["local-key"]
    }))
    .unwrap();
    handle_bindings_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "vendor alignment from the LOCAL component record must succeed, got: {}",
        response
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "vendor alignment in local mode must not dial the network"
    );
    {
        let guard = state.lock().await;
        let entry = guard
            .component_bindings
            .components
            .get("local-text-1")
            .expect("binding recorded");
        assert_eq!(entry.key_ids, vec!["local-key".to_string()]);
    }

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&vendor_keys_path);
    let _ = std::fs::remove_file(&vendor_oauth_path);
    let _ = std::fs::remove_file(&bindings_path);
}

#[tokio::test]
async fn bindings_upsert_local_mode_vendor_mismatch_rejected_without_network() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let components_path = format!("/tmp/wptsall-bindings-local-{}.json", uuid::Uuid::new_v4());
    let vendor_keys_path = format!("/tmp/wptsall-vendor-keys-{}.json", uuid::Uuid::new_v4());
    let vendor_oauth_path = format!("/tmp/wptsall-vendor-oauth-{}.json", uuid::Uuid::new_v4());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _keys_guard = EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", vendor_keys_path.clone());
    let _oauth_guard = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());

    write_local_components_doc(
        &components_path,
        serde_json::json!({
            "local-text-1": local_text_component_json("vendor-local", None)
        }),
    );

    let mut keys_doc = VendorKeysDoc::default();
    keys_doc.keys.insert(
        "other-key".to_string(),
        VendorKey {
            vendor_id: "vendor-other".to_string(),
            label: "Other Vendor Key".to_string(),
            auth_values: HashMap::from([("api_key".to_string(), "sk-other".to_string())]),
            max_concurrent: 5,
            requests_per_second: 3.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
        },
    );
    save_vendor_keys(&vendor_keys_path, &keys_doc).unwrap();
    save_vendor_oauth(&vendor_oauth_path, &VendorOAuthDoc::default()).unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let (counting_base, counter) = spawn_counting_listener().await;
    {
        let mut guard = state.lock().await;
        guard.server_base = counting_base;
        guard.component_bindings_path = format!(
            "/tmp/wptsall-bindings-upsert-{}.json",
            uuid::Uuid::new_v4()
        );
    }

    let (mut socket, reader) = with_response_socket().await;
    let body = serde_json::to_vec(&serde_json::json!({
        "component_id": "local-text-1",
        "key_ids": ["other-key"]
    }))
    .unwrap();
    handle_bindings_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 422 Unprocessable Entity"),
        "mismatched vendor key must be rejected, got: {}",
        response
    );
    assert!(
        response.contains("vendor-local"),
        "the expected vendor must come from the LOCAL component record: {}",
        response
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "mismatch validation in local mode must not dial the network"
    );

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&vendor_keys_path);
    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn task_type_upsert_local_mode_unknown_component_404_without_network() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let components_path = format!("/tmp/wptsall-bindings-local-{}.json", uuid::Uuid::new_v4());
    std::fs::remove_file(&components_path).ok();
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let (counting_base, counter) = spawn_counting_listener().await;
    let bindings_path = format!("/tmp/wptsall-tasktype-bind-{}.json", uuid::Uuid::new_v4());
    {
        let mut guard = state.lock().await;
        guard.server_base = counting_base;
        guard.task_type_component_bindings_path = bindings_path.clone();
    }

    let (mut socket, reader) = with_response_socket().await;
    let body = serde_json::to_vec(&serde_json::json!({
        "task_type": "text",
        "component_id": "ghost-component"
    }))
    .unwrap();
    handle_task_type_components_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 404 Not Found"),
        "unknown task-type component must be a 404-class error in local mode, got: {}",
        response
    );
    let payload = parse_http_json_body(&response);
    assert_eq!(
        payload["error"]["code"],
        serde_json::json!("COMPONENT_NOT_FOUND"),
        "expected COMPONENT_NOT_FOUND, got: {}",
        response
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "local mode must never fetch components from the website for task-type bindings"
    );
    {
        let guard = state.lock().await;
        assert!(
            guard.task_type_component_bindings.task_types.get("text").is_none(),
            "no task-type binding may be recorded for an unknown component"
        );
    }

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&bindings_path);
}

#[tokio::test]
async fn task_type_upsert_local_mode_success_uses_local_version_only() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let components_path = format!("/tmp/wptsall-bindings-local-{}.json", uuid::Uuid::new_v4());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    // The local record claims an unsupported API version while the cached
    // server list advertises a supported one for the same template. Local
    // mode must trust ONLY the local record and reject.
    write_local_components_doc(
        &components_path,
        serde_json::json!({
            "local-text-1": local_text_component_json("vendor-local", Some("99.0.0"))
        }),
    );

    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let (counting_base, counter) = spawn_counting_listener().await;
    let bindings_path = format!("/tmp/wptsall-tasktype-bind-{}.json", uuid::Uuid::new_v4());
    {
        let mut guard = state.lock().await;
        guard.server_base = counting_base;
        guard.task_type_component_bindings_path = bindings_path.clone();
        guard.components = vec![
            serde_json::from_value(serde_json::json!({
                "id": "tpl-local-text-v1",
                "name": "Server cached twin",
                "type": "text_translation",
                "vendor_id": "vendor-local",
                "api_version": "1.0.0",
                "status": "active",
                "supported_types": ["text"]
            }))
            .expect("valid cached server component"),
        ];
    }

    let (mut socket, reader) = with_response_socket().await;
    let body = serde_json::to_vec(&serde_json::json!({
        "task_type": "text",
        "component_id": "local-text-1"
    }))
    .unwrap();
    handle_task_type_components_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.contains("unsupported api_version"),
        "local mode must validate the version from the LOCAL record only, got: {}",
        response
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "version validation in local mode must not dial the network"
    );

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&bindings_path);
}

#[tokio::test]
async fn task_type_upsert_legacy_mode_still_uses_server_cache_for_version() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "1".to_string());
    let components_path = format!("/tmp/wptsall-bindings-local-{}.json", uuid::Uuid::new_v4());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    write_local_components_doc(
        &components_path,
        serde_json::json!({
            "local-text-1": local_text_component_json("vendor-local", Some("99.0.0"))
        }),
    );

    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_test"));
    let bindings_path = format!("/tmp/wptsall-tasktype-bind-{}.json", uuid::Uuid::new_v4());
    {
        let mut guard = state.lock().await;
        guard.task_type_component_bindings_path = bindings_path.clone();
        // Same cached server component as the local-mode test: in legacy mode
        // its supported api_version wins over the local record's 99.0.0, the
        // version gate passes, and the FULL runtime build + binding succeeds.
        // The local-mode twin test rejects the exact same setup, which pins
        // the gate direction on both sides.
        guard.components = vec![
            serde_json::from_value(serde_json::json!({
                "id": "tpl-local-text-v1",
                "name": "Server cached twin",
                "type": "text_translation",
                "vendor_id": "vendor-local",
                "api_version": "1.0.0",
                "status": "active",
                "supported_types": ["text"]
            }))
            .expect("valid cached server component"),
        ];
    }

    let (mut socket, reader) = with_response_socket().await;
    let body = serde_json::to_vec(&serde_json::json!({
        "task_type": "text",
        "component_id": "local-text-1"
    }))
    .unwrap();
    handle_task_type_components_upsert(&mut socket, &state, body.as_slice())
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "legacy mode must keep using the server cache for version checks and succeed, got: {}",
        response
    );
    assert!(
        !response.contains("unsupported api_version"),
        "the local record's unsupported version must not leak into legacy mode, got: {}",
        response
    );

    let _ = std::fs::remove_file(&components_path);
    let _ = std::fs::remove_file(&bindings_path);
}
