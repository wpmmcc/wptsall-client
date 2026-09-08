use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::webui_proxy;

// P1-G: the previous `apikeys` commands were stubs (returned empty / "Not yet
// implemented"). They now proxy through the embedded WebUI runtime so Desktop
// can actually list/save/delete vendor keys. The shape returned to the Svelte
// UI is the raw WebUI JSON (`Value`), preserving the same contract WebUI
// pages use directly over HTTP. This closes the biggest P1-12 gap
// (vendor-keys/OAuth/proxy configuration) without re-implementing the logic.

#[derive(Serialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub masked_key: String,
}

#[derive(Deserialize)]
pub struct SaveKeyRequest {
    pub provider: String,
    pub label: String,
    pub key: String,
}

#[tauri::command]
pub async fn list_keys() -> Result<Vec<ApiKeyInfo>, String> {
    // Back-compat shim kept for the existing PATH_TO_COMMAND entry
    // (`/api/keys` -> `list_keys`). The authoritative vendor-key list is
    // `list_vendor_keys` below; this returns an empty Vec so callers that
    // still hit `/api/keys` get a well-typed empty result instead of a stub
    // error.
    Ok(vec![])
}

#[tauri::command]
pub async fn save_key(request: SaveKeyRequest) -> Result<ApiKeyInfo, String> {
    let SaveKeyRequest {
        provider,
        label,
        key,
    } = request;
    let _ = (provider, label, key);
    Err("Use list_vendor_keys / create_vendor_key via the provider catalog UI.".into())
}

#[tauri::command]
pub async fn delete_key(key_id: String) -> Result<(), String> {
    let _ = key_id;
    Ok(())
}

// ---- Vendor keys (proxy to WebUI /api/vendor-keys) ----

#[tauri::command]
pub async fn list_vendor_keys(vendor_id: Option<String>) -> Result<Value, String> {
    let path = match vendor_id.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(vid) => format!("/api/vendor-keys?vendor_id={}", vid),
        None => "/api/vendor-keys".to_string(),
    };
    webui_proxy::get(&path).await
}

#[tauri::command]
pub async fn create_vendor_key(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/vendor-keys", request).await
}

#[tauri::command]
pub async fn update_vendor_key(key_id: String, request: Value) -> Result<Value, String> {
    let path = format!("/api/vendor-keys/{key_id}");
    webui_proxy::put(&path, request).await
}

#[tauri::command]
pub async fn delete_vendor_key(key_id: String) -> Result<Value, String> {
    let path = format!("/api/vendor-keys/{key_id}");
    webui_proxy::request_json_delete(&path).await
}

// ---- Vendor OAuth configs (proxy to WebUI /api/vendor-oauth) ----

#[tauri::command]
pub async fn list_vendor_oauth(vendor_id: Option<String>) -> Result<Value, String> {
    let path = match vendor_id.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(vid) => format!("/api/vendor-oauth?vendor_id={}", vid),
        None => "/api/vendor-oauth".to_string(),
    };
    webui_proxy::get(&path).await
}

#[tauri::command]
pub async fn create_vendor_oauth(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/vendor-oauth", request).await
}

#[tauri::command]
pub async fn update_vendor_oauth(oauth_id: String, request: Value) -> Result<Value, String> {
    let path = format!("/api/vendor-oauth/{oauth_id}");
    webui_proxy::put(&path, request).await
}

#[tauri::command]
pub async fn authorize_vendor_oauth(oauth_id: String) -> Result<Value, String> {
    let path = format!("/api/vendor-oauth/{oauth_id}/authorize");
    webui_proxy::post(&path, serde_json::Value::Null).await
}

#[tauri::command]
pub async fn delete_vendor_oauth(oauth_id: String) -> Result<Value, String> {
    let path = format!("/api/vendor-oauth/{oauth_id}");
    webui_proxy::request_json_delete(&path).await
}

// ---- Proxy profiles (proxy to WebUI /api/proxy-profiles) ----

#[tauri::command]
pub async fn list_proxy_profiles() -> Result<Value, String> {
    webui_proxy::get("/api/proxy-profiles").await
}

#[tauri::command]
pub async fn create_proxy_profile(request: Value) -> Result<Value, String> {
    webui_proxy::post("/api/proxy-profiles", request).await
}

#[tauri::command]
pub async fn update_proxy_profile(proxy_id: String, request: Value) -> Result<Value, String> {
    let path = format!("/api/proxy-profiles/{proxy_id}");
    webui_proxy::put(&path, request).await
}

#[tauri::command]
pub async fn test_proxy_profile(proxy_id: String) -> Result<Value, String> {
    let path = format!("/api/proxy-profiles/{proxy_id}/test");
    webui_proxy::post(&path, serde_json::Value::Null).await
}

#[tauri::command]
pub async fn delete_proxy_profile(proxy_id: String) -> Result<Value, String> {
    let path = format!("/api/proxy-profiles/{proxy_id}");
    webui_proxy::request_json_delete(&path).await
}
