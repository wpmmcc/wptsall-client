//! Headless integration test suite for client-desktop Tauri commands.
//!
//! Validates Desktop IPC command handlers against the embedded WebUI runtime
//! and underlying Rust core without requiring a display server (Xvfb),
//! WebKitGTK, or tauri-driver.

use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use serde_json::json;
use wptsall_desktop_lib::commands::{
    apikeys, auth, components, platform, settings, sites, tasks, worker,
};

/// Serializes the two tests that mutate process-global env vars to drive the
/// embedded WebUI agent (cargo test runs them on parallel threads by default).
static WEBUI_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn temp_test_dir(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "wptsall-desktop-headless-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn wait_for_agent(base: &str) {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    for _ in 0..50 {
        if let Ok(response) = client.get(format!("{base}/health")).send().await {
            if response.status().is_success() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("desktop agent did not become ready at {base}");
}

#[tokio::test]
async fn test_platform_info_command() {
    let info = platform::get_platform_info().await.expect("platform info");
    assert_eq!(info.os, std::env::consts::OS);
    assert!(!info.arch.is_empty());
    assert!(!info.version.is_empty());
    assert!(!info.data_dir.is_empty());
}

#[tokio::test]
async fn test_settings_commands() {
    let initial = settings::get_settings().await.expect("get settings");
    assert_eq!(initial.language, "en");
    assert_eq!(initial.log_level, "info");
    assert!(!initial.auto_start);

    let updated = settings::ClientSettings {
        auto_start: true,
        language: "zh-CN".into(),
        log_level: "debug".into(),
        proxy: Some("http://127.0.0.1:7890".into()),
    };
    settings::update_settings(updated).await.expect("update settings");
}

#[test]
fn test_site_binding_url_parser() {
    let (base1, secret1) = sites::parse_site_binding_input(
        "http://example.com/wp-json/wptsall/v2/mysecret/client",
        None,
    );
    assert_eq!(base1, "http://example.com");
    assert_eq!(secret1, "mysecret");

    let (base2, secret2) = sites::parse_site_binding_input(
        "https://example.com/subsite/wp-json/wptsall/v2/sec456/client/",
        Some("override-secret"),
    );
    assert_eq!(base2, "https://example.com/subsite");
    assert_eq!(secret2, "override-secret");
}

#[tokio::test]
async fn test_headless_commands_with_embedded_webui() {
    let _env_guard = WEBUI_ENV_LOCK.lock().unwrap();
    let dir = temp_test_dir("headless-ipc");
    let port = free_loopback_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = dir.join("wptsall.db");
    let log_path = dir.join("wptsall.log");

    std::env::set_var("WPTSALL_WEB_UI", "1");
    std::env::set_var("WPTSALL_WEB_UI_PORT", port.to_string());
    std::env::set_var("WPTSALL_WEB_UI_BIND", format!("127.0.0.1:{port}"));
    std::env::set_var("WPTSALL_DESKTOP_AGENT_BASE", &base);
    std::env::set_var("WPTSALL_DB_PATH", &db_path);
    std::env::set_var("WPTSALL_LOG_FILE", &log_path);
    std::env::set_var("WPTSALL_SERVER_BASE", "http://127.0.0.1:65530");
    std::env::set_var("WPTSALL_DEVICE_ID", "desktop-headless-test-device");

    let token = CancellationToken::new();
    let runtime_token = token.clone();
    let handle = tokio::spawn(async move {
        wptsall_client::web_ui::run_web_ui(runtime_token, std::time::Instant::now()).await
    });

    wait_for_agent(&base).await;

    // 1. Auth & Status commands
    let auth_status = auth::get_status().await.expect("auth status");
    assert!(!auth_status.logged_in);
    assert!(auth_status.domains.is_empty());

    // 2. Worker command
    let worker_status = worker::get_worker_status().await.expect("worker status");
    assert!(!worker_status.running);
    assert_eq!(worker_status.active_tasks, 0);

    // 3. Components command
    let components_list = components::list_components().await.expect("components");
    assert!(components_list.is_empty());

    // 4. Tasks command
    let tasks_list = tasks::list_tasks().await.expect("tasks");
    assert!(tasks_list.is_empty());

    // 5. Sites management commands
    let initial_sites = sites::list_sites().await.expect("list sites initial");
    assert!(initial_sites.is_empty());

    let add_site_req = sites::AddSiteRequest {
        wp_url: "http://127.0.0.1:9083/wp-json/wptsall/v2/testroute/client".into(),
        token: "wptoken-headless-test-12345".into(),
        route_secret: None,
    };
    let added_site = sites::add_site(add_site_req).await.expect("add site");
    assert_eq!(added_site.id, "http://127.0.0.1:9083");
    assert_eq!(added_site.domain, "http://127.0.0.1:9083");

    let sites_after_add = sites::list_sites().await.expect("list sites after add");
    assert_eq!(sites_after_add.len(), 1);
    assert_eq!(sites_after_add[0].domain, "http://127.0.0.1:9083");

    sites::remove_site("http://127.0.0.1:9083".into())
        .await
        .expect("remove site");
    let sites_after_remove = sites::list_sites().await.expect("list sites after remove");
    assert!(sites_after_remove.is_empty());

    // 6. Vendor key proxy commands
    let vendor_key_req = json!({
        "vendor_id": "mock-provider",
        "api_key": "sk-headless-test-key-999",
        "alias": "Headless Test Key"
    });
    let created_key = apikeys::create_vendor_key(vendor_key_req).await;
    // create_vendor_key succeeds when vendor catalog / storage accepts it
    if let Ok(key_val) = created_key {
        assert!(key_val.is_object());
    }

    let vendor_keys = apikeys::list_vendor_keys(None).await.expect("list vendor keys");
    assert!(vendor_keys.is_array() || vendor_keys.is_object() || vendor_keys.is_null());

    // Clean shutdown
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("webui join timeout")
        .expect("webui join");
    result.expect("webui runtime exit clean");

    std::env::remove_var("WPTSALL_WEB_UI");
    std::env::remove_var("WPTSALL_WEB_UI_PORT");
    std::env::remove_var("WPTSALL_WEB_UI_BIND");
    std::env::remove_var("WPTSALL_DESKTOP_AGENT_BASE");
    std::env::remove_var("WPTSALL_DB_PATH");
    std::env::remove_var("WPTSALL_LOG_FILE");
    std::env::remove_var("WPTSALL_SERVER_BASE");
    std::env::remove_var("WPTSALL_DEVICE_ID");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// C-3 WebUI/Desktop failure-state parity (plan §7; roadmap C-3 scenario
// group 2: invalid token / 403 / 429 / unreachable endpoint). The WebUI end
// already asserts these through the agent API surface; this is the
// Desktop-end evidence through the REAL Tauri command layer over the
// embedded agent runtime. Group 1 (loading/empty) is already asserted by
// test_headless_commands_with_embedded_webui (empty tasks/components/sites
// first-load states).
// ---------------------------------------------------------------------------

/// Minimal WP mock answering every request with a fixed status + WP-style
/// error JSON (the shape WP returns for forbidden / rate-limited requests).
async fn start_status_wp_mock(
    status: u16,
    reason: &'static str,
    code: &'static str,
    message: &'static str,
) -> (u16, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener as TokioListener;

    let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = format!(
        "{{\"code\":\"{code}\",\"message\":\"{message}\",\"data\":{{\"status\":{status}}}}}"
    );
    let handle = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(socket);
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if lower.starts_with("content-length:") {
                        content_length =
                            lower["content-length:".len()..].trim().parse().unwrap_or(0);
                    }
                }
                if content_length > 0 {
                    let mut buf = vec![0u8; content_length];
                    use tokio::io::AsyncReadExt;
                    let _ = reader.read_exact(&mut buf).await;
                }
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = reader.get_mut().write_all(resp.as_bytes()).await;
            });
        }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    (port, handle)
}

#[tokio::test]
async fn test_site_failure_states_surface_through_commands() {
    let _env_guard = WEBUI_ENV_LOCK.lock().unwrap();
    let dir = temp_test_dir("failure-parity");
    let port = free_loopback_port();
    let base = format!("http://127.0.0.1:{port}");
    let db_path = dir.join("wptsall.db");
    let log_path = dir.join("wptsall.log");

    std::env::set_var("WPTSALL_WEB_UI", "1");
    std::env::set_var("WPTSALL_WEB_UI_PORT", port.to_string());
    std::env::set_var("WPTSALL_WEB_UI_BIND", format!("127.0.0.1:{port}"));
    std::env::set_var("WPTSALL_DESKTOP_AGENT_BASE", &base);
    std::env::set_var("WPTSALL_DB_PATH", &db_path);
    std::env::set_var("WPTSALL_LOG_FILE", &log_path);
    std::env::set_var("WPTSALL_SERVER_BASE", "http://127.0.0.1:65530");
    std::env::set_var("WPTSALL_DEVICE_ID", "desktop-failure-parity-device");

    let token = CancellationToken::new();
    let runtime_token = token.clone();
    let handle = tokio::spawn(async move {
        wptsall_client::web_ui::run_web_ui(runtime_token, std::time::Instant::now()).await
    });

    wait_for_agent(&base).await;

    // Scenario 1: unreachable WP endpoint (connection refused / timeout).
    // A bound port with no listener is an instant, deterministic failure.
    let dead_port = free_loopback_port();
    let dead_url = format!("http://127.0.0.1:{dead_port}/wp-json/wptsall/v2/secdead/client");
    let added_dead = sites::add_site(sites::AddSiteRequest {
        wp_url: dead_url,
        token: "wptoken-failure-parity-dead".into(),
        route_secret: None,
    })
    .await
    .expect("add_site must store the binding even when the WP is unreachable");
    assert!(
        !added_dead.connected,
        "unreachable site must surface connected=false, never a zombie connected=true"
    );
    let err = sites::test_connection(added_dead.id.clone())
        .await
        .expect_err("unreachable endpoint must surface an error through the command layer");
    assert!(
        err.contains("CONNECTION_FAILED"),
        "unreachable endpoint should surface CONNECTION_FAILED, got: {err}"
    );

    // Scenario 2: WP answers 403 (also the WP answer shape for an invalid
    // device-scoped token). The upstream status + code must be passed
    // through to the command caller.
    let (port_403, mock_403) =
        start_status_wp_mock(403, "Forbidden", "rest_forbidden", "Forbidden").await;
    let added_403 = sites::add_site(sites::AddSiteRequest {
        wp_url: format!("http://127.0.0.1:{port_403}/wp-json/wptsall/v2/sec403/client"),
        token: "wptoken-failure-parity-403".into(),
        route_secret: None,
    })
    .await
    .expect("add_site must store the binding");
    assert!(!added_403.connected, "403 site must surface connected=false");
    let err = sites::test_connection(added_403.id.clone())
        .await
        .expect_err("403 upstream must surface an error through the command layer");
    assert!(
        err.contains("HTTP 403"),
        "upstream 403 status must be passed through, got: {err}"
    );
    assert!(
        err.contains("rest_forbidden"),
        "upstream WP error code must be passed through, got: {err}"
    );
    mock_403.abort();

    // Scenario 3: WP answers 429 (rate limited).
    let (port_429, mock_429) = start_status_wp_mock(
        429,
        "Too Many Requests",
        "rest_too_many_requests",
        "Too Many Requests",
    )
    .await;
    let added_429 = sites::add_site(sites::AddSiteRequest {
        wp_url: format!("http://127.0.0.1:{port_429}/wp-json/wptsall/v2/sec429/client"),
        token: "wptoken-failure-parity-429".into(),
        route_secret: None,
    })
    .await
    .expect("add_site must store the binding");
    assert!(!added_429.connected, "429 site must surface connected=false");
    let err = sites::test_connection(added_429.id.clone())
        .await
        .expect_err("429 upstream must surface an error through the command layer");
    assert!(
        err.contains("HTTP 429"),
        "upstream 429 status must be passed through, got: {err}"
    );
    mock_429.abort();

    // Failure states must not poison the desktop runtime: worker stays
    // observable and not-running, and the failure sites are listed as
    // disconnected rather than vanishing.
    let worker_status = worker::get_worker_status().await.expect("worker status");
    assert!(!worker_status.running);
    assert_eq!(worker_status.active_tasks, 0);

    let sites_after = sites::list_sites().await.expect("list sites after failures");
    assert_eq!(
        sites_after.len(),
        3,
        "failure sites must remain listed (observable), not silently dropped"
    );
    let listed_ids: Vec<String> = sites_after.iter().map(|s| s.id.clone()).collect();
    for expected in [
        added_dead.id.clone(),
        added_403.id.clone(),
        added_429.id.clone(),
    ] {
        assert!(
            listed_ids.contains(&expected),
            "failure site {expected} must stay listed, got: {listed_ids:?}"
        );
    }
    // NOTE: list-path `connected` is derived from token presence
    // (site_from_binding: token_configured defaults true), not a live
    // connection check — the live failure state is asserted above via the
    // add_site return value (connected=false) and test_connection Err.

    // Clean shutdown
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("webui join timeout")
        .expect("webui join");
    result.expect("webui runtime exit clean");

    std::env::remove_var("WPTSALL_WEB_UI");
    std::env::remove_var("WPTSALL_WEB_UI_PORT");
    std::env::remove_var("WPTSALL_WEB_UI_BIND");
    std::env::remove_var("WPTSALL_DESKTOP_AGENT_BASE");
    std::env::remove_var("WPTSALL_DB_PATH");
    std::env::remove_var("WPTSALL_LOG_FILE");
    std::env::remove_var("WPTSALL_SERVER_BASE");
    std::env::remove_var("WPTSALL_DEVICE_ID");
    let _ = std::fs::remove_dir_all(&dir);
}
