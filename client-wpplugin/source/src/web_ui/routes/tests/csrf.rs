//! Method-level Origin enforcement through the real TCP dispatcher.
// catalog: WEBUI-MOD-web-ui-routes-rs
// catalog: WEBUI-API-POST-api-vendor-keys
// catalog: WEBUI-API-PREFIX-api-vendor-keys
// oracle: L2

use super::*;
use crate::web_ui::test_support::WebUiTestHarness;

const SERVER_BASE: &str = "http://127.0.0.1:8787";
const KEY_PATH: &str = "/api/vendor-keys/cli04-original";

async fn seeded_harness() -> WebUiTestHarness {
    let harness = WebUiTestHarness::new(SERVER_BASE, None).await.unwrap();
    let created = harness
        .post_json(
            "/api/vendor-keys",
            json!({
                "id": "cli04-original",
                "vendor_id": "cli04-mock",
                "label": "original",
                "auth_values": {"api_key": "cli04-fake-key"}
            }),
        )
        .await
        .unwrap();
    assert!(created.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(created.body["data"]["id"], "cli04-original");
    harness
}

fn persisted_snapshot(harness: &WebUiTestHarness) -> (Vec<u8>, String) {
    let file = std::fs::read(vendor_keys_path()).unwrap();
    let conn = crate::db::open_db(harness.db_path.to_str().unwrap()).unwrap();
    let row = crate::db::system::get_system_config_checked(&conn, "vendor_keys_doc")
        .unwrap()
        .expect("seed must persist a real database row");
    (file, row)
}

async fn assert_cross_origin_write_denied(method: &str) {
    let harness = seeded_harness().await;
    let before = persisted_snapshot(&harness);
    let (status, raw_body) = harness
        .send_raw_request(
            method,
            KEY_PATH,
            Some(br#"{"label":"attacker-changed"}"#),
            &[("Origin", "https://cli04-attacker.invalid")],
        )
        .await
        .unwrap();
    let after = persisted_snapshot(&harness);
    let body: Value = serde_json::from_str(&raw_body).unwrap();
    assert!(
        status.starts_with("HTTP/1.1 403")
            && body["error"]["code"] == "CSRF_REJECTED"
            && before == after,
        "{method}: status={status}, file_preserved={}, row_preserved={}",
        before.0 == after.0,
        before.1 == after.1
    );
}

#[tokio::test]
async fn cli04_cross_origin_put_cannot_change_vendor_key() {
    assert_cross_origin_write_denied("PUT").await;
}

#[tokio::test]
async fn cli04_cross_origin_delete_cannot_remove_vendor_key() {
    assert_cross_origin_write_denied("DELETE").await;
}

#[tokio::test]
async fn cli04_unsafe_methods_reject_foreign_and_malformed_origins() {
    let harness = seeded_harness().await;
    let before = persisted_snapshot(&harness);
    for method in [
        "POST", "PUT", "DELETE", "PATCH", "TRACE", "CONNECT", "PROPFIND", "CUSTOM",
    ] {
        for origin in [
            "https://cli04-attacker.invalid",
            "http://127.0.0.1.evil.invalid",
            "http://localhost.evil.invalid",
            "null",
            "",
            "not-an-origin",
        ] {
            let (status, raw_body) = harness
                .send_raw_request(
                    method,
                    KEY_PATH,
                    Some(br#"{"label":"attacker-changed"}"#),
                    &[("oRiGiN", origin)],
                )
                .await
                .unwrap();
            let body: Value = serde_json::from_str(&raw_body).unwrap();
            assert!(
                status.starts_with("HTTP/1.1 403"),
                "{method} Origin={origin:?}: {status}"
            );
            assert_eq!(body["error"]["code"], "CSRF_REJECTED");
            assert_eq!(persisted_snapshot(&harness), before);
        }
    }
}

fn saved_vendor_keys(harness: &WebUiTestHarness) -> VendorKeysDoc {
    let conn = crate::db::open_db(harness.db_path.to_str().unwrap()).unwrap();
    crate::db::vendor::load_vendor_keys_doc(&conn)
}

#[tokio::test]
async fn cli04_local_tools_and_loopback_origins_can_write() {
    for origin in [
        None,
        Some("http://127.0.0.1:8977"),
        Some("https://LOCALHOST:3000"),
        Some("http://[::1]:8977"),
    ] {
        let harness = seeded_harness().await;
        let headers: Vec<(&str, &str)> =
            origin.into_iter().map(|value| ("Origin", value)).collect();
        let (status, _) = harness
            .send_raw_request(
                "PUT",
                KEY_PATH,
                Some(br#"{"label":"allowed-write"}"#),
                &headers,
            )
            .await
            .unwrap();
        assert!(status.starts_with("HTTP/1.1 200"), "{origin:?}: {status}");
        assert_eq!(
            crate::bindings::load_vendor_keys(&vendor_keys_path())
                .unwrap()
                .keys["cli04-original"]
                .label,
            "allowed-write"
        );
        assert_eq!(
            saved_vendor_keys(&harness).keys["cli04-original"].label,
            "allowed-write"
        );

        let (status, raw_body) = harness
            .send_raw_request("DELETE", KEY_PATH, None, &headers)
            .await
            .unwrap();
        assert!(status.starts_with("HTTP/1.1 200"));
        let body: Value = serde_json::from_str(&raw_body).unwrap();
        assert_eq!(body["data"]["deleted"], true);
        assert!(!crate::bindings::load_vendor_keys(&vendor_keys_path())
            .unwrap()
            .keys
            .contains_key("cli04-original"));
        assert!(!saved_vendor_keys(&harness)
            .keys
            .contains_key("cli04-original"));

        let (status, _) = harness
            .send_raw_request(
                "POST",
                "/api/vendor-keys",
                Some(br#"{"id":"cli04-added","vendor_id":"cli04-mock","label":"added"}"#),
                &headers,
            )
            .await
            .unwrap();
        assert!(status.starts_with("HTTP/1.1 200"));
        assert_eq!(
            saved_vendor_keys(&harness).keys["cli04-added"].label,
            "added"
        );
        assert_eq!(
            crate::bindings::load_vendor_keys(&vendor_keys_path())
                .unwrap()
                .keys["cli04-added"]
                .label,
            "added"
        );
    }
}

#[tokio::test]
async fn cli04_safe_methods_remain_exempt_without_adding_cors_support() {
    for external in [false, true] {
        let harness = seeded_harness().await;
        if external {
            let enable = harness
                .post_json(
                    "/api/access-control",
                    json!({"external_access": true, "allowed_ips": ["192.168.100.5"]}),
                )
                .await
                .unwrap();
            assert_eq!(enable.body["data"]["external_access"], true);
        }
        let before = persisted_snapshot(&harness);
        for method in ["GET", "HEAD", "OPTIONS"] {
            for origin in ["https://cli04-attacker.invalid", "null"] {
                let (status, raw_body) = harness
                    .send_raw_request(method, "/health", None, &[("Origin", origin)])
                    .await
                    .unwrap();
                assert_eq!(
                    status,
                    if method == "GET" {
                        "HTTP/1.1 200 OK"
                    } else {
                        "HTTP/1.1 404 Not Found"
                    },
                    "{method}, external={external}"
                );
                assert!(!raw_body.contains("CSRF_REJECTED"));
                assert_eq!(persisted_snapshot(&harness), before);
            }
        }
    }
}

async fn enable_external(harness: &WebUiTestHarness) -> String {
    let enable = harness
        .post_json(
            "/api/access-control",
            json!({"external_access": true, "allowed_ips": ["192.168.100.5"]}),
        )
        .await
        .unwrap();
    assert_eq!(enable.body["data"]["external_access"], true);
    enable.body["data"]["access_token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn cli04_external_writes_require_origin_even_with_correct_token() {
    let harness = seeded_harness().await;
    let token = enable_external(&harness).await;
    let before = persisted_snapshot(&harness);
    for method in ["POST", "PUT", "DELETE", "PATCH", "CUSTOM"] {
        for origin in [None, Some("http://192.168.100.6:8977")] {
            let mut headers = vec![("X-WPTSALL-WebUI-Token", token.as_str())];
            if let Some(origin) = origin {
                headers.push(("Origin", origin));
            }
            let (status, raw_body) = harness
                .send_raw_request(
                    method,
                    KEY_PATH,
                    Some(br#"{"label":"attacker-changed"}"#),
                    &headers,
                )
                .await
                .unwrap();
            let body: Value = serde_json::from_str(&raw_body).unwrap();
            assert_eq!(status, "HTTP/1.1 403 Forbidden", "{method} {origin:?}");
            assert_eq!(body["error"]["code"], "CSRF_REJECTED");
            assert_eq!(persisted_snapshot(&harness), before);
        }
    }
    let (status, _) = harness
        .send_raw_request(
            "PUT",
            KEY_PATH,
            Some(br#"{"label":"whitelisted"}"#),
            &[
                ("Origin", "http://192.168.100.5:8977"),
                ("X-WPTSALL-WebUI-Token", token.as_str()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        saved_vendor_keys(&harness).keys["cli04-original"].label,
        "whitelisted"
    );
}

#[tokio::test]
async fn cli04_allowed_origin_cannot_bypass_host_or_token_guards() {
    let harness = seeded_harness().await;
    let token = enable_external(&harness).await;
    let before = persisted_snapshot(&harness);
    for method in ["PUT", "DELETE"] {
        let (status, raw_body) = harness
            .send_raw_request_without_host(
                method,
                KEY_PATH,
                Some(br#"{"label":"attacker-changed"}"#),
                &[
                    ("Host", "cli04-attacker.invalid"),
                    ("Origin", "http://127.0.0.1:8977"),
                    ("X-WPTSALL-WebUI-Token", token.as_str()),
                ],
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&raw_body).unwrap();
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        assert_eq!(body["error"]["code"], "WEBUI_HOST_REJECTED");
        assert_eq!(persisted_snapshot(&harness), before);
        for headers in [
            vec![("Origin", "http://127.0.0.1:8977")],
            vec![
                ("Origin", "http://127.0.0.1:8977"),
                ("X-WPTSALL-WebUI-Token", "cli04-wrong-token"),
            ],
        ] {
            let (status, raw_body) = harness
                .send_raw_request(method, KEY_PATH, None, &headers)
                .await
                .unwrap();
            let body: Value = serde_json::from_str(&raw_body).unwrap();
            assert_eq!(status, "HTTP/1.1 401 Unauthorized");
            assert_eq!(body["error"]["code"], "WEBUI_TOKEN_REQUIRED");
            assert_eq!(persisted_snapshot(&harness), before);
        }
    }
}
