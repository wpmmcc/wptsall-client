//! Connection-level security guards from the 5.3falsh2 audit (12 号批 A2):
//! the S4 Host allowlist and the S2 external-mode token gate. Matrix unit
//! tests cover the pure decision cores; harness journeys exercise the real
//! dispatcher (`routes::handle_web_ui_connection`) end-to-end, including
//! the N-4 regression (unauthenticated POST must be rejected while
//! external mode is armed).

// catalog: WEBUI-API-GET-index-html
// catalog: WEBUI-API-GET-favicon-svg
// catalog: WEBUI-API-GET-health
// oracle: L1
// (批E T-1: the token-gate exemption matrix in this file is the L1
// reference for the always-on root routes.)

use super::*;
use crate::web_ui::test_support::WebUiTestHarness;

/// Server base for the harness: none of the endpoints exercised here
/// performs upstream I/O, so an unbound address is sufficient.
const GUARD_TEST_SERVER_BASE: &str = "http://127.0.0.1:8787";

// ---------------------------------------------------------------------------
// Unit matrices (pure decision cores)
// ---------------------------------------------------------------------------

#[test]
fn host_header_host_parses_valid_forms() {
    assert_eq!(host_header_host("127.0.0.1"), Some("127.0.0.1"));
    assert_eq!(host_header_host("127.0.0.1:8977"), Some("127.0.0.1"));
    assert_eq!(host_header_host("localhost"), Some("localhost"));
    assert_eq!(host_header_host("localhost:80"), Some("localhost"));
    assert_eq!(host_header_host("[::1]"), Some("::1"));
    assert_eq!(host_header_host("[::1]:8977"), Some("::1"));
    assert_eq!(host_header_host("  127.0.0.1:8977  "), Some("127.0.0.1"));
}

#[test]
fn host_header_host_rejects_malformed_forms() {
    assert_eq!(host_header_host(""), None);
    assert_eq!(host_header_host("   "), None);
    assert_eq!(host_header_host("evil:notaport"), None);
    assert_eq!(host_header_host("host:"), None);
    assert_eq!(host_header_host(":8977"), None);
    assert_eq!(host_header_host("[::1"), None);
    assert_eq!(host_header_host("[bad]tail"), None);
    assert_eq!(
        host_header_host("::1"),
        None,
        "unbracketed bare IPv6 is invalid in Host"
    );
    assert_eq!(
        host_header_host("fe80::1%eth0"),
        None,
        "zone-scoped IPv6 is not an allowlistable host"
    );
}

#[test]
fn host_header_allowed_matrix_local_mode() {
    // Local mode: loopback names only, exact equality.
    for host in ["127.0.0.1", "localhost", "::1"] {
        assert!(
            host_header_allowed(host, false, None, &[]),
            "local mode must allow loopback host: {}",
            host
        );
    }
    for host in [
        "127.0.0.1.evil.com",
        "localhost.evil.com",
        "evil.com",
        "192.168.1.5",
        "0.0.0.0",
    ] {
        assert!(
            !host_header_allowed(host, false, None, &[]),
            "local mode must reject non-loopback host: {}",
            host
        );
    }
}

#[test]
fn host_header_allowed_matrix_external_mode() {
    let allowlist = vec!["ui.internal.example".to_string()];
    // External mode: loopback + bind host + explicit allowlist entries.
    assert!(host_header_allowed(
        "127.0.0.1",
        true,
        Some("192.168.1.5"),
        &allowlist
    ));
    assert!(host_header_allowed(
        "192.168.1.5",
        true,
        Some("192.168.1.5"),
        &allowlist
    ));
    assert!(host_header_allowed(
        "ui.internal.example",
        true,
        Some("192.168.1.5"),
        &allowlist
    ));
    // Allowlist matching is case-insensitive on both sides.
    assert!(host_header_allowed(
        "UI.Internal.Example",
        true,
        Some("192.168.1.5"),
        &allowlist
    ));
    // Not the bind host, not allowlisted, not loopback → rejected.
    assert!(!host_header_allowed(
        "192.168.1.6",
        true,
        Some("192.168.1.5"),
        &allowlist
    ));
    assert!(!host_header_allowed(
        "evil.com",
        true,
        Some("192.168.1.5"),
        &allowlist
    ));
    // Bind host irrelevant in local mode: still loopback-only.
    assert!(!host_header_allowed(
        "192.168.1.5",
        false,
        Some("192.168.1.5"),
        &allowlist
    ));
}

#[test]
fn webui_token_required_matrix() {
    let remote: Option<std::net::IpAddr> = Some("192.168.100.5".parse().unwrap());
    let loopback: Option<std::net::IpAddr> = Some("127.0.0.1".parse().unwrap());
    // Local mode never gates.
    assert!(!webui_token_required(
        false,
        remote,
        "POST",
        "/api/sync-pairs"
    ));
    assert!(!webui_token_required(
        false,
        None,
        "POST",
        "/api/sync-pairs"
    ));
    // External + remote peer: every /api request gated.
    assert!(webui_token_required(
        true,
        remote,
        "POST",
        "/api/sync-pairs"
    ));
    assert!(webui_token_required(true, remote, "GET", "/api/status"));
    assert!(
        webui_token_required(true, None, "GET", "/api/status"),
        "unknown peer fails closed"
    );
    // Static assets never gate (token entry surface).
    assert!(!webui_token_required(true, remote, "GET", "/"));
    assert!(!webui_token_required(true, remote, "GET", "/index.html"));
    assert!(!webui_token_required(true, remote, "GET", "/favicon.svg"));
    assert!(!webui_token_required(true, remote, "GET", "/health"));
    // External + loopback peer: everything gated EXCEPT the read-only
    // access-control recovery lane.
    assert!(webui_token_required(
        true,
        loopback,
        "POST",
        "/api/sync-pairs"
    ));
    assert!(webui_token_required(true, loopback, "GET", "/api/status"));
    assert!(webui_token_required(
        true,
        loopback,
        "POST",
        "/api/access-control"
    ));
    assert!(!webui_token_required(
        true,
        loopback,
        "GET",
        "/api/access-control"
    ));
}

#[test]
fn constant_time_eq_basics() {
    assert!(constant_time_eq(b"token", b"token"));
    assert!(!constant_time_eq(b"token", b"tokem"));
    assert!(!constant_time_eq(b"token", b"token2"));
    assert!(constant_time_eq(b"", b""));
}

// ---------------------------------------------------------------------------
// Harness journeys (real dispatcher)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn host_guard_rejects_non_loopback_hosts_in_local_mode() {
    let harness = WebUiTestHarness::new(GUARD_TEST_SERVER_BASE, Some("sess_test"))
        .await
        .unwrap();

    // S4 negative: DNS-rebinding payloads and foreign hosts must be
    // rejected before any handler runs. The Host-less raw lane injects
    // the exact Host under test (no duplicate-header override tricks).
    for evil in [
        "127.0.0.1.evil.com",
        "localhost.evil.com",
        "evil.com",
        "192.168.1.5",
    ] {
        let (status, body) = harness
            .send_raw_request_without_host("GET", "/api/log-settings", None, &[("Host", evil)])
            .await
            .unwrap();
        assert!(
            status.starts_with("HTTP/1.1 403"),
            "Host {evil} must be rejected in local mode, got {status}"
        );
        assert!(
            body.contains("WEBUI_HOST_REJECTED"),
            "Host {evil} must report WEBUI_HOST_REJECTED, got {body}"
        );
    }
}

#[tokio::test]
async fn host_guard_rejects_missing_host_header() {
    let harness = WebUiTestHarness::new(GUARD_TEST_SERVER_BASE, Some("sess_test"))
        .await
        .unwrap();

    // S4 fail-closed: HTTP/1.1 clients always send Host; a request without
    // one is malformed or hostile.
    let (status, body) = harness
        .send_raw_request_without_host("GET", "/api/log-settings", None, &[])
        .await
        .unwrap();
    assert!(
        status.starts_with("HTTP/1.1 403"),
        "missing Host must be rejected, got {status}"
    );
    assert!(
        body.contains("WEBUI_HOST_REJECTED"),
        "missing Host must report WEBUI_HOST_REJECTED, got {body}"
    );
}

#[tokio::test]
async fn host_guard_allows_loopback_host_forms() {
    let harness = WebUiTestHarness::new(GUARD_TEST_SERVER_BASE, Some("sess_test"))
        .await
        .unwrap();

    // S4 positive: every legitimate loopback Host form reaches the
    // handler (the same forms browsers produce for 127.0.0.1 deployments).
    for host in [
        "127.0.0.1",
        "127.0.0.1:8977",
        "localhost",
        "localhost:8977",
        "[::1]",
        "[::1]:8977",
    ] {
        let (status, _body) = harness
            .send_raw_request_without_host("GET", "/api/log-settings", None, &[("Host", host)])
            .await
            .unwrap();
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "loopback Host {host} must be allowed, got {status}"
        );
    }
}

#[tokio::test]
async fn access_control_update_requires_non_loopback_ip_to_enable_external() {
    let harness = WebUiTestHarness::new(GUARD_TEST_SERVER_BASE, Some("sess_test"))
        .await
        .unwrap();

    // A6 / N-4 negative: enabling external access with a loopback-only or
    // empty whitelist must be refused (previously persisted as-is — the
    // validation existed only as a comment).
    for ips in [serde_json::json!(["127.0.0.1"]), serde_json::json!([])] {
        let response = harness
            .post_json(
                "/api/access-control",
                serde_json::json!({ "external_access": true, "allowed_ips": ips }),
            )
            .await
            .unwrap();
        assert_eq!(
            response.body["error"]["code"],
            serde_json::json!("EXTERNAL_REQUIRES_IP"),
            "loopback-only enable must be refused: {}",
            response.body
        );
    }

    // State must be unchanged: still local mode.
    let state = harness.get_json("/api/access-control").await.unwrap();
    assert_eq!(
        state.body["data"]["external_access"],
        serde_json::json!(false),
        "rejected enable must not flip the mode"
    );
}

#[tokio::test]
async fn external_mode_token_gate_journey() {
    let harness = WebUiTestHarness::new(GUARD_TEST_SERVER_BASE, Some("sess_test"))
        .await
        .unwrap();

    // Arm external mode through the real API (arrives while still local,
    // so no token is needed yet). The response carries the one-time
    // token display for the operator.
    let enable = harness
        .post_json(
            "/api/access-control",
            serde_json::json!({ "external_access": true, "allowed_ips": ["192.168.100.5"] }),
        )
        .await
        .unwrap();
    assert_eq!(
        enable.body["data"]["external_access"],
        serde_json::json!(true)
    );
    let token = enable.body["data"]["access_token"]
        .as_str()
        .expect("enable response must carry the access token")
        .to_string();
    assert_eq!(
        token.len(),
        32,
        "access token must be 32 hex chars (uuid v4 simple)"
    );
    assert!(
        token.bytes().all(|b| b.is_ascii_hexdigit()),
        "access token must be hex: {token}"
    );

    // S2 negative: /api requests without a token are rejected (harness
    // peer is loopback — only the recovery lane is exempt, not /api/status).
    let denied = harness.get_json("/api/log-settings").await.unwrap();
    assert!(
        denied.status_line.starts_with("HTTP/1.1 401"),
        "external mode must reject unauthenticated /api GET, got {}",
        denied.status_line
    );
    assert_eq!(
        denied.body["error"]["code"],
        serde_json::json!("WEBUI_TOKEN_REQUIRED")
    );

    // Wrong token is equally rejected.
    let (wrong_status, wrong_body) = harness
        .send_raw_request(
            "GET",
            "/api/log-settings",
            None,
            &[("X-WPTSALL-WebUI-Token", "deadbeefdeadbeefdeadbeefdeadbeef")],
        )
        .await
        .unwrap();
    assert!(
        wrong_status.starts_with("HTTP/1.1 401"),
        "wrong token must be rejected, got {wrong_status}"
    );
    assert!(wrong_body.contains("WEBUI_TOKEN_REQUIRED"));

    // S2 positive: the real token unlocks the API.
    let (ok_status, _ok_body) = harness
        .send_raw_request(
            "GET",
            "/api/log-settings",
            None,
            &[("X-WPTSALL-WebUI-Token", token.as_str())],
        )
        .await
        .unwrap();
    assert!(
        ok_status.starts_with("HTTP/1.1 200"),
        "correct token must be accepted, got {ok_status}"
    );

    // Static assets stay open in external mode (token entry surface).
    let (static_status, _static_body) = harness
        .send_raw_request("GET", "/", None, &[])
        .await
        .unwrap();
    assert!(
        static_status.starts_with("HTTP/1.1 200"),
        "static shell must stay reachable without a token, got {static_status}"
    );

    // N-4 regression: an unauthenticated POST must be rejected while
    // external mode is armed (this is the exact posture an attacker on a
    // whitelisted LAN IP would face). CSRF passes (loopback Origin is
    // allowed); the token gate is what refuses the write.
    let (post_status, post_body) = harness
        .send_raw_request(
            "POST",
            "/api/access-control",
            Some(br#"{"external_access":false,"allowed_ips":[]}"#),
            &[("Origin", "http://127.0.0.1:8977")],
        )
        .await
        .unwrap();
    assert!(
        post_status.starts_with("HTTP/1.1 401"),
        "unauthenticated POST in external mode must be rejected, got {post_status}"
    );
    assert!(post_body.contains("WEBUI_TOKEN_REQUIRED"));

    // Loopback recovery lane: GET /api/access-control stays readable on
    // the machine itself and echoes the token so the operator can
    // re-arm remote clients or disable external mode.
    let recovery = harness.get_json("/api/access-control").await.unwrap();
    assert_eq!(
        recovery.body["data"]["access_token"].as_str(),
        Some(token.as_str()),
        "recovery lane must echo the active token"
    );

    // Disarm with the token (loopback peer + POST + token → allowed).
    let disarm = harness
        .send_raw_request(
            "POST",
            "/api/access-control",
            Some(br#"{"external_access":false,"allowed_ips":[]}"#),
            &[
                ("Origin", "http://127.0.0.1:8977"),
                ("X-WPTSALL-WebUI-Token", token.as_str()),
            ],
        )
        .await
        .unwrap();
    assert!(
        disarm.0.starts_with("HTTP/1.1 200"),
        "disarm with token must succeed, got {}",
        disarm.0
    );

    // Local mode restored: the gate is gone.
    let after = harness.get_json("/api/log-settings").await.unwrap();
    assert!(
        after.status_line.starts_with("HTTP/1.1 200"),
        "local mode must not require a token, got {}",
        after.status_line
    );
}
