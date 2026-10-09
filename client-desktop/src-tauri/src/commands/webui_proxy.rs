use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

const DEFAULT_WEB_UI_PORT: u16 = crate::commands::agent::DESKTOP_AGENT_PORT;

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
