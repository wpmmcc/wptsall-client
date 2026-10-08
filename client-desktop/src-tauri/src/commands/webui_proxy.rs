use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

const DEFAULT_WEB_UI_PORT: u16 = 8977;

fn web_ui_port() -> u16 {
    std::env::var("WPTSALL_WEB_UI_PORT")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_WEB_UI_PORT)
}

pub(super) fn agent_base_url() -> String {
    std::env::var("WPTSALL_DESKTOP_AGENT_BASE")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", web_ui_port()))
}

fn api_error_message(status: reqwest::StatusCode, payload: &Value) -> String {
    let error = payload.get("error").and_then(Value::as_object);
    let code = error
        .and_then(|object| object.get("code"))
        .and_then(Value::as_str)
        .or_else(|| payload.get("code").and_then(Value::as_str))
        .unwrap_or("DESKTOP_AGENT_ERROR");
    let message = error
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .unwrap_or("Desktop agent request failed");
    format!("{code}: {message} (HTTP {status})")
}

pub(super) async fn request_json(
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    let url = format!("{}{}", agent_base_url(), path);
    let request_timeout_secs = if path == "/api/worker/run-once" {
        std::env::var("WPTSALL_DESKTOP_RUN_ONCE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(360)
    } else {
        60
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(request_timeout_secs))
        .build()
        .map_err(|err| format!("desktop agent HTTP client: {err:#}"))?;

    let mut request = client.request(method, &url);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let built = request
        .build()
        .map_err(|err| format!("desktop agent request build at {url}: {err:#}"))?;

    // The embedded agent starts asynchronously while the window renders
    // immediately, so the very first commands can race the listener. Retry
    // briefly on connect-stage failures (agent not listening YET) before
    // reporting "desktop agent unavailable" — this keeps the boot sequence
    // green without hiding genuinely-down agents (non-connect errors and
    // the final attempt still surface immediately).
    let connect_retries = if path == "/api/worker/run-once" { 0 } else { 5 };
    let mut response = None;
    let mut last_err: Option<reqwest::Error> = None;
    for attempt in 0..=connect_retries {
        let exec = built
            .try_clone()
            .ok_or_else(|| format!("desktop agent request body not replayable at {url}"))?;
        match client.execute(exec).await {
            Ok(r) => {
                response = Some(r);
                break;
            }
            Err(err) => {
                let is_connect = err.is_connect();
                last_err = Some(err);
                if !is_connect || attempt == connect_retries {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    let response = match response {
        Some(r) => r,
        None => {
            let err = last_err.expect("send failed without an error");
            return Err(format!("desktop agent unavailable at {url}: {err:#}"));
        }
    };
    let status = response.status();
    let payload = response
        .json::<Value>()
        .await
        .map_err(|err| format!("desktop agent invalid JSON from {url}: {err:#}"))?;

    if !status.is_success() || payload.get("success") == Some(&Value::Bool(false)) {
        return Err(api_error_message(status, &payload));
    }

    Ok(payload.get("data").cloned().unwrap_or(Value::Null))
}

pub(super) async fn get(path: &str) -> Result<Value, String> {
    request_json(Method::GET, path, None).await
}

pub(super) async fn post(path: &str, body: Value) -> Result<Value, String> {
    request_json(Method::POST, path, Some(body)).await
}

pub(super) async fn put(path: &str, body: Value) -> Result<Value, String> {
    request_json(Method::PUT, path, Some(body)).await
}

pub(super) async fn request_json_delete(path: &str) -> Result<Value, String> {
    request_json(Method::DELETE, path, None).await
}

#[tauri::command]
pub async fn proxy_webui_request(
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, String> {
    let m = match method.to_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "DELETE" => Method::DELETE,
        "PATCH" => Method::PATCH,
        _ => return Err(format!("Unsupported method: {method}")),
    };
    request_json(m, &path, body).await
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn agent_base_uses_explicit_override() {
        if !crate::tests::in_owned_env_child(
            "commands::webui_proxy::tests::agent_base_uses_explicit_override",
        ) {
            return;
        }
        std::env::set_var("WPTSALL_DESKTOP_AGENT_BASE", "http://127.0.0.1:19077/");
        assert_eq!(agent_base_url(), "http://127.0.0.1:19077");
        std::env::remove_var("WPTSALL_DESKTOP_AGENT_BASE");
    }

    #[test]
    fn api_error_prefers_structured_code_and_message() {
        let payload = json!({
            "success": false,
            "error": { "code": "SESSION_REQUIRED", "message": "Please login first" }
        });
        let message = api_error_message(reqwest::StatusCode::UNAUTHORIZED, &payload);
        assert!(message.contains("SESSION_REQUIRED"));
        assert!(message.contains("Please login first"));
        assert!(message.contains("HTTP 401"));
    }

    // catalog: WEBUI-API-GET-api-provider-catalog
    // oracle: L2
    #[tokio::test]
    async fn ui01_catalog_proxy_preserves_query_over_http() {
        const PATHS: [&str; 4] = [
            "/api/provider-catalog",
            "/api/provider-catalog?q=openai",
            "/api/provider-catalog?q=%E5%9B%BE%E7%89%87+%2B+%26%3F+%23%3D%25&vendor_id=custom_http_mt&future=one&future=two",
            "/api/provider-catalog?q=fixture-error",
        ];
        if std::env::var("WPTSALL_UI01_PROXY_CHILD").as_deref() == Ok("1") {
            for path in &PATHS[..3] {
                let data = proxy_webui_request("GET".into(), (*path).into(), None)
                    .await
                    .unwrap();
                assert_eq!(data, json!({ "method": "GET", "target": path }));
            }
            let error = proxy_webui_request("GET".into(), PATHS[3].into(), None)
                .await
                .unwrap_err();
            assert!(error.contains("CATALOG_FIXTURE_ERROR: fixture unavailable"));
            assert!(error.contains("HTTP 500"));
            return;
        }

        // A child owns the agent-base env; no parent process globals or user
        // configuration are changed, even when other Desktop tests run.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let recorder = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut observed = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while observed.len() < PATHS.len() && std::time::Instant::now() < deadline {
                let (mut socket, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0; 2048];
                while !raw.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0, "incomplete fixture request");
                    raw.extend_from_slice(&buffer[..count]);
                    assert!(raw.len() < 16 * 1024, "fixture header too large");
                }
                let request = std::str::from_utf8(&raw).unwrap();
                let mut parts = request.lines().next().unwrap().split_whitespace();
                assert_eq!(parts.next(), Some("GET"));
                let target = parts.next().unwrap().to_string();
                assert_eq!(parts.next(), Some("HTTP/1.1"));
                assert!(request.ends_with("\r\n\r\n"), "GET has unexpected body");
                for header in request.lines().skip(1) {
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            assert_eq!(value.trim(), "0", "GET has a request body");
                        }
                        assert!(!name.eq_ignore_ascii_case("transfer-encoding"));
                    }
                }
                let (status, payload) = if target == PATHS[3] {
                    (
                        "500 Internal Server Error",
                        json!({
                            "success": false,
                            "error": {
                                "code": "CATALOG_FIXTURE_ERROR",
                                "message": "fixture unavailable"
                            }
                        }),
                    )
                } else {
                    (
                        "200 OK",
                        json!({
                            "success": true,
                            "data": { "method": "GET", "target": target }
                        }),
                    )
                };
                let body = serde_json::to_string(&payload).unwrap();
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                observed.push(target);
            }
            observed
        });
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "commands::webui_proxy::tests::ui01_catalog_proxy_preserves_query_over_http",
                "--nocapture",
            ])
            .env("WPTSALL_UI01_PROXY_CHILD", "1")
            .env("WPTSALL_DESKTOP_AGENT_BASE", base)
            .output()
            .unwrap();
        let observed = recorder.join().unwrap();
        assert!(
            child.status.success(),
            "proxy child failed: {}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(observed, PATHS);
    }

    fn temp_test_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wptsall-desktop-{name}-{}-{}",
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
    async fn desktop_commands_proxy_to_embedded_webui_runtime() {
        if !crate::tests::in_owned_env_child(
            "commands::webui_proxy::tests::desktop_commands_proxy_to_embedded_webui_runtime",
        ) {
            return;
        }
        let dir = temp_test_dir("embedded-webui");
        let port = free_loopback_port();
        let base = format!("http://127.0.0.1:{port}");
        let db_path = dir.join("wptsall.db");
        let log_path = dir.join("wptsall.log");

        std::env::set_var("WPTSALL_DATA_DIR", &dir);
        std::env::set_var("WPTSALL_WEB_UI", "1");
        std::env::set_var("WPTSALL_WEB_UI_PORT", port.to_string());
        std::env::set_var("WPTSALL_WEB_UI_BIND", format!("127.0.0.1:{port}"));
        std::env::set_var("WPTSALL_DESKTOP_AGENT_BASE", &base);
        std::env::set_var("WPTSALL_DB_PATH", &db_path);
        std::env::set_var("WPTSALL_LOG_FILE", &log_path);
        std::env::set_var("WPTSALL_SERVER_BASE", "http://127.0.0.1:65530");
        std::env::set_var("WPTSALL_DEVICE_ID", "desktop-integration-test-device");

        let token = CancellationToken::new();
        let runtime_token = token.clone();
        let handle = tokio::spawn(async move {
            wptsall_client::web_ui::run_web_ui(runtime_token, std::time::Instant::now()).await
        });

        wait_for_agent(&base).await;

        let status = crate::commands::auth::get_status().await.unwrap();
        assert!(!status.logged_in);
        assert!(status.domains.is_empty());

        let worker = crate::commands::worker::get_worker_status().await.unwrap();
        assert!(!worker.running);
        assert_eq!(worker.active_tasks, 0);

        let components = crate::commands::components::list_components()
            .await
            .unwrap();
        assert!(components.is_empty());

        let tasks = crate::commands::tasks::list_tasks().await.unwrap();
        assert!(tasks.is_empty());

        token.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("desktop agent task should stop")
            .expect("desktop agent join");
        result.expect("desktop agent runtime");

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
}