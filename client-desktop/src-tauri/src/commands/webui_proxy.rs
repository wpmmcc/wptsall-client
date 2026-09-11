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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn agent_base_uses_explicit_override() {
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
        let dir = temp_test_dir("embedded-webui");
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
