//! G-07/G-19 route-coverage clusters plus the legacy control-plane gates —
//! every assertion goes through the real dispatcher harness
//! (`WebUiTestHarness` real-TCP / `routes::handle_web_ui_connection`), so
//! the CSRF check, the legacy classifier, and the route match all run.

use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::web_ui::test_support::WebUiTestHarness;

// catalog: WEBUI-API-GET-api-access-control
// catalog: WEBUI-API-GET-api-cloud-api-types
// catalog: WEBUI-API-GET-api-components-local
// catalog: WEBUI-API-GET-api-discovery-tasks
// catalog: WEBUI-API-GET-api-jobs
// catalog: WEBUI-API-GET-api-log-settings
// catalog: WEBUI-API-GET-api-platform-entitlements
// catalog: WEBUI-API-GET-api-platform-products
// catalog: WEBUI-API-GET-api-proxy-profiles
// catalog: WEBUI-API-GET-api-stats-overview
// catalog: WEBUI-API-GET-api-sync-pairs
// catalog: WEBUI-API-GET-api-translations
// catalog: WEBUI-API-GET-api-update-check
// catalog: WEBUI-API-GET-api-vendor-keys
// catalog: WEBUI-API-GET-api-vendor-oauth
// catalog: WEBUI-API-GET-api-vendors
// catalog: WEBUI-API-GET-api-worker-config
// catalog: WEBUI-API-GET-api-wp-translation-providers
// catalog: WEBUI-API-PREFIX-api-sync-pairs
// catalog: WEBUI-API-PREFIX-api-vendor-keys
// catalog: WEBUI-API-PREFIX-api-vendor-oauth
// catalog: WEBUI-API-PREFIX-api-proxy-profiles
// catalog: WEBUI-API-PREFIX-api-translations
// catalog: WEBUI-API-POST-api-components-bindings-delete
// catalog: WEBUI-API-POST-api-components-bindings-upsert
// catalog: WEBUI-API-POST-api-components-local
// catalog: WEBUI-API-POST-api-discovery-tasks-bootstrap
// catalog: WEBUI-API-POST-api-domain-tokens-delete
// catalog: WEBUI-API-POST-api-domain-tokens-upsert
// catalog: WEBUI-API-POST-api-integrations-pack-import
// catalog: WEBUI-API-POST-api-perform-update
// catalog: WEBUI-API-POST-api-providers-test
// catalog: WEBUI-API-POST-api-proxy-profiles
// catalog: WEBUI-API-POST-api-rule-component-bindings-delete
// catalog: WEBUI-API-POST-api-rule-component-bindings-upsert
// catalog: WEBUI-API-POST-api-sync-pairs
// catalog: WEBUI-API-POST-api-task-type-components-delete
// catalog: WEBUI-API-POST-api-task-type-components-upsert
// catalog: WEBUI-API-POST-api-vendor-keys
// catalog: WEBUI-API-POST-api-vendor-oauth
// catalog: WEBUI-API-POST-api-worker-stop
// oracle: L2
// (Apparatus-depth claim, file-granular by catalog design: every route
// listed above is asserted in THIS file through the real dispatcher
// harness (real TCP -> routes::handle_web_ui_connection -> CSRF ->
// classifier -> route match) — read/prefix routes via read_model_routes_
// all_list + the legacy-gate cluster + CRUD list-absence checks; write
// routes via full create/update/delete roundtrips, harness-mediated
// upserts (bindings / task-type / rule-component), and defined
// validation-error contracts (pack import, providers test, translations
// retry) plus the perform-update fails-safe contracts (unreachable update
// source → UPDATE_CHECK_FAILED without wedging the in-progress guard;
// already-up-to-date manifest → ALREADY_UP_TO_DATE with zero artifact
// downloads — the destructive swap itself is intentionally not exercised).
// Deliberately NOT claimed: the legacy-gated install-from-server /
// components-refresh / logout / oauth-start — no honest write oracle
// exists for them in local-first mode; they stay partial in the catalog.)

fn text_template_json() -> serde_json::Value {
    serde_json::json!({
        "id": "tpl-cov-test-v1",
        "name": "Coverage Test Template",
        "version": "1.0.0",
        "type": "text_translation",
        "auth": null,
        "request": {"method": "POST", "url": "http://127.0.0.1:1/translate"},
        "response": {"translated_text_path": "data.text"}
    })
}

// -----------------------------------------------------------------------
// G-19: dispatcher-level gates
// -----------------------------------------------------------------------

#[tokio::test]
async fn csrf_origin_negative_rejected() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let (status_line, raw_body) = harness
        .send_raw_request(
            "POST",
            "/api/vendor-keys",
            Some(br#"{"vendor_id":"x"}"#),
            &[
                ("Origin", "http://evil.example"),
                ("Content-Type", "application/json"),
            ],
        )
        .await
        .unwrap();
    assert!(
        status_line.starts_with("HTTP/1.1 403"),
        "cross-origin POST must be blocked, got: {} {}",
        status_line,
        raw_body
    );
    let body: serde_json::Value = serde_json::from_str(&raw_body).unwrap();
    assert_eq!(body["error"]["code"], "CSRF_REJECTED");
}

#[tokio::test]
async fn not_found_catch_all_and_legacy_gates() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // Unknown API route falls through to the JSON NOT_FOUND catch-all.
    let res = harness
        .get_json("/api/definitely-not-a-route")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 404"),
        "got: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");

    // Legacy server-control-plane routes are 404'd by the classifier before
    // any handler runs (local-first mode, gate disabled).
    let legacy_gets = [
        "/api/platform/products",
        "/api/platform/entitlements",
        "/api/vendors",
        "/api/wp-translation-providers",
        "/api/cloud-api-types",
    ];
    for path in legacy_gets {
        let res = harness.get_json(path).await.unwrap();
        assert!(
            res.status_line.starts_with("HTTP/1.1 404"),
            "legacy GET {} must be blocked, got: {}",
            path,
            res.status_line
        );
        assert_eq!(
            res.body["error"]["code"], "LEGACY_CONTROL_PLANE_DISABLED",
            "legacy GET {}: {}",
            path, res.raw_body
        );
    }
    let legacy_posts = [
        "/api/components/refresh",
        "/api/logout",
        "/api/oauth/start",
        "/api/components/local/install-from-server",
    ];
    for path in legacy_posts {
        let res = harness.post_json(path, json!({})).await.unwrap();
        assert!(
            res.status_line.starts_with("HTTP/1.1 404"),
            "legacy POST {} must be blocked, got: {}",
            path,
            res.status_line
        );
        assert_eq!(
            res.body["error"]["code"], "LEGACY_CONTROL_PLANE_DISABLED",
            "legacy POST {}: {}",
            path, res.raw_body
        );
    }
}

// -----------------------------------------------------------------------
// Statics / health / worker
// -----------------------------------------------------------------------

#[tokio::test]
async fn statics_health_and_worker_routes() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let (status, body) = harness
        .send_raw_request("GET", "/health", None, &[])
        .await
        .unwrap();
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "health: {} {}",
        status,
        body
    );
    assert!(body.contains("\"status\""), "health body: {}", body);
    // UI-27-02/P2-3: /health must carry the on-disk UI bundle version so the
    // About tab can pre-display versions without an update-server round trip.
    // 批 O3 疤: /health is wrapped in the standard {"success": true, "data": …}
    // envelope (it used to be flat — the only outlier, which isOk() rejected).
    let health: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        health["success"].as_bool() == Some(true),
        "health must use the standard success envelope: {}",
        body
    );
    assert!(
        health["data"]["ui_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "health must report a non-empty ui_version: {}",
        body
    );

    let (status, body) = harness
        .send_raw_request("GET", "/", None, &[])
        .await
        .unwrap();
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "index: {} {}",
        status,
        body
    );
    assert!(
        body.to_ascii_lowercase().contains("<html"),
        "index must serve the HTML shell: {}",
        &body[..body.len().min(120)]
    );

    let (status, _body) = harness
        .send_raw_request("GET", "/favicon.svg", None, &[])
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 200"), "favicon: {}", status);

    let res = harness.get_json("/api/worker/config").await.unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "worker config: {}",
        res.raw_body
    );

    // Stopping an idle worker is a normal no-op success.
    let res = harness
        .post_json("/api/worker/stop", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "worker stop: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["running"], false);
}

// -----------------------------------------------------------------------
// Read models
// -----------------------------------------------------------------------

#[tokio::test]
async fn read_model_routes_all_list() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    for path in [
        "/api/vendor-keys",
        "/api/vendor-oauth",
        "/api/proxy-profiles",
        "/api/translations",
        "/api/jobs",
        "/api/discovery-tasks",
        "/api/stats/overview",
        "/api/log-settings",
        "/api/access-control",
    ] {
        let res = harness.get_json(path).await.unwrap();
        assert!(
            res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
            "read model {} failed: {}",
            path,
            res.raw_body
        );
    }
}

// -----------------------------------------------------------------------
// Integrations CRUD by id (vendor keys / oauth configs / proxy profiles)
// -----------------------------------------------------------------------

#[tokio::test]
async fn integrations_crud_by_id_roundtrip() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // --- vendor keys ---
    let res = harness
        .post_json(
            "/api/vendor-keys",
            json!({
                "id": "vk-cov-1",
                "vendor_id": "vendor-cov",
                "label": "coverage key",
                "auth_values": {"api_key": "sk-cov"}
            }),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["id"] == "vk-cov-1",
        "vendor key create: {}",
        res.raw_body
    );
    let res = harness
        .put_json("/api/vendor-keys/vk-cov-1", json!({"label": "renamed"}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "vendor key update: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/vendor-keys").await.unwrap();
    assert!(res.raw_body.contains("vk-cov-1"));
    let res = harness
        .delete_json("/api/vendor-keys/vk-cov-1")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["deleted"] == true,
        "vendor key delete: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/vendor-keys").await.unwrap();
    assert!(
        !res.raw_body.contains("vk-cov-1"),
        "deleted key must leave the list"
    );

    // --- vendor oauth configs ---
    let res = harness
        .post_json(
            "/api/vendor-oauth",
            json!({
                "id": "oa-cov-1",
                "vendor_id": "vendor-cov",
                "label": "coverage oauth",
                "grant_type": "client_credentials",
                "token_url": "http://127.0.0.1:1/token"
            }),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["id"] == "oa-cov-1",
        "oauth create: {}",
        res.raw_body
    );
    let res = harness
        .put_json("/api/vendor-oauth/oa-cov-1", json!({"label": "renamed"}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "oauth update: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/vendor-oauth").await.unwrap();
    assert!(res.raw_body.contains("oa-cov-1"));
    let res = harness
        .delete_json("/api/vendor-oauth/oa-cov-1")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["deleted"] == true,
        "oauth delete: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/vendor-oauth").await.unwrap();
    assert!(!res.raw_body.contains("oa-cov-1"));

    // --- proxy profiles ---
    let res = harness
        .post_json(
            "/api/proxy-profiles",
            json!({
                "id": "px-cov-1",
                "name": "coverage proxy",
                "protocol": "http",
                "host": "127.0.0.1",
                "port": 8080
            }),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["id"] == "px-cov-1",
        "proxy create: {}",
        res.raw_body
    );
    let res = harness
        .put_json("/api/proxy-profiles/px-cov-1", json!({"name": "renamed"}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "proxy update: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/proxy-profiles").await.unwrap();
    assert!(res.raw_body.contains("px-cov-1"));
    let res = harness
        .delete_json("/api/proxy-profiles/px-cov-1")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["deleted"] == true,
        "proxy delete: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/proxy-profiles").await.unwrap();
    assert!(!res.raw_body.contains("px-cov-1"));
}

// -----------------------------------------------------------------------
// Binding upsert-then-delete roundtrips (all four binding families)
// -----------------------------------------------------------------------

#[tokio::test]
async fn binding_upsert_delete_roundtrips() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // All three binding families validate that the referenced component
    // exists in the local registry — seed them first.
    for (id, name) in [
        ("comp-cov-bind", "Coverage Bind Component"),
        ("comp-cov-tt", "Coverage TaskType Component"),
        ("comp-cov-rule", "Coverage Rule Component"),
    ] {
        let res = harness
            .create_local_component(json!({
                "id": id,
                "name": name,
                "vendor_id": "vendor-cov",
                "kind": "text",
                "enabled": true,
                "template_json": text_template_json()
            }))
            .await
            .unwrap();
        assert!(
            res.status_line.starts_with("HTTP/1.1 200"),
            "seed component {id}: {}",
            res.raw_body
        );
    }

    // Component bindings.
    let res = harness
        .post_json(
            "/api/components/bindings/upsert",
            json!({"component_id": "comp-cov-bind", "auth": {"api_key": "k"}}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "binding upsert: {}",
        res.raw_body
    );
    let res = harness
        .post_json(
            "/api/components/bindings/delete",
            json!({"component_id": "comp-cov-bind"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "binding delete: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["deleted"], true);

    // Task-type bindings.
    let res = harness
        .upsert_task_type_binding("text", "comp-cov-tt")
        .await
        .unwrap();
    assert!(res.status_line.starts_with("HTTP/1.1 200"));
    let res = harness
        .post_json(
            "/api/task-type-components/delete",
            json!({"task_type": "text"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "task-type delete: {}",
        res.raw_body
    );

    // Rule-component bindings.
    let res = harness
        .upsert_rule_component_binding("global", None, "plain_text", "comp-cov-rule")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "{}",
        res.raw_body
    );
    let res = harness
        .post_json(
            "/api/rule-component-bindings/delete",
            json!({"scope": "global", "slot_key": "plain_text"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "rule binding delete: {}",
        res.raw_body
    );

    // Domain tokens.
    let res = harness
        .upsert_domain_binding("https://cov-bind.example.com", "tok-cov", "sec-cov")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "{}",
        res.raw_body
    );
    let res = harness
        .post_json(
            "/api/domain-tokens/delete",
            json!({"api_base_url": "https://cov-bind.example.com"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "domain token delete: {}",
        res.raw_body
    );
}

// -----------------------------------------------------------------------
// Local component version lifecycle + quick-test error path
// -----------------------------------------------------------------------

#[tokio::test]
async fn local_component_version_lifecycle_and_quick_test() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let res = harness
        .create_local_component(json!({
            "id": "comp-cov-versions",
            "name": "Coverage Versions Component",
            "vendor_id": "vendor-cov",
            "kind": "text",
            "enabled": true,
            "template_json": text_template_json()
        }))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "create: {}",
        res.raw_body
    );

    // Create version.
    let res = harness
        .post_json(
            "/api/components/local/comp-cov-versions/versions",
            json!({"version": "v9", "remarks": "first"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["version"] == "v9",
        "version create: {}",
        res.raw_body
    );

    // Update version.
    let res = harness
        .put_json(
            "/api/components/local/comp-cov-versions/versions/v9",
            json!({"remarks": "updated"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "version update: {}",
        res.raw_body
    );

    // Quick-test without auth fails fast with the defined INVALID_AUTH error
    // (route exercised; no live provider needed).
    let res = harness
        .post_json(
            "/api/components/local/comp-cov-versions/quick-test",
            json!({"text": "hello"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 4"),
        "quick-test without auth must be a validation error, got: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "INVALID_AUTH");

    // Delete version.
    let res = harness
        .delete_json("/api/components/local/comp-cov-versions/versions/v9")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["deleted"] == true,
        "version delete: {}",
        res.raw_body
    );

    // Delete component, then it disappears from the list.
    let res = harness
        .delete_json("/api/components/local/comp-cov-versions")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["data"]["deleted"] == true,
        "component delete: {}",
        res.raw_body
    );
    let res = harness.get_json("/api/components/local").await.unwrap();
    assert!(
        !res.raw_body.contains("comp-cov-versions"),
        "deleted component must leave the local list: {}",
        res.raw_body
    );
}

// -----------------------------------------------------------------------
// Sync pairs: HTTP DELETE by id + credentials list
// -----------------------------------------------------------------------

#[tokio::test]
async fn sync_pairs_http_delete_and_credentials_list() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    for (domain, token, secret) in [
        ("https://cov-src.example.com", "tok-src", "sec-src"),
        ("https://cov-tgt.example.com", "tok-tgt", "sec-tgt"),
    ] {
        let seeded = harness
            .post_json(
                "/api/domain-tokens/upsert",
                json!({
                    "api_base_url": domain,
                    "wp_client_token": token,
                    "route_secret": secret,
                    "plugin_identity": "wpmmcc",
                }),
            )
            .await
            .unwrap();
        assert!(
            seeded.status_line.starts_with("HTTP/1.1 200"),
            "seed upsert failed: {}",
            seeded.raw_body
        );
    }

    let res = harness
        .post_json(
            "/api/sync-pairs",
            json!({
                "name": "Cov pair",
                "source_domain": "https://cov-src.example.com",
                "target_domain": "https://cov-tgt.example.com",
                "sync_mode": "sync_only",
                "post_types": ["post"]
            }),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "pair create: {}",
        res.raw_body
    );
    let pair_id = res.body["data"]["pair"]["id"].as_str().unwrap().to_string();

    let res = harness
        .get_json("/api/sync-pairs/credentials")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "credentials list: {}",
        res.raw_body
    );

    // The dynamic DELETE-by-id route.
    let res = harness
        .delete_json(&format!("/api/sync-pairs/{pair_id}"))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "pair delete: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["deleted"], true);

    let res = harness.get_json("/api/sync-pairs").await.unwrap();
    assert!(
        !res.raw_body.contains(&pair_id),
        "deleted pair must leave the list: {}",
        res.raw_body
    );
}

// -----------------------------------------------------------------------
// Operations: retry / bootstrap / providers probe / pack import / update check
// -----------------------------------------------------------------------

#[tokio::test]
async fn operations_retry_bootstrap_providers_pack_import() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    // Retry of an unknown translation id is a RETRY_FAILED error.
    let res = harness
        .post_json("/api/translations/999999/retry", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 4"),
        "unknown retry must fail, got: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "RETRY_FAILED");

    // Bootstrap with no bound sites succeeds with empty arrays.
    let res = harness
        .post_json("/api/discovery-tasks/bootstrap", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "bootstrap: {}",
        res.raw_body
    );
    for key in ["synced", "skipped", "failed"] {
        assert_eq!(
            res.body["data"][key].as_array().map(Vec::len),
            Some(0),
            "bootstrap {} must be empty without bound sites: {}",
            key,
            res.raw_body
        );
    }

    // Provider probe with no credentials is the INVALID_AUTH validation error.
    let res = harness
        .post_json("/api/providers/test", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 4"),
        "provider probe without auth must fail validation, got: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "INVALID_AUTH");

    // Y-6: a probe with credentials but no vendor anchor (empty vendor_id,
    // no reverse-lookup key_id) fails closed as VENDOR_UNRESOLVED — the
    // literal "unknown" attribution fallback is gone, and the request
    // never leaves the box.
    let res = harness
        .post_json(
            "/api/providers/test",
            json!({ "auth_values": { "api_key": "sk-not-a-real-key" } }),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 4"),
        "provider probe without a vendor anchor must fail closed, got: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "VENDOR_UNRESOLVED");

    // Importing garbage bytes is rejected at the pack parser.
    let (status, raw_body) = harness
        .send_raw_request(
            "POST",
            "/api/integrations/pack/import",
            Some(b"not-a-pack"),
            &[
                ("Origin", "http://127.0.0.1:8977"),
                ("Content-Type", "application/json"),
            ],
        )
        .await
        .unwrap();
    assert!(
        status.starts_with("HTTP/1.1 4"),
        "garbage pack import must fail, got: {} {}",
        status,
        raw_body
    );
    let body: serde_json::Value = serde_json::from_str(&raw_body).unwrap();
    assert_eq!(body["error"]["code"], "INVALID_INTEGRATION_PACK");
}

#[tokio::test]
async fn update_check_always_answers_json() {
    // Point server_base at a closed loopback port so the update check fails
    // fast (connection refused) instead of touching any real endpoint.
    let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_addr = closed_listener.local_addr().unwrap();
    drop(closed_listener);
    let harness = WebUiTestHarness::new(&format!("http://{}", closed_addr), None)
        .await
        .unwrap();

    let res = harness.get_json("/api/update-check").await.unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "update-check must always answer 200 JSON, got: {}",
        res.raw_body
    );
    assert!(
        res.body.get("success").is_some(),
        "update-check must carry a success key: {}",
        res.raw_body
    );
    // UI-27-03: a failed check must still return local versions in `data`
    // so the About tab can render "you are on vX" alongside the error.
    assert_eq!(
        res.body["success"], false,
        "closed port must fail: {}",
        res.raw_body
    );
    assert_eq!(
        res.body["error"]["code"], "UPDATE_CHECK_FAILED",
        "update-check failure code: {}",
        res.raw_body
    );
    assert!(
        res.body["data"]["current_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "update-check failure must carry data.current_version: {}",
        res.raw_body
    );
    assert!(
        res.body["data"]["ui_current_version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "update-check failure must carry data.ui_current_version: {}",
        res.raw_body
    );

    // NOTE: POST /api/perform-update is covered by the two fails-safe
    // contracts below (unreachable source / already-up-to-date) — the
    // destructive swap itself is deliberately not exercised in tests.
}

// -----------------------------------------------------------------------
// POST /api/perform-update — non-destructive fails-safe contracts
// -----------------------------------------------------------------------

/// Minimal loopback HTTP server serving a releases manifest at
/// `/api/v1/client/releases` and 404 for everything else (including the
/// detached `.minisig`), recording every requested path so tests can prove
/// no artifact downloads were attempted.
async fn spawn_releases_manifest_server(latest_version: &str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requested_paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let manifest = json!({
        "data": {
            "schema_version": 1,
            "products": {
                "client-wpplugin": {
                    "latest_version": latest_version,
                    "min_supported_version": latest_version,
                    "release_notes_url": "",
                    "download_url_template": "",
                    "signature_url_template": "",
                    "mandatory": false,
                }
            }
        }
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();

    let seen = Arc::clone(&requested_paths);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            // request head only — GETs carry no body
            loop {
                match socket.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let head = String::from_utf8_lossy(&buf).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            seen.lock().unwrap().push(path.clone());
            let (response, body): (String, &[u8]) = if path == "/api/v1/client/releases" {
                (
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        manifest_bytes.len()
                    ),
                    &manifest_bytes,
                )
            } else {
                (
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string(),
                    &[],
                )
            };
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(body).await;
        }
    });
    (format!("http://{addr}"), requested_paths)
}

#[tokio::test]
async fn perform_update_fails_safe_when_update_source_unreachable() {
    // Point the releases manifest at a closed loopback port so the update
    // check fails fast (connection refused) instead of touching any real
    // endpoint — same closed-port technique as update_check above.
    let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_addr = closed_listener.local_addr().unwrap();
    drop(closed_listener);
    // Pin the manifest URL so an ambient WPTSALL_RELEASES_URL cannot reroute
    // the check away from the closed port.
    let _releases_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_RELEASES_URL",
        format!("http://{closed_addr}/api/v1/client/releases"),
    );
    let harness = WebUiTestHarness::new(&format!("http://{closed_addr}"), None)
        .await
        .unwrap();

    let res = harness
        .post_json("/api/perform-update", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 400"),
        "unreachable update source must fail with a client error, got: {}",
        res.raw_body
    );
    assert_eq!(res.body["success"], false, "raw: {}", res.raw_body);
    assert_eq!(
        res.body["error"]["code"], "UPDATE_CHECK_FAILED",
        "perform-update failure code: {}",
        res.raw_body
    );

    // The failure happened before the in-progress flag was set: a retry must
    // take the same check path, not wedge on UPDATE_IN_PROGRESS.
    let retry = harness
        .post_json("/api/perform-update", json!({}))
        .await
        .unwrap();
    assert_eq!(
        retry.body["error"]["code"], "UPDATE_CHECK_FAILED",
        "retry must not be blocked by the concurrent-update guard: {}",
        retry.raw_body
    );
}

#[tokio::test]
async fn perform_update_refuses_and_downloads_nothing_when_already_up_to_date() {
    // Manifest whose latest binary version equals the running one (and no
    // UI product) → update_available=false → the destructive endpoint must
    // refuse with ALREADY_UP_TO_DATE before any download/swap step.
    let current = env!("CARGO_PKG_VERSION");
    let (base, requested) = spawn_releases_manifest_server(current).await;
    // Route the manifest fetch at the mock server and accept the unsigned
    // manifest (404 on .minisig) — the documented local/dev escape hatch.
    let _releases_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_RELEASES_URL",
        format!("{base}/api/v1/client/releases"),
    );
    let _unsigned_guard = crate::db::TestEnvVarGuard::set("WPTSALL_ALLOW_UNSIGNED_MANIFEST", "1");
    let harness = WebUiTestHarness::new(&base, None).await.unwrap();

    let res = harness
        .post_json("/api/perform-update", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 400"),
        "already-up-to-date refusal must be a client error, got: {}",
        res.raw_body
    );
    assert_eq!(res.body["success"], false, "raw: {}", res.raw_body);
    assert_eq!(
        res.body["error"]["code"], "ALREADY_UP_TO_DATE",
        "perform-update refusal code: {}",
        res.raw_body
    );

    // Non-destructive proof: only the manifest and its .minisig were
    // fetched — no artifact, kit, or signature downloads were attempted.
    let seen = requested.lock().unwrap().clone();
    assert!(
        seen.contains(&"/api/v1/client/releases".to_string()),
        "manifest must have been fetched, saw: {seen:?}"
    );
    assert!(
        seen.iter()
            .all(|p| { p == "/api/v1/client/releases" || p == "/api/v1/client/releases.minisig" }),
        "unexpected download attempt against the update source: {seen:?}"
    );
}

#[tokio::test]
async fn access_control_get_reports_actual_listen_port() {
    // UI-27-02: the Access tab must render the real bind port. The endpoint
    // derives it from env (WPTSALL_WEB_UI_PORT / WPTSALL_WEB_UI_BIND → 8977);
    // the contract here is that the field exists and is a plausible port, not
    // a specific value (env is process-global and tests run in parallel).
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let res = harness.get_json("/api/access-control").await.unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200") && res.body["success"] == true,
        "access-control get: {}",
        res.raw_body
    );
    let port = res.body["data"]["current_port"].as_u64();
    assert!(
        port.is_some_and(|p| p > 0 && p <= 65535),
        "access-control must report data.current_port (u16), got: {}",
        res.raw_body
    );
}
