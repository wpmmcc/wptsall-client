//! P0-LF-03 §5.5: local component create/update/test must never touch the
//! website or the cached server component list in local mode. Creating a
//! non-OpenAI component without `template_json` is a local validation error,
//! updating never refreshes a server snapshot, testing a local component
//! works without website session state, and version validation reads the
//! local record only.

use super::*;
use crate::web_ui::test_support::WebUiTestHarness;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A loopback listener that counts accepted connections and drops them; any
/// dial by the code under test is counted (and fails fast on the peer).
async fn spawn_counting_listener() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            counter_clone.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    (format!("http://{}", addr), counter)
}

fn text_template_json() -> serde_json::Value {
    serde_json::json!({
        "id": "tpl-local-test-v1",
        "name": "Local Test Template",
        "version": "1.0.0",
        "type": "text_translation",
        "auth": null,
        "request": {"method": "POST", "url": "http://127.0.0.1:1/translate"},
        "response": {"translated_text_path": "data.text"}
    })
}

#[tokio::test]
async fn local_component_create_without_template_requires_local_template() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, counter) = spawn_counting_listener().await;
    let harness = WebUiTestHarness::new(&server_base, Some("sess_test"))
        .await
        .unwrap();

    let response = harness
        .post_json(
            "/api/components/local",
            serde_json::json!({
                "id": "no-template-comp",
                "name": "No Template Component",
                "template_id": "some-server-template",
                "vendor_id": "vendor-local",
                "kind": "text",
                "enabled": true
            }),
        )
        .await
        .unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 422"),
        "creating a non-OpenAI component without template_json must be a local validation error, got: {} {}",
        response.status_line,
        response.raw_body
    );
    assert_eq!(
        response.body["error"]["code"],
        serde_json::json!("COMPONENT_TEMPLATE_REQUIRED"),
        "expected COMPONENT_TEMPLATE_REQUIRED, got: {}",
        response.raw_body
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "component creation in local mode must never download a server snapshot"
    );
}

#[tokio::test]
async fn local_component_create_with_template_succeeds_without_network() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, counter) = spawn_counting_listener().await;
    let harness = WebUiTestHarness::new(&server_base, None).await.unwrap();

    let response = harness
        .create_local_component(serde_json::json!({
            "id": "inline-comp",
            "name": "Inline Component",
            "vendor_id": "vendor-local",
            "kind": "text",
            "enabled": true,
            "template_json": text_template_json()
        }))
        .await
        .unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 200"),
        "creating a component with an inline template must succeed without a session, got: {} {}",
        response.status_line,
        response.raw_body
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "inline component creation must not touch the network"
    );
}

#[tokio::test]
async fn local_component_update_never_refreshes_snapshot_in_local_mode() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, counter) = spawn_counting_listener().await;
    let harness = WebUiTestHarness::new(&server_base, None).await.unwrap();

    harness
        .create_local_component(serde_json::json!({
            "id": "update-comp",
            "name": "Update Component",
            "vendor_id": "vendor-local",
            "kind": "text",
            "enabled": true,
            "template_json": text_template_json()
        }))
        .await
        .unwrap()
        .body
        .get("success")
        .expect("create ok");

    // A plain field update must not trigger any snapshot refresh.
    let response = harness
        .put_json(
            "/api/components/local/update-comp",
            serde_json::json!({"remarks": "updated locally"}),
        )
        .await
        .unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 200"),
        "update must succeed, got: {} {}",
        response.status_line,
        response.raw_body
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "updating a component in local mode must never refresh a server snapshot"
    );

    // Pointing the component at another template_id must NOT fetch a server
    // snapshot in local mode either; the update succeeds and the component
    // simply keeps whatever local template it has.
    let response = harness
        .put_json(
            "/api/components/local/update-comp",
            serde_json::json!({"template_id": "tpl-elsewhere-v2"}),
        )
        .await
        .unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 200"),
        "template_id update must not require a server snapshot in local mode, got: {} {}",
        response.status_line,
        response.raw_body
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "template_id update in local mode must never fetch a server snapshot"
    );
}

#[tokio::test]
async fn component_test_uses_local_template_without_session() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, counter) = spawn_counting_listener().await;
    // NO session token: the local test path must not need website state.
    let harness = WebUiTestHarness::new(&server_base, None).await.unwrap();

    harness
        .create_local_component(serde_json::json!({
            "id": "test-comp",
            "name": "Test Component",
            "vendor_id": "vendor-local",
            "kind": "text",
            "enabled": true,
            "template_json": text_template_json()
        }))
        .await
        .unwrap();
    // Bind the component so the test route finds its local key binding.
    harness
        .post_json(
            "/api/components/bindings/upsert",
            serde_json::json!({"component_id": "test-comp"}),
        )
        .await
        .unwrap();

    let response = harness
        .post_json(
            "/api/components/test",
            serde_json::json!({"component_key": "test-comp", "text": "hello"}),
        )
        .await
        .unwrap();
    let body_text = response.raw_body.clone();
    assert!(
        !body_text.contains("SESSION_REQUIRED"),
        "testing a local component must not require website session state: {}",
        body_text
    );
    // The template points at 127.0.0.1:1 (connection refused), so the run
    // reports a component test failure — but it must be a LOCAL failure,
    // never a website round trip.
    assert!(
        response.status_line.starts_with("HTTP/1.1 200"),
        "the test route answers 200 with a structured failure, got: {} {}",
        response.status_line,
        body_text
    );
    assert!(
        body_text.contains("COMPONENT_TEST_FAILED") || body_text.contains("translated_text"),
        "expected a local test outcome, got: {}",
        body_text
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "component testing in local mode must never contact the website"
    );
}

#[tokio::test]
async fn component_version_test_uses_local_version_only() {
    let _env_guard = components_env_lock().lock().unwrap();
    let _gate_guard = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let (server_base, counter) = spawn_counting_listener().await;
    let harness = WebUiTestHarness::new(&server_base, None).await.unwrap();

    harness
        .create_local_component(serde_json::json!({
            "id": "version-comp",
            "name": "Version Component",
            "vendor_id": "vendor-local",
            "kind": "text",
            "enabled": true,
            "template_json": text_template_json()
        }))
        .await
        .unwrap();
    // The create API does not accept source_template_api_version; stamp an
    // unsupported version onto the LOCAL record through the runtime doc (the
    // version the version-test route must honor in local mode).
    {
        let mut doc = load_local_components_runtime_doc();
        let comp = doc
            .components
            .get_mut("version-comp")
            .expect("component exists");
        comp.source_template_api_version = Some("99.0.0".to_string());
        save_local_components_runtime_doc(&doc).expect("save local components doc");
    }
    harness
        .post_json(
            "/api/components/local/version-comp/versions",
            serde_json::json!({"version": "v1"}),
        )
        .await
        .unwrap();

    let response = harness
        .post_json(
            "/api/components/local/version-comp/versions/v1/test",
            serde_json::json!({"text": "hello"}),
        )
        .await
        .unwrap();
    assert!(
        response.raw_body.contains("COMPONENT_API_VERSION_UNSUPPORTED"),
        "version validation must use the LOCAL record's api_version, got: {}",
        response.raw_body
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "version validation in local mode must not touch the network"
    );
}
