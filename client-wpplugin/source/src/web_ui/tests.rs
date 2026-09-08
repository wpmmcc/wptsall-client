use super::*;
use reqwest::Client;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

type EnvVarGuard = crate::db::TestEnvVarGuard;

fn build_test_web_ui_state(
    server_base: &str,
    session_token: Option<&str>,
) -> Arc<Mutex<WebUiState>> {
    let http_client = Client::builder().no_proxy().build().unwrap();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    Arc::new(Mutex::new(WebUiState {
        server_base: server_base.to_string(),
        device_id: "device-test".to_string(),
        session_token: session_token.map(|s| s.to_string()),
        oauth_code_verifier: None,
        oauth_state: None,
        domains: Vec::new(),
        components: Vec::new(),
        component_bindings_path: "/tmp/test-component-bindings.json".to_string(),
        component_bindings: ComponentBindingsDoc::default(),
        domain_token_bindings_path: "/tmp/test-domain-token-bindings.json".to_string(),
        domain_token_bindings: DomainTokenBindingsDoc::default(),
        task_type_component_bindings_path: "/tmp/test-task-type-bindings.json".to_string(),
        task_type_component_bindings: TaskTypeComponentBindingsDoc::default(),
        rule_component_bindings_path: "/tmp/test-rule-bindings.json".to_string(),
        rule_component_bindings: RuleComponentBindingsDoc::default(),
        worker_loop_running: false,
        worker_status: "idle".to_string(),
        worker_loop_poll_seconds: 20,
        worker_last_summary: json!({}),
        worker_recent_runs: Vec::new(),
        local_components_backfilled: 0,
        local_components_backfill_error: String::new(),
        last_error: String::new(),
        last_event: "test.ready".to_string(),
        updated_at: 0,
        log_enabled: false,
        log_min_level: "info".to_string(),
        vendor_oauth_pending: std::collections::HashMap::new(),
        db: Arc::new(Mutex::new(db)),
        http_client,
        update_in_progress: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }))
}

// -----------------------------------------------------------------------
// P0-STOR: WebUI/Desktop must pin SQLite storage mode
// -----------------------------------------------------------------------

#[test]
fn ensure_web_ui_storage_mode_overwrites_false_flag() {
    let _guard = EnvVarGuard::set("WPTSALL_WEB_UI", "false");
    ensure_web_ui_storage_mode();
    assert_eq!(
        std::env::var("WPTSALL_WEB_UI").ok().as_deref(),
        Some("1"),
        "run_web_ui/Desktop agent must force WPTSALL_WEB_UI=1 so UI+worker share SQLite"
    );
}

#[test]
fn ensure_web_ui_storage_mode_pins_when_unset_via_empty_sentinel() {
    // EnvVarGuard cannot "unset"; use a non-truthy value then force-pin.
    let _guard = EnvVarGuard::set("WPTSALL_WEB_UI", "");
    ensure_web_ui_storage_mode();
    assert_eq!(std::env::var("WPTSALL_WEB_UI").ok().as_deref(), Some("1"));
}

// -----------------------------------------------------------------------
// AccessControl unit tests (C1+M1 audit fix: IP-based access control)
// -----------------------------------------------------------------------

#[tokio::test]
async fn access_control_loopback_always_allowed() {
    let ac = AccessControl::new(false, &[]);
    assert!(ac.is_allowed("127.0.0.1".parse().unwrap()).await);
    assert!(ac.is_allowed("::1".parse().unwrap()).await);
}

#[tokio::test]
async fn access_control_non_loopback_denied_by_default() {
    let ac = AccessControl::new(false, &[]);
    assert!(!ac.is_allowed("192.168.1.100".parse().unwrap()).await);
    assert!(!ac.is_allowed("10.0.0.1".parse().unwrap()).await);
}

#[tokio::test]
async fn access_control_whitelisted_ip_allowed() {
    let extra: IpAddr = "192.168.1.100".parse().unwrap();
    let ac = AccessControl::new(true, &[extra]);
    assert!(ac.is_allowed("192.168.1.100".parse().unwrap()).await);
    // Non-whitelisted still denied
    assert!(!ac.is_allowed("192.168.1.200".parse().unwrap()).await);
    // Loopback still allowed
    assert!(ac.is_allowed("127.0.0.1".parse().unwrap()).await);
}

#[tokio::test]
async fn access_control_external_mode_flag() {
    let ac_off = AccessControl::new(false, &[]);
    assert!(!ac_off.is_external().await);

    let ac_on = AccessControl::new(true, &[]);
    assert!(ac_on.is_external().await);
}

#[tokio::test]
async fn access_control_update_replaces_whitelist() {
    let ip1: IpAddr = "10.0.0.1".parse().unwrap();
    let ip2: IpAddr = "10.0.0.2".parse().unwrap();
    let ac = AccessControl::new(false, &[ip1]);

    assert!(ac.is_allowed(ip1).await);
    assert!(!ac.is_allowed(ip2).await);

    // Update: replace ip1 with ip2
    ac.update(true, &[ip2]).await;
    assert!(!ac.is_allowed(ip1).await);
    assert!(ac.is_allowed(ip2).await);
    assert!(ac.is_external().await);
    // Loopback still works after update
    assert!(ac.is_allowed("127.0.0.1".parse().unwrap()).await);
    assert!(ac.is_allowed("::1".parse().unwrap()).await);
}

#[tokio::test]
async fn access_control_get_settings_excludes_loopback() {
    let ip: IpAddr = "10.0.0.5".parse().unwrap();
    let ac = AccessControl::new(true, &[ip]);
    let (external, ips) = ac.get_settings().await;
    assert!(external);
    assert_eq!(ips.len(), 1);
    assert_eq!(ips[0], "10.0.0.5");
}

#[tokio::test]
async fn access_control_ipv6_whitelisted() {
    let ip: IpAddr = "2001:db8::1".parse().unwrap();
    let ac = AccessControl::new(true, &[ip]);
    assert!(ac.is_allowed("2001:db8::1".parse().unwrap()).await);
    assert!(!ac.is_allowed("2001:db8::2".parse().unwrap()).await);
}

#[test]
fn missing_route_secret_must_not_fallback_to_secretless_client_path() {
    let api_base_url = "https://example.com/wp-json/wptsall/v2/client";
    let domain_base = normalize_domain_base(api_base_url);
    let route_secret: Option<String> = None;

    let wp_base = route_secret
        .as_deref()
        .and_then(|s| build_wp_base_url(&domain_base, s));

    assert!(
        wp_base.is_none(),
        "missing route_secret must not construct a secretless /wp-json/wptsall/v2/client fallback"
    );
}

#[test]
fn local_sites_from_token_bindings_are_active_and_unmetered() {
    let mut doc = DomainTokenBindingsDoc::default();
    doc.domains.insert(
        "HTTP://Example.test/wp-json/wptsall/v2/secret/client".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "wp-token".to_string(),
            route_secret: "secret".to_string(),
        },
    );
    doc.domains.insert(
        "https://empty-token.test".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "  ".to_string(),
            route_secret: "secret".to_string(),
        },
    );

    let sites = local_sites_from_domain_token_bindings(&doc);

    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].api_base_url, "http://example.test");
    assert_eq!(sites[0].site_status, "active");
    assert_eq!(sites[0].route_secret.as_deref(), Some("secret"));
    assert!(sites[0].max_relations.is_none());
    assert!(sites[0].plan_expires_at.is_none());
}

#[tokio::test]
async fn fetch_domains_for_session_preserves_structured_server_auth_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;

        let body =
            r#"{"success":false,"error":{"code":"SESSION_EXPIRED","message":"Session expired"}}"#;
        let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let client = Client::new();
    let err = fetch_domains_for_session(&client, &format!("http://{}", addr), "sess_test")
        .await
        .expect_err("server auth error should be surfaced to caller");
    let err_text = format!("{:#}", err);

    assert!(
        !err_text.contains("unsupported response shape"),
        "structured server auth errors should not be collapsed into unsupported response shape: {}",
        err_text
    );
    assert!(
        err_text.contains("SESSION_EXPIRED") || err_text.contains("Session expired"),
        "session expiry semantics should be preserved in the propagated error: {}",
        err_text
    );
}

#[tokio::test]
async fn worker_once_must_not_succeed_when_404_recovery_refresh_hits_auth_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("");

            let (status, body) = match step {
                0 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/domains"),
                        "expected initial domains fetch, got: {}",
                        first_line
                    );
                    (
                            "200 OK",
                            format!(
                                "{{\"success\":true,\"data\":{{\"items\":[{{\"api_base_url\":\"http://{}/wp-json/wptsall/v2/client\",\"site_status\":\"active\",\"route_secret\":\"secret-old\",\"plan_tier\":\"pro\",\"max_relations\":1}}]}}}}",
                                addr
                            ),
                        )
                }
                1 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/signing-public-key"),
                        "expected signing key fetch, got: {}",
                        first_line
                    );
                    (
                            "200 OK",
                            "{\"success\":true,\"data\":{\"public_key_pem\":\"-----BEGIN PUBLIC KEY-----\\nplaceholder\\n-----END PUBLIC KEY-----\",\"key_id\":\"kid-test\"}}".to_string(),
                        )
                }
                2 => {
                    assert!(
                        first_line.starts_with(
                            "GET /wp-json/wptsall/v2/secret-old/client/site-relations"
                        ),
                        "expected initial WP site-relations fetch, got: {}",
                        first_line
                    );
                    ("404 Not Found", "{}".to_string())
                }
                3 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/domains"),
                        "expected domains refresh after 404, got: {}",
                        first_line
                    );
                    (
                            "401 Unauthorized",
                            "{\"success\":false,\"error\":{\"code\":\"SESSION_EXPIRED\",\"message\":\"Session expired\"}}".to_string(),
                        )
                }
                _ => unreachable!(),
            };
            let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", addr), Some("sess-test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            format!("http://{}", addr),
            DomainTokenBindingEntry {
                wp_client_token: "wp-client-token-test".to_string(),
                route_secret: String::new(),
            },
        );
    }

    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let tmp_db = PathBuf::from(format!(
        "/tmp/wptsall-webui-run-once-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", tmp_db.display().to_string());
    let _runtime_guard = EnvVarGuard::set("WPTSALL_COMPONENT_RUNTIME", "0".to_string());
    let _fallback_guard = EnvVarGuard::set("WPTSALL_COMPONENT_FALLBACK", "1".to_string());

    let err = web_ui_run_worker_once(&state, "/tmp/test-web-ui-run-once.log")
            .await
            .expect_err(
                "run-once should not report success when 404 recovery refresh hits session auth failure",
            );
    let err_text = format!("{:#}", err);

    assert!(
        err_text.contains("SESSION_EXPIRED"),
        "404 recovery auth failure should be surfaced to caller: {}",
        err_text
    );
}

#[tokio::test]
async fn worker_once_must_not_succeed_when_signing_key_prefetch_hits_auth_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("");

            let (status, body) = match step {
                0 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/domains"),
                        "expected initial domains fetch, got: {}",
                        first_line
                    );
                    (
                            "200 OK",
                            format!(
                                "{{\"success\":true,\"data\":{{\"items\":[{{\"api_base_url\":\"http://{}/wp-json/wptsall/v2/client\",\"site_status\":\"active\",\"route_secret\":\"secret-ok\",\"plan_tier\":\"pro\",\"max_relations\":1}}]}}}}",
                                addr
                            ),
                        )
                }
                1 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/signing-public-key"),
                        "expected signing key fetch, got: {}",
                        first_line
                    );
                    (
                            "401 Unauthorized",
                            "{\"success\":false,\"error\":{\"code\":\"SESSION_EXPIRED\",\"message\":\"Session expired\"}}".to_string(),
                        )
                }
                2 => {
                    assert!(
                        first_line
                            .starts_with("GET /wp-json/wptsall/v2/secret-ok/client/site-relations"),
                        "expected WP site-relations fetch after signing-key failure, got: {}",
                        first_line
                    );
                    ("200 OK", "{\"relations\":[]}".to_string())
                }
                _ => unreachable!(),
            };
            let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", addr), Some("sess-test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            format!("http://{}", addr),
            DomainTokenBindingEntry {
                wp_client_token: "wp-client-token-test".to_string(),
                route_secret: String::new(),
            },
        );
    }

    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let tmp_db = PathBuf::from(format!(
        "/tmp/wptsall-webui-run-once-signing-key-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", tmp_db.display().to_string());
    let _runtime_guard = EnvVarGuard::set("WPTSALL_COMPONENT_RUNTIME", "0".to_string());
    let _fallback_guard = EnvVarGuard::set("WPTSALL_COMPONENT_FALLBACK", "1".to_string());

    let err = web_ui_run_worker_once(&state, "/tmp/test-web-ui-run-once-signing-key.log")
            .await
            .expect_err(
                "run-once should not report success when signing-key prefetch hits session auth failure",
            );
    let err_text = format!("{:#}", err);

    assert!(
        err_text.contains("SESSION_EXPIRED"),
        "signing-key auth failure should be surfaced to caller instead of being swallowed: {}",
        err_text
    );
}

#[tokio::test]
async fn worker_once_uses_cached_signing_key_when_refresh_is_rate_limited() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("");

            let (status, body) = match step {
                0 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/domains"),
                        "expected initial domains fetch, got: {}",
                        first_line
                    );
                    (
                        "200 OK",
                        format!(
                            "{{\"success\":true,\"data\":{{\"items\":[{{\"api_base_url\":\"http://{}/wp-json/wptsall/v2/client\",\"site_status\":\"active\",\"route_secret\":\"secret-ok\",\"plan_tier\":\"pro\",\"max_relations\":1}}]}}}}",
                            addr
                        ),
                    )
                }
                1 => {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/signing-public-key"),
                        "expected signing key refresh, got: {}",
                        first_line
                    );
                    (
                        "429 Too Many Requests",
                        "{\"success\":false,\"error\":{\"code\":\"RATE_LIMITED\",\"message\":\"Too many requests. Please try again later.\"}}".to_string(),
                    )
                }
                2 => {
                    assert!(
                        first_line
                            .starts_with("GET /wp-json/wptsall/v2/secret-ok/client/site-relations"),
                        "expected WP site-relations fetch after cached-key fallback, got: {}",
                        first_line
                    );
                    ("200 OK", "{\"relations\":[]}".to_string())
                }
                _ => unreachable!(),
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let state = build_test_web_ui_state(&format!("http://{}", addr), Some("sess-test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            format!("http://{}", addr),
            DomainTokenBindingEntry {
                wp_client_token: "wp-client-token-test".to_string(),
                route_secret: String::new(),
            },
        );
    }

    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let tmp_db = PathBuf::from(format!(
        "/tmp/wptsall-webui-run-once-signing-key-rate-limit-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", tmp_db.display().to_string());
    let _runtime_guard = EnvVarGuard::set("WPTSALL_COMPONENT_RUNTIME", "0".to_string());
    let _fallback_guard = EnvVarGuard::set("WPTSALL_COMPONENT_FALLBACK", "1".to_string());

    {
        let db_arc = {
            let guard = state.lock().await;
            std::sync::Arc::clone(&guard.db)
        };
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::system::set_signing_key_material(
            &conn,
            "-----BEGIN PUBLIC KEY-----\nplaceholder\n-----END PUBLIC KEY-----",
            Some("kid-cached"),
        )
        .unwrap();
    }

    let summary = web_ui_run_worker_once(
        &state,
        "/tmp/test-web-ui-run-once-signing-key-rate-limit.log",
    )
    .await
    .expect("cached signing key should allow run-once to continue when refresh is rate limited");

    assert_eq!(summary["total_items"], serde_json::json!(0));
}
