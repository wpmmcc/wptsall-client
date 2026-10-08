//! Route matrix over the WebUI dispatcher: data-driven
//! (method, path, payload, session-state) -> (status / error-code / shape)
//! cases for route modules that previously had zero catalog signal.
//!
//! Covered modules (exercised through the REAL dispatcher via
//! `WebUiTestHarness`, not through private handlers):
//! - components/catalog.rs    (capabilities local view, refresh session gate)
//! - components/downloads.rs  (template validation + failure paths)
//! - bindings/domain_tokens.rs (upsert/delete round-trip via /api/domain-tokens)
//! - bindings/task_rules.rs    (rule component bindings via /api/rule-component-bindings)
//!
//! catalog: WEBUI-MOD-web-ui-routes-components-catalog-rs
//! catalog: WEBUI-MOD-web-ui-routes-components-downloads-rs
//! catalog: WEBUI-MOD-bindings-domain-tokens-rs
//! catalog: WEBUI-MOD-bindings-task-rules-rs
//! oracle: L2
//!
//! Run: cargo test --manifest-path client-wpplugin/source/Cargo.toml --test webui_route_matrix

use serde_json::{json, Value};
use wptsall_client::test_support::WebUiTestHarness;

/// A server base that is guaranteed dead (nothing binds port 1 on loopback in
/// the test environment) so upstream-fetching routes fail fast.
const DEAD_SERVER: &str = "http://127.0.0.1:1";
const SESSION: &str = "sess-matrix";

fn error_code(body: &Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Enables the legacy server control plane for the duration of a scope.
/// `/api/components/refresh` and `/api/components/template` are declared
/// legacy routes (P0-LF-02): in the default local mode the dispatcher
/// rejects them with 404 LEGACY_CONTROL_PLANE_DISABLED BEFORE any handler
/// runs, so their handler contracts (session gate, validation, failure
/// paths) are only reachable with the opt-in gate enabled.
struct LegacyGateGuard {
    previous: Option<String>,
}
impl LegacyGateGuard {
    fn enable() -> Self {
        let previous = std::env::var("WPTSALL_USE_SERVER_CONTROL_PLANE").ok();
        std::env::set_var("WPTSALL_USE_SERVER_CONTROL_PLANE", "1");
        Self { previous }
    }
}
impl Drop for LegacyGateGuard {
    fn drop(&mut self) {
        match self.previous.as_deref() {
            Some(value) => std::env::set_var("WPTSALL_USE_SERVER_CONTROL_PLANE", value),
            None => std::env::remove_var("WPTSALL_USE_SERVER_CONTROL_PLANE"),
        }
    }
}

/// The legacy gate is a PROCESS-wide env var while tests in one binary run
/// in parallel; every test in this file therefore serializes on this lock
/// and explicitly declares the gate state it needs.
static MATRIX_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn matrix_lock() -> std::sync::MutexGuard<'static, ()> {
    MATRIX_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes the legacy gate env var (local mode) for the current scope.
struct LocalModeGuard;
impl LocalModeGuard {
    fn enforce() -> Self {
        std::env::remove_var("WPTSALL_USE_SERVER_CONTROL_PLANE");
        LocalModeGuard
    }
}

#[tokio::test]
async fn legacy_components_routes_blocked_in_local_mode() {
    let _lock = matrix_lock();
    let _mode = LocalModeGuard::enforce();
    // P0-LF-02 pin (router level): in the default local mode the legacy
    // refresh/template routes must be rejected before any handler runs.
    let harness = WebUiTestHarness::new(DEAD_SERVER, Some(SESSION))
        .await
        .unwrap();
    for path in ["/api/components/refresh", "/api/components/template"] {
        let resp = harness.post_json(path, json!({})).await.unwrap();
        assert!(
            resp.status_line.contains("404"),
            "{} in local mode must be 404, got {}",
            path,
            resp.status_line
        );
        assert_eq!(
            error_code(&resp.body),
            "LEGACY_CONTROL_PLANE_DISABLED",
            "path {}: {}",
            path,
            resp.body
        );
    }
}

#[tokio::test]
async fn session_gates_on_components_routes() {
    let _lock = matrix_lock();
    // No session token: refresh (catalog.rs) and template (downloads.rs)
    // must refuse before any upstream call. Legacy opt-in is required for
    // these routes to reach their handlers at all (see the local-mode pin
    // above).
    let _gate = LegacyGateGuard::enable();
    let harness = WebUiTestHarness::new(DEAD_SERVER, None).await.unwrap();

    let refresh = harness
        .post_json("/api/components/refresh", json!({}))
        .await
        .unwrap();
    assert!(
        refresh.status_line.contains("401"),
        "refresh without session must be 401, got {}",
        refresh.status_line
    );
    assert_eq!(error_code(&refresh.body), "SESSION_REQUIRED");

    let template = harness
        .post_json("/api/components/template", json!({ "component_id": "x" }))
        .await
        .unwrap();
    assert!(
        template.status_line.contains("401"),
        "template without session must be 401, got {}",
        template.status_line
    );
    assert_eq!(error_code(&template.body), "SESSION_REQUIRED");
}

#[tokio::test]
async fn components_template_validation_and_failure_paths() {
    let _lock = matrix_lock();
    let _gate = LegacyGateGuard::enable();
    let harness = WebUiTestHarness::new(DEAD_SERVER, Some(SESSION))
        .await
        .unwrap();

    // Data-driven validation matrix for POST /api/components/template.
    struct Case {
        body: Value,
        expect_code: &'static str,
    }
    let cases = vec![
        // Missing component_id -> explicit validation error.
        Case {
            body: json!({}),
            expect_code: "INVALID_COMPONENT_ID",
        },
        // Blank component_id -> same validation error.
        Case {
            body: json!({ "component_id": "   " }),
            expect_code: "INVALID_COMPONENT_ID",
        },
        // Valid id but the (dead) upstream fetch fails -> load failure code.
        Case {
            body: json!({ "component_id": "mx-ghost" }),
            expect_code: "COMPONENT_TEMPLATE_LOAD_FAILED",
        },
    ];
    for case in cases {
        let resp = harness
            .post_json("/api/components/template", case.body)
            .await
            .unwrap();
        assert_eq!(
            resp.body["success"],
            json!(false),
            "case {:?}: expected success=false, got {}",
            case.expect_code,
            resp.body
        );
        assert_eq!(
            error_code(&resp.body),
            case.expect_code,
            "case body: {}",
            resp.body
        );
    }
}

#[tokio::test]
async fn local_capabilities_matrix() {
    let _lock = matrix_lock();
    let _mode = LocalModeGuard::enforce();
    let harness = WebUiTestHarness::new(DEAD_SERVER, Some(SESSION))
        .await
        .unwrap();

    // Create a local component with an inline template through the API.
    let created = harness
        .create_local_component(json!({
            "id": "mx-text-v1",
            "name": "Matrix Text Component",
            "kind": "text",
            "template_json": {
                "id": "mx-template-v1",
                "name": "Matrix Template",
                "version": "1.0.0",
                "type": "text_translation",
                "request": { "method": "POST", "url": "https://example.com/mx" },
                "response": { "translated_text_path": "data.text" }
            }
        }))
        .await
        .unwrap();
    assert_eq!(created.body["success"], json!(true), "{}", created.body);
    assert_eq!(created.body["data"]["id"], json!("mx-text-v1"));

    // Local mode capabilities: derived from the local document only.
    let caps = harness
        .get_json("/api/components/capabilities")
        .await
        .unwrap();
    assert!(caps.status_line.contains("200"), "{}", caps.status_line);
    assert_eq!(caps.body["success"], json!(true));
    assert_eq!(caps.body["data"]["source"], json!("local"));

    let items = caps.body["data"]["components"]
        .as_array()
        .expect("components array");
    let mine = items
        .iter()
        .find(|item| item["id"] == json!("mx-text-v1"))
        .expect("created component must appear in local capabilities");
    assert_eq!(mine["enabled"], json!(true));
    assert_eq!(mine["available"], json!(true));
    assert_eq!(mine["has_local_template"], json!(true));
    assert_eq!(mine["source"], json!("local"));

    // The enabled component with a template must be a routing candidate.
    let format_map = caps.body["data"]["format_component_map"]
        .as_object()
        .expect("format map");
    let is_candidate = format_map.values().any(|components| {
        components
            .as_array()
            .map(|list| list.iter().any(|id| *id == json!("mx-text-v1")))
            .unwrap_or(false)
    });
    assert!(
        is_candidate,
        "format map: {}",
        caps.body["data"]["format_component_map"]
    );
}

#[tokio::test]
async fn rule_component_bindings_matrix() {
    let _lock = matrix_lock();
    let _mode = LocalModeGuard::enforce();
    let harness = WebUiTestHarness::new(DEAD_SERVER, Some(SESSION))
        .await
        .unwrap();

    // Rule bindings require an existing local component: create one first.
    let created = harness
        .create_local_component(json!({
            "id": "mx-rules-v1",
            "name": "Matrix Rules Component",
            "kind": "text",
            "template_json": {
                "id": "mx-rules-template-v1",
                "name": "Matrix Rules Template",
                "version": "1.0.0",
                "type": "text_translation",
                "request": { "method": "POST", "url": "https://example.com/mx-rules" },
                "response": { "translated_text_path": "data.text" }
            }
        }))
        .await
        .unwrap();
    assert_eq!(created.body["success"], json!(true), "{}", created.body);

    // Valid global default binding round-trips.
    let upsert = harness
        .upsert_rule_component_binding("global", None, "plain_text", "mx-rules-v1")
        .await
        .unwrap();
    assert_eq!(upsert.body["success"], json!(true), "{}", upsert.body);
    assert_eq!(upsert.body["data"]["scope"], json!("global"));
    assert_eq!(upsert.body["data"]["slot_key"], json!("plain_text"));
    assert_eq!(upsert.body["data"]["component_id"], json!("mx-rules-v1"));

    // Data-driven invalid matrix: each bad request maps to its pinned code.
    struct Case {
        body: Value,
        expect_code: &'static str,
    }
    let cases = vec![
        Case {
            body: json!({ "scope": "bogus", "slot_key": "plain_text", "component_id": "mx-rules-v1" }),
            expect_code: "INVALID_SCOPE",
        },
        Case {
            body: json!({ "scope": "plugin", "slot_key": "plain_text", "component_id": "mx-rules-v1" }),
            expect_code: "INVALID_SCOPE_KEY",
        },
        Case {
            body: json!({ "scope": "relation", "scope_key": "0", "slot_key": "plain_text", "component_id": "mx-rules-v1" }),
            expect_code: "INVALID_SCOPE_KEY",
        },
        Case {
            body: json!({ "scope": "global", "slot_key": "not-a-format", "component_id": "mx-rules-v1" }),
            expect_code: "INVALID_SLOT_KEY",
        },
        Case {
            body: json!({ "scope": "global", "slot_key": "slug" }),
            expect_code: "INVALID_COMPONENT_ID",
        },
        Case {
            body: json!({ "scope": "global", "slot_key": "slug", "component_id": "ghost-component" }),
            expect_code: "COMPONENT_NOT_FOUND",
        },
    ];
    for case in cases {
        let resp = harness
            .post_json("/api/rule-component-bindings/upsert", case.body)
            .await
            .unwrap();
        assert_eq!(
            resp.body["success"],
            json!(false),
            "case {:?}: expected success=false, got {}",
            case.expect_code,
            resp.body
        );
        assert_eq!(
            error_code(&resp.body),
            case.expect_code,
            "case body: {}",
            resp.body
        );
    }

    // Delete round-trip: first delete removes, second reports not-present.
    let delete = harness
        .post_json(
            "/api/rule-component-bindings/delete",
            json!({ "scope": "global", "slot_key": "plain_text" }),
        )
        .await
        .unwrap();
    assert_eq!(delete.body["success"], json!(true), "{}", delete.body);
    assert_eq!(delete.body["data"]["deleted"], json!(true));

    let delete_again = harness
        .post_json(
            "/api/rule-component-bindings/delete",
            json!({ "scope": "global", "slot_key": "plain_text" }),
        )
        .await
        .unwrap();
    assert_eq!(delete_again.body["data"]["deleted"], json!(false));
}

#[tokio::test]
async fn domain_tokens_matrix() {
    let _lock = matrix_lock();
    let _mode = LocalModeGuard::enforce();
    let harness = WebUiTestHarness::new(DEAD_SERVER, Some(SESSION))
        .await
        .unwrap();

    // Valid upsert round-trips (persist + normalized key in response).
    let token = "matrix-token-abcdef";
    let upsert = harness
        .upsert_domain_binding("http://127.0.0.1:9081/", token, "matrix-secret")
        .await
        .unwrap();
    assert_eq!(upsert.body["success"], json!(true), "{}", upsert.body);
    assert_eq!(
        upsert.body["data"]["api_base_url"],
        json!("http://127.0.0.1:9081")
    );
    assert_eq!(upsert.body["data"]["token_len"], json!(token.len()));
    assert_eq!(upsert.body["data"]["route_secret_set"], json!(true));

    // Data-driven invalid matrix.
    struct Case {
        body: Value,
        expect_code: &'static str,
    }
    let cases = vec![
        Case {
            body: json!({}),
            expect_code: "INVALID_API_BASE_URL",
        },
        Case {
            // New binding without a token.
            body: json!({ "api_base_url": "http://127.0.0.1:9082" }),
            expect_code: "INVALID_WP_CLIENT_TOKEN",
        },
    ];
    for case in cases {
        let resp = harness
            .post_json("/api/domain-tokens/upsert", case.body)
            .await
            .unwrap();
        assert_eq!(
            resp.body["success"],
            json!(false),
            "case {:?}: expected success=false, got {}",
            case.expect_code,
            resp.body
        );
        assert_eq!(
            error_code(&resp.body),
            case.expect_code,
            "case body: {}",
            resp.body
        );
    }

    // Delete round-trip.
    let delete = harness
        .post_json(
            "/api/domain-tokens/delete",
            json!({ "api_base_url": "http://127.0.0.1:9081" }),
        )
        .await
        .unwrap();
    assert_eq!(delete.body["success"], json!(true), "{}", delete.body);
    assert_eq!(delete.body["data"]["deleted"], json!(true));

    let delete_again = harness
        .post_json(
            "/api/domain-tokens/delete",
            json!({ "api_base_url": "http://127.0.0.1:9081" }),
        )
        .await
        .unwrap();
    assert_eq!(delete_again.body["data"]["deleted"], json!(false));
}

#[tokio::test]
async fn dispatcher_not_found_and_wrong_method() {
    let _lock = matrix_lock();
    let _mode = LocalModeGuard::enforce();
    let harness = WebUiTestHarness::new(DEAD_SERVER, Some(SESSION))
        .await
        .unwrap();

    // Unknown path -> 404 NOT_FOUND.
    let unknown = harness
        .get_json("/api/definitely-not-a-route")
        .await
        .unwrap();
    assert!(
        unknown.status_line.contains("404"),
        "unknown path must 404, got {}",
        unknown.status_line
    );
    assert_eq!(error_code(&unknown.body), "NOT_FOUND");

    // Known path with the wrong method falls through the dispatcher to 404.
    let wrong_method = harness.get_json("/api/components/template").await.unwrap();
    assert!(
        wrong_method.status_line.contains("404"),
        "wrong method must fall through to 404, got {}",
        wrong_method.status_line
    );
}
