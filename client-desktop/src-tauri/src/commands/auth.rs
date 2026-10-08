use serde::Serialize;
use serde_json::json;

use super::webui_proxy;

#[derive(Serialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub email: Option<String>,
    pub device_id: Option<String>,
    pub domains: Vec<DomainInfo>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
pub struct DomainInfo {
    pub domain: String,
}

fn map_domain(value: &serde_json::Value) -> DomainInfo {
    DomainInfo {
        domain: value
            .get("api_base_url")
            .or_else(|| value.get("domain"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

pub(crate) fn map_auth_status(data: &serde_json::Value) -> AuthStatus {
    let mut extra = data.as_object().cloned().unwrap_or_default();
    extra.remove("logged_in");
    extra.remove("email");
    extra.remove("device_id");
    extra.remove("domains");

    let domains = data
        .get("domains")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().map(map_domain).collect())
        .unwrap_or_default();

    AuthStatus {
        logged_in: data
            .get("logged_in")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        email: data
            .get("email")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        device_id: data
            .get("device_id")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        domains,
        extra,
    }
}

#[tauri::command]
pub async fn login(email: String, password: String) -> Result<AuthStatus, String> {
    let _ = (email, password);
    Err("Password login is not supported by Protocol v2 desktop; use OAuth PKCE via /api/oauth/start".into())
}

#[tauri::command]
pub async fn start_oauth() -> Result<serde_json::Value, String> {
    webui_proxy::post("/api/oauth/start", json!({})).await
}

#[tauri::command]
pub async fn logout() -> Result<(), String> {
    webui_proxy::post("/api/logout", json!({}))
        .await
        .map(|_| ())
}

#[tauri::command]
pub async fn get_status() -> Result<AuthStatus, String> {
    let data = webui_proxy::get("/api/status").await?;
    Ok(map_auth_status(&data))
}

#[tauri::command]
pub async fn refresh_domains() -> Result<Vec<DomainInfo>, String> {
    let data = webui_proxy::post("/api/domains/refresh", json!({})).await?;
    Ok(data
        .get("domains")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().map(map_domain).collect())
        .unwrap_or_default())
}
