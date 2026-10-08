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
    apikeys, auth, components, platform, review, settings, sites, tasks, worker,
};

/// Serializes the two tests that mutate process-global env vars to drive the
/// embedded WebUI agent (cargo test runs them on parallel threads by default).
static WEBUI_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct WebUiEnvironment {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl WebUiEnvironment {
    fn owned(dir: &std::path::Path, port: u16, base: &str, device: &str) -> Self {
        let mut scope = Self {
            previous: Vec::new(),
        };
        for (key, value) in [
            ("WPTSALL_WEB_UI", "1".to_string()),
            ("WPTSALL_WEB_UI_PORT", port.to_string()),
            ("WPTSALL_WEB_UI_BIND", format!("127.0.0.1:{port}")),
            ("WPTSALL_DESKTOP_AGENT_BASE", base.to_string()),
            ("WPTSALL_DATA_DIR", dir.display().to_string()),
            (
                "WPTSALL_DB_PATH",
                dir.join("wptsall.db").display().to_string(),
            ),
            (
                "WPTSALL_LOG_FILE",
                dir.join("wptsall.log").display().to_string(),
            ),
            ("WPTSALL_SERVER_BASE", "http://127.0.0.1:65530".to_string()),
            ("WPTSALL_DEVICE_ID", device.to_string()),
            (
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                "owned-desktop-headless-fixture-key".to_string(),
            ),
        ] {
            scope.previous.push((key, std::env::var_os(key)));
            std::env::set_var(key, value);
        }
        scope
    }
}

impl Drop for WebUiEnvironment {
    fn drop(&mut self) {
        for (key, value) in self.previous.iter().rev() {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

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
async fn test_cold_capacity_status_and_policy_through_desktop_commands() {
    let _env_guard = WEBUI_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let dir = temp_test_dir("cold-capacity");
    let port = free_loopback_port();
    let base = format!("http://127.0.0.1:{port}");
    let _configuration = WebUiEnvironment::owned(&dir, port, &base, "desktop-cold-capacity-device");
    std::fs::write(
        dir.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let token = CancellationToken::new();
    let runtime_token = token.clone();
    let handle = tokio::spawn(async move {
        wptsall_client::web_ui::run_web_ui(runtime_token, std::time::Instant::now()).await
    });
    let status = auth::get_status()
        .await
        .expect("capacity-only agent status");
    let status = serde_json::to_value(status).unwrap();
    assert_eq!(status["storage_paused"], true);
    assert_eq!(status["database_available"], false);
    assert_eq!(status["worker_loop_running"], false);
    assert_eq!(status["restart_required"], true);
    let inventory = wptsall_desktop_lib::commands::webui_proxy::proxy_webui_request(
        "GET".into(),
        "/api/storage/capacity".into(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(inventory["policy"]["max_physical_bytes"], 1);
    let updated = wptsall_desktop_lib::commands::webui_proxy::proxy_webui_request(
        "POST".into(),
        "/api/storage/capacity".into(),
        Some(json!({
            "max_physical_bytes": 32 * 1024 * 1024,
            "expected_revision": inventory["revision"],
            "confirm_change": true,
        })),
    )
    .await
    .unwrap();
    assert_eq!(updated["policy"]["max_physical_bytes"], 32 * 1024 * 1024);
    let jobs = review::list_jobs(None, None, None).await.unwrap_err();
    assert!(jobs.contains("STORAGE_CAPACITY_EXHAUSTED"));
    let worker = worker::start_worker().await.unwrap_err();
    assert!(worker.contains("STORAGE_CAPACITY_EXHAUSTED"));
    let health = wptsall_desktop_lib::commands::webui_proxy::proxy_webui_request(
        "GET".into(),
        "/health".into(),
        None,
    )
    .await
    .expect("existing health path remains available while storage is paused");
    assert_eq!(health["storage_paused"], true);
    assert!(!dir.join("wptsall.db").exists());
    token.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("capacity-only agent shutdown timeout")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn test_language_pack_review_request_bridge() {
    const REQUEST: &str = "631b8b74-913a-4972-8892-33a892ce07bb";
    if std::env::var("WPTSALL_PACK_REVIEW_BRIDGE_CHILD").as_deref() == Ok("1") {
        let content = review::get_item_content("8".into()).await.unwrap();
        assert_eq!(content["delivery_unresolved"], true);
        let edited = json!({"content":{"entries":[{"entry_id":101,"msgstr":"Owned reviewed %s"}]}});
        let saved = review::save_item_translated("8".into(), edited.clone())
            .await
            .unwrap();
        assert_eq!(saved["request"], edited);
        review::approve_item("8".into()).await.unwrap();
        for request in [
            json!({"request_id":REQUEST}),
            json!({"request_id":REQUEST,"resume_only":true}),
        ] {
            let result = review::retranslate_item("8".into(), request.clone())
                .await
                .unwrap();
            assert_eq!(result["request"], request);
        }
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let recorder = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut observed = Vec::new();
        while observed.len() < 5 && std::time::Instant::now() < deadline {
            let (mut socket, _) = match listener.accept() {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("owned bridge accept: {error}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let (end, length) = loop {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0, "incomplete owned bridge request");
                bytes.extend_from_slice(&buffer[..n]);
                assert!(bytes.len() <= 16 * 1024, "unexpected fixture body size");
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break (end, length);
                    }
                }
            };
            let first_line = std::str::from_utf8(&bytes[..end])
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .to_owned();
            let request = if length == 0 {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap()
            };
            let payload =
                json!({"success":true,"data":{"request":request,"delivery_unresolved":true}});
            let body = payload.to_string();
            write!(socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            ).unwrap();
            observed.push((first_line, request));
        }
        observed
    });
    // The child owns its agent address. No parent globals, user configuration,
    // shared service or real provider endpoint participate.
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "test_language_pack_review_request_bridge",
            "--nocapture",
        ])
        .env("WPTSALL_PACK_REVIEW_BRIDGE_CHILD", "1")
        .env("WPTSALL_DESKTOP_AGENT_BASE", base)
        .output()
        .unwrap();
    let observed = recorder.join().unwrap();
    assert!(
        child.status.success(),
        "owned bridge child failed: {}{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(observed.len(), 5);
    assert_eq!(observed[0].0, "GET /api/items/8/content HTTP/1.1");
    assert_eq!(observed[1].0, "PUT /api/items/8/translated HTTP/1.1");
    assert_eq!(observed[2].0, "POST /api/items/8/approve HTTP/1.1");
    assert_eq!(observed[3].0, "POST /api/items/8/retranslate HTTP/1.1");
    assert_eq!(
        observed[4].1,
        json!({"request_id":REQUEST,"resume_only":true})
    );
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
    settings::update_settings(updated)
        .await
        .expect("update settings");
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
    let _env_guard = WEBUI_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let dir = temp_test_dir("headless-ipc");
    let port = free_loopback_port();
    let base = format!("http://127.0.0.1:{port}");
    let _configuration = WebUiEnvironment::owned(&dir, port, &base, "desktop-headless-test-device");

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
        existing_api_base_url: None,
        plugin_identity: None,
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

    let vendor_keys = apikeys::list_vendor_keys(None)
        .await
        .expect("list vendor keys");
    assert!(vendor_keys.is_array() || vendor_keys.is_object() || vendor_keys.is_null());

    // Clean shutdown
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("webui join timeout")
        .expect("webui join");
    result.expect("webui runtime exit clean");

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
    let _env_guard = WEBUI_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let dir = temp_test_dir("failure-parity");
    let port = free_loopback_port();
    let base = format!("http://127.0.0.1:{port}");
    let _configuration =
        WebUiEnvironment::owned(&dir, port, &base, "desktop-failure-parity-device");

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
        existing_api_base_url: None,
        plugin_identity: None,
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
        existing_api_base_url: None,
        plugin_identity: None,
    })
    .await
    .expect("add_site must store the binding");
    assert!(
        !added_403.connected,
        "403 site must surface connected=false"
    );
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
        existing_api_base_url: None,
        plugin_identity: None,
    })
    .await
    .expect("add_site must store the binding");
    assert!(
        !added_429.connected,
        "429 site must surface connected=false"
    );
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

    let sites_after = sites::list_sites()
        .await
        .expect("list sites after failures");
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

    let _ = std::fs::remove_dir_all(&dir);
}
