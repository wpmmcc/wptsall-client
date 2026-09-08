use anyhow::Context;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::bindings::{
    load_proxy_profiles, load_vendor_keys, load_vendor_oauth, save_proxy_profiles,
    save_vendor_keys, save_vendor_oauth,
};
use crate::logging::unix_ts;
use crate::types::*;

use super::auth::{base64_url_encode_bytes, simple_urlencode};
use super::errors::{write_conflict_response, write_error_response, write_not_found_response};
use super::http::{parse_query_string, write_http_response};
use super::{proxy_profiles_path, vendor_keys_path, vendor_oauth_path};

// ---------------------------------------------------------------------------
// Vendor Keys CRUD
// ---------------------------------------------------------------------------

pub(super) async fn handle_vendor_keys_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let path = vendor_keys_path();
    let doc = load_vendor_keys(&path).unwrap_or_default();
    let params = parse_query_string(query);
    let vendor_id_filter = params
        .get("vendor_id")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let items: Vec<Value> = doc
        .keys
        .iter()
        .filter(|(_id, key)| {
            if let Some(ref vid) = vendor_id_filter {
                key.vendor_id == *vid
            } else {
                true
            }
        })
        .map(|(id, key)| {
            json!({
                "id": id,
                "vendor_id": key.vendor_id,
                "label": key.label,
                "auth_keys": key.auth_values.keys().collect::<Vec<_>>(),
                "max_concurrent": key.max_concurrent,
                "requests_per_second": key.requests_per_second,
                "weight": key.weight,
                "enabled": key.enabled,
                "max_input_chars": key.max_input_chars,
                "max_file_size_mb": key.max_file_size_mb,
            })
        })
        .collect();
    let payload = json!({ "success": true, "data": { "items": items } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_vendor_key_create(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/vendor-keys json payload")?;
    let id = req
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return write_error_response(socket, "INVALID_ID", "id is required").await;
    }
    let vendor_id = req
        .get("vendor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if vendor_id.is_empty() {
        return write_error_response(socket, "INVALID_VENDOR_ID", "vendor_id is required").await;
    }
    let label = req
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let auth_values: HashMap<String, String> = req
        .get("auth_values")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let max_concurrent = req
        .get("max_concurrent")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    let requests_per_second = req
        .get("requests_per_second")
        .and_then(|v| v.as_f64())
        .unwrap_or(3.0);
    let weight = req.get("weight").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let enabled = req.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let max_input_chars = req
        .get("max_input_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let max_file_size_mb = req
        .get("max_file_size_mb")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let path = vendor_keys_path();
    let mut doc = load_vendor_keys(&path).unwrap_or_default();
    if doc.keys.contains_key(&id) {
        return write_conflict_response(socket, "DUPLICATE_ID", "key id already exists").await;
    }
    doc.keys.insert(
        id.clone(),
        crate::types::VendorKey {
            vendor_id,
            label,
            auth_values,
            max_concurrent,
            requests_per_second,
            weight,
            enabled,
            max_input_chars,
            max_file_size_mb,
        },
    );
    save_vendor_keys(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::vendor::save_vendor_keys_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_vendor_key_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id: &str,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid PUT /api/vendor-keys/:id json payload")?;
    let path = vendor_keys_path();
    let mut doc = load_vendor_keys(&path).unwrap_or_default();
    let Some(key) = doc.keys.get_mut(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "vendor key not found").await;
    };
    if let Some(label) = req.get("label").and_then(|v| v.as_str()) {
        key.label = label.trim().to_string();
    }
    if let Some(auth_values) = req.get("auth_values") {
        if let Ok(m) = serde_json::from_value::<HashMap<String, String>>(auth_values.clone()) {
            key.auth_values = m;
        }
    }
    if let Some(mc) = req.get("max_concurrent").and_then(|v| v.as_u64()) {
        key.max_concurrent = mc as usize;
    }
    if let Some(rps) = req.get("requests_per_second").and_then(|v| v.as_f64()) {
        key.requests_per_second = rps;
    }
    if let Some(w) = req.get("weight").and_then(|v| v.as_u64()) {
        key.weight = w as u32;
    }
    if let Some(e) = req.get("enabled").and_then(|v| v.as_bool()) {
        key.enabled = e;
    }
    if let Some(mic) = req.get("max_input_chars").and_then(|v| v.as_u64()) {
        key.max_input_chars = mic as usize;
    }
    if let Some(mfs) = req.get("max_file_size_mb").and_then(|v| v.as_f64()) {
        key.max_file_size_mb = mfs;
    }
    save_vendor_keys(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::vendor::save_vendor_keys_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_vendor_key_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = vendor_keys_path();
    let mut doc = load_vendor_keys(&path).unwrap_or_default();
    let removed = doc.keys.remove(id).is_some();
    save_vendor_keys(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::vendor::save_vendor_keys_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id, "deleted": removed } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

// ---------------------------------------------------------------------------
// OAuth Configs CRUD
// ---------------------------------------------------------------------------

pub(super) async fn handle_oauth_configs_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let path = vendor_oauth_path();
    let doc = load_vendor_oauth(&path).unwrap_or_default();
    let params = parse_query_string(query);
    let vendor_id_filter = params
        .get("vendor_id")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let items: Vec<Value> = doc
        .configs
        .iter()
        .filter(|(_id, cfg)| {
            if let Some(ref vid) = vendor_id_filter {
                cfg.vendor_id == *vid
            } else {
                true
            }
        })
        .map(|(id, cfg)| {
            json!({
                "id": id,
                "vendor_id": cfg.vendor_id,
                "label": cfg.label,
                "grant_type": cfg.grant_type,
                "auth_url": cfg.auth_url,
                "token_url": cfg.token_url,
                "client_id": cfg.client_id,
                "scopes": cfg.scopes,
                "has_token": cfg.cached_token.is_some(),
                "has_refresh_token": cfg.refresh_token.is_some(),
                "token_expires_at": cfg.cached_token_expires_at,
                "auth_extra_params": cfg.auth_extra_params,
                "max_concurrent": cfg.max_concurrent,
                "weight": cfg.weight,
                "max_input_chars": cfg.max_input_chars,
                "max_file_size_mb": cfg.max_file_size_mb,
                "token_field": cfg.token_field,
            })
        })
        .collect();
    let payload = json!({ "success": true, "data": { "items": items } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_oauth_config_create(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/vendor-oauth json payload")?;
    let id = req
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return write_error_response(socket, "INVALID_ID", "id is required").await;
    }
    let vendor_id = req
        .get("vendor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if vendor_id.is_empty() {
        return write_error_response(socket, "INVALID_VENDOR_ID", "vendor_id is required").await;
    }
    let label = req
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let grant_type = req
        .get("grant_type")
        .and_then(|v| v.as_str())
        .unwrap_or("client_credentials")
        .trim()
        .to_string();
    let auth_url = req
        .get("auth_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let token_url = req
        .get("token_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let client_id = req
        .get("client_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let client_secret = req
        .get("client_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let scopes = req
        .get("scopes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let extra_params: HashMap<String, String> = req
        .get("extra_params")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let auth_extra_params: HashMap<String, String> = req
        .get("auth_extra_params")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let max_concurrent = req
        .get("max_concurrent")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;
    let weight = req.get("weight").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let max_input_chars = req
        .get("max_input_chars")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let max_file_size_mb = req
        .get("max_file_size_mb")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let token_field = req
        .get("token_field")
        .and_then(|v| v.as_str())
        .unwrap_or("access_token")
        .trim()
        .to_string();

    let path = vendor_oauth_path();
    let mut doc = load_vendor_oauth(&path).unwrap_or_default();
    doc.configs.insert(
        id.clone(),
        crate::types::OAuthConfig {
            vendor_id,
            label,
            grant_type,
            auth_url,
            token_url,
            client_id,
            client_secret,
            scopes,
            extra_params,
            auth_extra_params,
            max_concurrent,
            weight,
            max_input_chars,
            max_file_size_mb,
            token_field,
            cached_token: None,
            cached_token_expires_at: 0,
            refresh_token: None,
        },
    );
    save_vendor_oauth(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::vendor::save_vendor_oauth_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_oauth_config_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = vendor_oauth_path();
    let mut doc = load_vendor_oauth(&path).unwrap_or_default();
    let removed = doc.configs.remove(id).is_some();
    save_vendor_oauth(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::vendor::save_vendor_oauth_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id, "deleted": removed } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_oauth_config_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id: &str,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid PUT /api/vendor-oauth/:id json payload")?;
    let path = vendor_oauth_path();
    let mut doc = load_vendor_oauth(&path).unwrap_or_default();
    let Some(cfg) = doc.configs.get_mut(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "oauth config not found").await;
    };
    if let Some(label) = req.get("label").and_then(|v| v.as_str()) {
        cfg.label = label.trim().to_string();
    }
    if let Some(auth_url) = req.get("auth_url").and_then(|v| v.as_str()) {
        cfg.auth_url = auth_url.trim().to_string();
    }
    if let Some(token_url) = req.get("token_url").and_then(|v| v.as_str()) {
        cfg.token_url = token_url.trim().to_string();
    }
    if let Some(client_id) = req.get("client_id").and_then(|v| v.as_str()) {
        cfg.client_id = client_id.trim().to_string();
    }
    if let Some(client_secret) = req.get("client_secret").and_then(|v| v.as_str()) {
        cfg.client_secret = client_secret.trim().to_string();
    }
    if let Some(scopes) = req.get("scopes").and_then(|v| v.as_str()) {
        cfg.scopes = scopes.trim().to_string();
    }
    if let Some(extra_params) = req.get("extra_params") {
        if let Ok(m) = serde_json::from_value::<HashMap<String, String>>(extra_params.clone()) {
            cfg.extra_params = m;
        }
    }
    if let Some(auth_extra_params) = req.get("auth_extra_params") {
        if let Ok(m) = serde_json::from_value::<HashMap<String, String>>(auth_extra_params.clone())
        {
            cfg.auth_extra_params = m;
        }
    }
    if let Some(mc) = req.get("max_concurrent").and_then(|v| v.as_u64()) {
        cfg.max_concurrent = mc as usize;
    }
    if let Some(w) = req.get("weight").and_then(|v| v.as_u64()) {
        cfg.weight = w as u32;
    }
    if let Some(mic) = req.get("max_input_chars").and_then(|v| v.as_u64()) {
        cfg.max_input_chars = mic as usize;
    }
    if let Some(mfs) = req.get("max_file_size_mb").and_then(|v| v.as_f64()) {
        cfg.max_file_size_mb = mfs;
    }
    if let Some(tf) = req.get("token_field").and_then(|v| v.as_str()) {
        cfg.token_field = tf.trim().to_string();
    }
    save_vendor_oauth(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::vendor::save_vendor_oauth_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_oauth_authorize(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = vendor_oauth_path();
    let doc = load_vendor_oauth(&path).unwrap_or_default();
    let Some(config) = doc.configs.get(id).cloned() else {
        return write_not_found_response(socket, "NOT_FOUND", "oauth config not found").await;
    };
    match config.grant_type.as_str() {
        "client_credentials" => {
            handle_oauth_authorize_client_credentials(socket, state, id, config, path, doc).await
        }
        "authorization_code" => handle_vendor_oauth_start(socket, state, id, &config).await,
        other => {
            write_error_response(
                socket,
                "UNSUPPORTED_GRANT_TYPE",
                &format!("unsupported grant_type: {}", other),
            )
            .await
        }
    }
}

/// Start an authorization_code OAuth flow: generate PKCE + state, return the authorization URL.
async fn handle_vendor_oauth_start(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    config_id: &str,
    config: &crate::types::OAuthConfig,
) -> anyhow::Result<()> {
    use rand::Rng;
    use sha2::{Digest, Sha256};

    if config.auth_url.is_empty() {
        return write_error_response(
            socket,
            "MISSING_AUTH_URL",
            "auth_url is required for authorization_code flow",
        )
        .await;
    }
    if config.token_url.is_empty() {
        return write_error_response(
            socket,
            "MISSING_TOKEN_URL",
            "token_url is required for authorization_code flow",
        )
        .await;
    }

    // PKCE: code_verifier (64 random alphanumeric chars) + code_challenge = BASE64URL(SHA256(verifier))
    let code_verifier: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let code_challenge = base64_url_encode_bytes(&hasher.finalize());

    let oauth_state = uuid::Uuid::new_v4().to_string();

    let redirect_uri = format!(
        "{}/oauth/vendor/callback",
        crate::web_ui::web_ui_loopback_origin()
    );

    // Build authorization URL params
    let mut params: Vec<(&str, String)> = vec![
        ("client_id", config.client_id.clone()),
        ("redirect_uri", redirect_uri.clone()),
        ("response_type", "code".to_string()),
        ("state", oauth_state.clone()),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256".to_string()),
    ];
    if !config.scopes.is_empty() {
        params.push(("scope", config.scopes.clone()));
    }
    // Provider-specific authorization URL params (user-configured, not hardcoded)
    for (k, v) in &config.auth_extra_params {
        params.push((k.as_str(), v.clone()));
    }

    let qs: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, simple_urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let authorize_url = format!("{}?{}", config.auth_url, qs);

    // Store pending state (expires in 10 minutes)
    {
        let mut guard = state.lock().await;
        guard.vendor_oauth_pending.insert(
            oauth_state.clone(),
            crate::types::VendorOAuthPendingEntry {
                config_id: config_id.to_string(),
                code_verifier,
                expires_at: unix_ts() as i64 + 600,
            },
        );
    }

    let payload = json!({
        "success": true,
        "data": {
            "authorize_url": authorize_url,
            "callback_url": redirect_uri,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// Handle GET /oauth/vendor/callback — exchange auth code for tokens and store them.
pub(super) async fn handle_vendor_oauth_callback(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    let code = params.get("code").cloned().unwrap_or_default();
    let oauth_state = params.get("state").cloned().unwrap_or_default();

    if code.is_empty() {
        let error = params
            .get("error")
            .cloned()
            .unwrap_or_else(|| "no_code".to_string());
        let html = vendor_oauth_result_page("授权失败", &format!("错误: {}", error), false);
        return write_http_response(
            socket,
            "400 Bad Request",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    }

    // Look up the pending state (remove it atomically)
    let pending = {
        let mut guard = state.lock().await;
        guard.vendor_oauth_pending.remove(&oauth_state)
    };

    let Some(pending) = pending else {
        let html =
            vendor_oauth_result_page("授权失败", "无效或过期的 state 参数，请重新发起授权", false);
        return write_http_response(
            socket,
            "400 Bad Request",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    };

    if unix_ts() as i64 > pending.expires_at {
        let html = vendor_oauth_result_page(
            "授权失败",
            "授权流程已超时（10 分钟），请重新发起授权",
            false,
        );
        return write_http_response(
            socket,
            "400 Bad Request",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    }

    // Load config
    let path = vendor_oauth_path();
    let mut doc = load_vendor_oauth(&path).unwrap_or_default();
    let Some(config) = doc.configs.get(&pending.config_id).cloned() else {
        let html = vendor_oauth_result_page("授权失败", "OAuth 配置不存在或已被删除", false);
        return write_http_response(
            socket,
            "400 Bad Request",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    };

    let redirect_uri = format!(
        "{}/oauth/vendor/callback",
        crate::web_ui::web_ui_loopback_origin()
    );

    // Exchange authorization code for tokens
    let client = Client::builder().timeout(Duration::from_secs(15)).build()?;

    let form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("client_id", &config.client_id),
        ("client_secret", &config.client_secret),
        ("code_verifier", &pending.code_verifier),
    ];

    let resp = client.post(&config.token_url).form(&form).send().await;

    match resp {
        Ok(r) => {
            let status = r.status();
            let body: Value = r.json().await.unwrap_or(json!({}));

            if !status.is_success() {
                let err_msg = body
                    .get("error_description")
                    .or_else(|| body.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                eprintln!(
                    "vendor_oauth token exchange failed: status={}, error={}",
                    status.as_u16(),
                    err_msg
                );
                let html = vendor_oauth_result_page(
                    "授权失败",
                    &format!(
                        "Token 交换失败 (HTTP {})，请检查 OAuth 配置后重试",
                        status.as_u16()
                    ),
                    false,
                );
                return write_http_response(
                    socket,
                    "400 Bad Request",
                    "text/html; charset=utf-8",
                    html.as_bytes(),
                )
                .await;
            }

            let access_token = body
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if access_token.is_empty() {
                let html = vendor_oauth_result_page("授权失败", "响应中缺少 access_token", false);
                return write_http_response(
                    socket,
                    "400 Bad Request",
                    "text/html; charset=utf-8",
                    html.as_bytes(),
                )
                .await;
            }

            let expires_in = body
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .unwrap_or(3600);
            let refresh_token = body
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Persist tokens into the config
            if let Some(cfg) = doc.configs.get_mut(&pending.config_id) {
                cfg.cached_token = Some(access_token);
                cfg.cached_token_expires_at = unix_ts() as i64 + expires_in;
                if refresh_token.is_some() {
                    cfg.refresh_token = refresh_token;
                }
            }
            save_vendor_oauth(&path, &doc)?;
            let db_arc = {
                let g = state.lock().await;
                std::sync::Arc::clone(&g.db)
            };
            {
                let db = db_arc.lock().await;
                let _ = crate::db::vendor::save_vendor_oauth_doc(&db, &doc);
            }

            let html = vendor_oauth_result_page(
                "授权成功",
                &format!(
                    "已成功获取 Token，有效期 {} 秒。此窗口将自动关闭。",
                    expires_in
                ),
                true,
            );
            write_http_response(
                socket,
                "200 OK",
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
            .await
        }
        Err(err) => {
            eprintln!("vendor_oauth token exchange network error: {:#}", err);
            let html = vendor_oauth_result_page(
                "授权失败",
                "Token 交换网络请求失败，请检查网络连接后重试",
                false,
            );
            write_http_response(
                socket,
                "500 Internal Server Error",
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
            .await
        }
    }
}

/// Result page shown in the popup window after vendor OAuth callback.
fn vendor_oauth_result_page(title: &str, message: &str, success: bool) -> String {
    let color = if success { "#16a34a" } else { "#dc2626" };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{title}</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#f9fafb}}
.card{{background:#fff;border-radius:12px;padding:32px 40px;text-align:center;box-shadow:0 4px 24px rgba(0,0,0,.08);max-width:380px}}
h2{{color:{color};margin:0 0 12px}}p{{color:#6b7280;margin:0 0 16px;line-height:1.5}}
.countdown{{font-size:.85rem;color:#9ca3af}}</style>
</head><body>
<div class="card">
  <h2>{title}</h2>
  <p>{message}</p>
  <p class="countdown">此窗口将在 <span id="c">3</span> 秒后关闭...</p>
</div>
<script>
var n=3;var t=setInterval(function(){{n--;document.getElementById('c').textContent=n;if(n<=0){{clearInterval(t);window.close();}}}},1000);
</script>
</body></html>"#
    )
}

async fn handle_oauth_authorize_client_credentials(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
    config: crate::types::OAuthConfig,
    path: String,
    mut doc: crate::types::VendorOAuthDoc,
) -> anyhow::Result<()> {
    if config.token_url.is_empty() {
        return write_error_response(socket, "INVALID_TOKEN_URL", "token_url is required").await;
    }

    let client = Client::builder().timeout(Duration::from_secs(15)).build()?;
    let mut form_params = HashMap::new();
    form_params.insert("grant_type", "client_credentials".to_string());
    form_params.insert("client_id", config.client_id.clone());
    form_params.insert("client_secret", config.client_secret.clone());
    if !config.scopes.is_empty() {
        form_params.insert("scope", config.scopes.clone());
    }
    for (k, v) in &config.extra_params {
        form_params.insert(k.as_str(), v.clone());
    }

    let resp = client
        .post(&config.token_url)
        .form(&form_params)
        .send()
        .await;

    match resp {
        Ok(r) => {
            let status = r.status();
            let body_text = r.text().await.unwrap_or_default();
            if !status.is_success() {
                return write_error_response(
                    socket,
                    "TOKEN_REQUEST_FAILED",
                    &format!("status {}: {}", status.as_u16(), body_text),
                )
                .await;
            }
            let token_json: Value = serde_json::from_str(&body_text).unwrap_or(json!({}));
            let access_token = token_json
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let expires_in = token_json
                .get("expires_in")
                .and_then(|v| v.as_i64())
                .unwrap_or(3600);
            if access_token.is_empty() {
                return write_error_response(
                    socket,
                    "NO_ACCESS_TOKEN",
                    "no access_token in response",
                )
                .await;
            }
            let token_preview = if access_token.len() > 12 {
                format!(
                    "Bearer {}...{}",
                    &access_token[..6],
                    &access_token[access_token.len() - 4..]
                )
            } else {
                format!("Bearer {}", access_token)
            };
            if let Some(cfg) = doc.configs.get_mut(id) {
                cfg.cached_token = Some(access_token);
                cfg.cached_token_expires_at = unix_ts() as i64 + expires_in;
            }
            save_vendor_oauth(&path, &doc)?;
            let db_arc = {
                let g = state.lock().await;
                std::sync::Arc::clone(&g.db)
            };
            {
                let db = db_arc.lock().await;
                let _ = crate::db::vendor::save_vendor_oauth_doc(&db, &doc);
            }
            let payload = json!({
                "success": true,
                "data": { "token_preview": token_preview, "expires_in": expires_in }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            write_error_response(socket, "TOKEN_REQUEST_ERROR", &format!("{:#}", err)).await
        }
    }
}

// ---------------------------------------------------------------------------
// Proxy Profiles CRUD
// ---------------------------------------------------------------------------

pub(super) async fn handle_proxy_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let path = proxy_profiles_path();
    let doc = load_proxy_profiles(&path).unwrap_or_default();
    let items: Vec<Value> = doc
        .profiles
        .iter()
        .map(|(id, p)| {
            json!({
                "id": id,
                "name": p.name,
                "protocol": p.protocol,
                "host": p.host,
                "port": p.port,
                "has_auth": !p.username.is_empty(),
                "enabled": p.enabled,
            })
        })
        .collect();
    let payload = json!({ "success": true, "data": { "items": items } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_proxy_create(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/proxy-profiles json payload")?;
    let id = req
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if id.is_empty() {
        return write_error_response(socket, "INVALID_ID", "id is required").await;
    }
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let protocol = req
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("http")
        .trim()
        .to_string();
    let host = req
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if host.is_empty() {
        return write_error_response(socket, "INVALID_HOST", "host is required").await;
    }
    let port_raw = req.get("port").and_then(|v| v.as_u64()).unwrap_or(8080);
    if port_raw == 0 || port_raw > u16::MAX as u64 {
        return write_error_response(socket, "INVALID_PORT", "port must be between 1 and 65535")
            .await;
    }
    let port = port_raw as u16;
    let username = req
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let password = req
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let enabled = req.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

    let profile = crate::types::ProxyProfile {
        name: name.clone(),
        protocol: protocol.clone(),
        host: host.clone(),
        port,
        username: username.clone(),
        password: password.clone(),
        enabled,
    };
    if let Err(err) = crate::component_rt::proxy::validate_proxy_profile(&profile) {
        let code = if !matches!(protocol.as_str(), "http" | "https" | "socks5" | "socks5h") {
            "INVALID_PROTOCOL"
        } else {
            "INVALID_HOST"
        };
        return write_error_response(socket, code, &format!("{}", err)).await;
    }

    let path = proxy_profiles_path();
    let mut doc = load_proxy_profiles(&path).unwrap_or_default();
    doc.profiles.insert(id.clone(), profile);
    save_proxy_profiles(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::proxy::save_proxy_profiles_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_proxy_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    id: &str,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid PUT /api/proxy-profiles/:id json payload")?;
    let path = proxy_profiles_path();
    let mut doc = load_proxy_profiles(&path).unwrap_or_default();
    let Some(profile) = doc.profiles.get_mut(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "proxy profile not found").await;
    };
    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        profile.name = name.trim().to_string();
    }
    if let Some(protocol) = req.get("protocol").and_then(|v| v.as_str()) {
        profile.protocol = protocol.trim().to_string();
    }
    if let Some(host) = req.get("host").and_then(|v| v.as_str()) {
        profile.host = host.trim().to_string();
    }
    if let Some(port) = req.get("port").and_then(|v| v.as_u64()) {
        if port == 0 || port > u16::MAX as u64 {
            return write_error_response(
                socket,
                "INVALID_PORT",
                "port must be between 1 and 65535",
            )
            .await;
        }
        profile.port = port as u16;
    }
    if let Some(username) = req.get("username").and_then(|v| v.as_str()) {
        profile.username = username.to_string();
    }
    if let Some(password) = req.get("password").and_then(|v| v.as_str()) {
        profile.password = password.to_string();
    }
    if let Some(enabled) = req.get("enabled").and_then(|v| v.as_bool()) {
        profile.enabled = enabled;
    }
    if let Err(err) = crate::component_rt::proxy::validate_proxy_profile(profile) {
        let code = if !matches!(
            profile.protocol.as_str(),
            "http" | "https" | "socks5" | "socks5h"
        ) {
            "INVALID_PROTOCOL"
        } else {
            "INVALID_HOST"
        };
        return write_error_response(socket, code, &format!("{}", err)).await;
    }
    save_proxy_profiles(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::proxy::save_proxy_profiles_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_proxy_delete(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = proxy_profiles_path();
    let mut doc = load_proxy_profiles(&path).unwrap_or_default();
    let removed = doc.profiles.remove(id).is_some();
    save_proxy_profiles(&path, &doc)?;
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::proxy::save_proxy_profiles_doc(&db, &doc);
    }
    let payload = json!({ "success": true, "data": { "id": id, "deleted": removed } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_proxy_test(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = proxy_profiles_path();
    let doc = load_proxy_profiles(&path).unwrap_or_default();
    let Some(profile) = doc.profiles.get(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "proxy profile not found").await;
    };
    if let Err(err) = crate::component_rt::proxy::validate_proxy_profile(profile) {
        return write_error_response(socket, "INVALID_PROXY", &format!("{}", err)).await;
    }
    let username = crate::component_rt::runner::resolve_credential_reference(&profile.username)
        .map_err(|err| anyhow::anyhow!("resolve proxy username failed: {}", err))?;
    let password = crate::component_rt::runner::resolve_credential_reference(&profile.password)
        .map_err(|err| anyhow::anyhow!("resolve proxy password failed: {}", err))?;
    let proxy_url = format!("{}://{}:{}", profile.protocol, profile.host, profile.port);
    let result = async {
        let rp = reqwest::Proxy::all(&proxy_url)?;
        let rp = if !username.is_empty() {
            rp.basic_auth(&username, &password)
        } else {
            rp
        };
        let client = Client::builder()
            .proxy(rp)
            .timeout(Duration::from_secs(10))
            .build()?;
        let resp = client.get("https://httpbin.org/ip").send().await?;
        let body: Value = resp.json().await?;
        let ip = body
            .get("origin")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        Ok::<String, anyhow::Error>(ip)
    }
    .await;

    match result {
        Ok(ip) => {
            let payload = json!({
                "success": true,
                "data": { "reachable": true, "ip": ip, "proxy_url": proxy_url }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            let payload = json!({
                "success": false,
                "error": { "code": "PROXY_TEST_FAILED", "message": format!("{:#}", err) },
                "data": { "reachable": false, "proxy_url": proxy_url }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
    }
}
