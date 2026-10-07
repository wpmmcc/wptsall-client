use anyhow::Context;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::logging::unix_ts;
use crate::types::*;

use super::auth::{base64_url_encode_bytes, simple_urlencode};
use super::errors::{
    err_public, write_conflict_response, write_error_response, write_not_found_response,
};
use super::http::{parse_query_string, write_http_response};
use super::{proxy_profiles_path, vendor_keys_path, vendor_oauth_path};

pub(super) mod config_store;
pub(super) mod oauth_flow;
use config_store::{load_config, save_config};

fn valid_integration_fields(request: &Value, kind: &str) -> bool {
    let Some(fields) = request.as_object() else {
        return false;
    };
    for name in [
        "id",
        "vendor_id",
        "label",
        "grant_type",
        "auth_url",
        "token_url",
        "client_id",
        "client_secret",
        "scopes",
        "token_field",
        "name",
        "host",
        "protocol",
        "username",
        "password",
    ] {
        if fields.get(name).is_some_and(|value| !value.is_string()) {
            return false;
        }
    }
    if fields
        .get("enabled")
        .is_some_and(|value| !value.is_boolean())
    {
        return false;
    }
    for (name, minimum, maximum) in [
        ("max_concurrent", 1, u32::MAX as u64),
        ("weight", 0, u32::MAX as u64),
        ("max_input_chars", 0, u32::MAX as u64),
    ] {
        if fields.get(name).is_some_and(|value| {
            !value
                .as_u64()
                .is_some_and(|value| value >= minimum && value <= maximum)
        }) {
            return false;
        }
    }
    for name in ["requests_per_second", "max_file_size_mb"] {
        if fields.get(name).is_some_and(|value| {
            !value
                .as_f64()
                .is_some_and(|value| value.is_finite() && value >= 0.0)
        }) {
            return false;
        }
    }
    let maps: &[&str] = match kind {
        "keys" => &["auth_values"],
        "oauth" => &["extra_params", "auth_extra_params"],
        _ => &[],
    };
    if maps.iter().any(|name| {
        fields.get(*name).is_some_and(|value| {
            !value
                .as_object()
                .is_some_and(|values| values.values().all(Value::is_string))
        })
    }) {
        return false;
    }
    !fields
        .get("port")
        .is_some_and(|value| value.as_u64().is_none())
}

// ---------------------------------------------------------------------------
// Vendor Keys CRUD
// ---------------------------------------------------------------------------

pub(super) async fn handle_vendor_keys_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let path = vendor_keys_path();
    let doc: VendorKeysDoc = load_config(state, &path, "vendor_keys_doc").await?;
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
    if !valid_integration_fields(&req, "keys") {
        return write_error_response(
            socket,
            "INVALID_INTEGRATION_CONFIG",
            "invalid integration field shape or limit",
        )
        .await;
    }
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
    let mut doc: VendorKeysDoc = load_config(state, &path, "vendor_keys_doc").await?;
    let before = doc.clone();
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
    save_config(state, &path, "vendor_keys_doc", &before, &doc).await?;
    crate::logging::log_event_global(
        "info",
        "integration.vendor_key_created",
        json!({ "id": id }),
    );
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
    if !valid_integration_fields(&req, "keys") {
        return write_error_response(
            socket,
            "INVALID_INTEGRATION_CONFIG",
            "invalid integration field shape or limit",
        )
        .await;
    }
    let path = vendor_keys_path();
    let mut doc: VendorKeysDoc = load_config(state, &path, "vendor_keys_doc").await?;
    let before = doc.clone();
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
    save_config(state, &path, "vendor_keys_doc", &before, &doc).await?;
    crate::logging::log_event_global(
        "info",
        "integration.vendor_key_updated",
        json!({ "id": id }),
    );
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
    let mut doc: VendorKeysDoc = load_config(state, &path, "vendor_keys_doc").await?;
    let before = doc.clone();
    let removed = doc.keys.remove(id).is_some();
    save_config(state, &path, "vendor_keys_doc", &before, &doc).await?;
    crate::logging::log_event_global(
        "warn",
        "integration.vendor_key_deleted",
        json!({ "id": id }),
    );
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
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let path = vendor_oauth_path();
    let doc: VendorOAuthDoc = load_config(state, &path, "vendor_oauth_doc").await?;
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
    if !valid_integration_fields(&req, "oauth") {
        return write_error_response(
            socket,
            "INVALID_INTEGRATION_CONFIG",
            "invalid integration field shape or limit",
        )
        .await;
    }
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
    let mut doc: VendorOAuthDoc = load_config(state, &path, "vendor_oauth_doc").await?;
    let before = doc.clone();
    if doc.configs.contains_key(&id) {
        return write_conflict_response(socket, "DUPLICATE_ID", "OAuth id already exists").await;
    }
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
    save_config(state, &path, "vendor_oauth_doc", &before, &doc).await?;
    crate::logging::log_event_global("info", "integration.oauth_created", json!({ "id": id }));
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
    let mut doc: VendorOAuthDoc = load_config(state, &path, "vendor_oauth_doc").await?;
    let before = doc.clone();
    let removed = doc.configs.remove(id).is_some();
    save_config(state, &path, "vendor_oauth_doc", &before, &doc).await?;
    crate::logging::log_event_global("warn", "integration.oauth_deleted", json!({ "id": id }));
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
    if !valid_integration_fields(&req, "oauth") {
        return write_error_response(
            socket,
            "INVALID_INTEGRATION_CONFIG",
            "invalid integration field shape or limit",
        )
        .await;
    }
    let path = vendor_oauth_path();
    let mut doc: VendorOAuthDoc = load_config(state, &path, "vendor_oauth_doc").await?;
    let before = doc.clone();
    let Some(cfg) = doc.configs.get_mut(id) else {
        return write_not_found_response(socket, "NOT_FOUND", "oauth config not found").await;
    };
    let identity = crate::component_rt::oauth::OAuthTokenManager::credential_fingerprint(cfg)?;
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
    if crate::component_rt::oauth::OAuthTokenManager::credential_fingerprint(cfg)? != identity {
        cfg.cached_token = None;
        cfg.cached_token_expires_at = 0;
        cfg.refresh_token = None;
    }
    save_config(state, &path, "vendor_oauth_doc", &before, &doc).await?;
    crate::logging::log_event_global("info", "integration.oauth_updated", json!({ "id": id }));
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
    let doc = config_store::load_oauth_for_tokens(state, &path).await?;
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
    if crate::component_rt::oauth::OAuthTokenManager::validate_token_request(config).is_err() {
        return write_error_response(
            socket,
            "INVALID_TOKEN_REQUEST",
            "token endpoint or parameters are invalid; no authorization was created",
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

    // Reserved identity/PKCE values cannot be shadowed by provider extras.
    let reserved = [
        "client_id",
        "redirect_uri",
        "response_type",
        "state",
        "code_challenge",
        "code_challenge_method",
        "scope",
    ];
    if config
        .auth_extra_params
        .keys()
        .any(|key| reserved.contains(&key.to_ascii_lowercase().as_str()))
    {
        return write_error_response(
            socket,
            "OAUTH_IDENTITY_OVERRIDE",
            "authorization parameters must not override OAuth identity or PKCE",
        )
        .await;
    }
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

    let mut authorize_url = match url::Url::parse(&config.auth_url) {
        Ok(url) => url,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_AUTH_URL",
                "authorization endpoint is invalid; no authorization was created",
            )
            .await
        }
    };
    if !(matches!(authorize_url.scheme(), "http" | "https")
        && authorize_url.host_str().is_some()
        && authorize_url.username().is_empty()
        && authorize_url.password().is_none()
        && authorize_url.fragment().is_none()
        && crate::component_rt::runner::assert_provider_url_allowed(authorize_url.as_str()).is_ok()
        && !authorize_url
            .query_pairs()
            .any(|(key, _)| reserved.contains(&key.to_ascii_lowercase().as_str())))
    {
        return write_error_response(
            socket,
            "INVALID_AUTH_URL",
            "authorization endpoint or parameters are invalid; no authorization was created",
        )
        .await;
    }
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", simple_urlencode(key), simple_urlencode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let query = authorize_url
        .query()
        .filter(|query| !query.is_empty())
        .map(|existing| format!("{existing}&{query}"))
        .unwrap_or(query);
    authorize_url.set_query(Some(&query));
    let (authorize_url,redirect_uri) = match oauth_flow::start(
        state,config_id,config,&oauth_state,&code_verifier,&redirect_uri,authorize_url.as_str(),
    ).await {
        Ok(saved) => saved,
        Err(_) => return write_conflict_response(socket,"OAUTH_AUTHORIZATION_RETAINED",
            "original OAuth authorization is unresolved or changed; no new authorization was created").await,
    };

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

    let (status, message, success) = match oauth_flow::complete(state, &oauth_state, &code).await {
        Ok(oauth_flow::Outcome::Applied(expires_in)) => (
            "200 OK",
            format!(
                "已成功获取 Token，有效期 {} 秒。此窗口将自动关闭。",
                expires_in
            ),
            true,
        ),
        Ok(oauth_flow::Outcome::Rejected(status)) => (
            "400 Bad Request",
            format!(
                "Token 交换失败 (HTTP {})，原始请求已保留，未重复交换 Token",
                status
            ),
            false,
        ),
        Ok(oauth_flow::Outcome::MissingToken) => (
            "400 Bad Request",
            "响应中缺少 access_token，原始请求已保留，未重复交换 Token".into(),
            false,
        ),
        Ok(oauth_flow::Outcome::NetworkUnknown) => (
            "500 Internal Server Error",
            "Token 交换网络请求失败，原始请求已保留，未重复交换 Token".into(),
            false,
        ),
        Err(_) => (
            "409 Conflict",
            "OAuth 授权状态未确认、已更改或无法保存，原始证据已保留，未重复交换 Token".into(),
            false,
        ),
    };
    let html = vendor_oauth_result_page(
        if success {
            "授权成功"
        } else {
            "授权失败"
        },
        &message,
        success,
    );
    write_http_response(socket, status, "text/html; charset=utf-8", html.as_bytes()).await
}

/// Result page shown in the popup window after vendor OAuth callback.
fn vendor_oauth_result_page(title: &str, message: &str, success: bool) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    };
    let title = escape(title);
    let message = escape(message);
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
    doc: crate::types::VendorOAuthDoc,
) -> anyhow::Result<()> {
    if config.token_url.is_empty() {
        return write_error_response(socket, "INVALID_TOKEN_URL", "token_url is required").await;
    }

    let db = state.lock().await.db.clone();
    let client = crate::component_rt::oauth::OAuthHttpClient::direct()?;
    let manager = crate::component_rt::oauth::OAuthTokenManager::new(doc.configs, client, path)
        .with_recovery_db(db);
    match manager.get_token(id).await {
        Ok(_) => {
            let saved = manager.snapshot().await;
            let expires_at = saved
                .get(id)
                .map(|config| config.cached_token_expires_at)
                .unwrap_or(0);
            let expires_in = expires_at.saturating_sub(unix_ts() as i64).max(0);
            crate::logging::log_event_global(
                "info",
                "integration.oauth_authorized",
                json!({ "id": id, "grant_type": "client_credentials" }),
            );
            let payload = json!({
                "success": true,
                "data": { "token_preview": "Bearer [redacted]", "expires_in": expires_in }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => write_error_response(socket, "TOKEN_REQUEST_ERROR", &err_public(&err)).await,
    }
}

// ---------------------------------------------------------------------------
// Proxy Profiles CRUD
// ---------------------------------------------------------------------------

pub(super) async fn handle_proxy_list(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let path = proxy_profiles_path();
    let doc: ProxyProfilesDoc = load_config(state, &path, "proxy_profiles_doc").await?;
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
    if !valid_integration_fields(&req, "proxy") {
        return write_error_response(
            socket,
            "INVALID_INTEGRATION_CONFIG",
            "invalid integration field shape or limit",
        )
        .await;
    }
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
    let mut doc: ProxyProfilesDoc = load_config(state, &path, "proxy_profiles_doc").await?;
    let before = doc.clone();
    if doc.profiles.contains_key(&id) {
        return write_conflict_response(socket, "DUPLICATE_ID", "proxy profile id already exists")
            .await;
    }
    doc.profiles.insert(id.clone(), profile);
    save_config(state, &path, "proxy_profiles_doc", &before, &doc).await?;
    crate::logging::log_event_global("info", "integration.proxy_created", json!({ "id": id }));
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
    if !valid_integration_fields(&req, "proxy") {
        return write_error_response(
            socket,
            "INVALID_INTEGRATION_CONFIG",
            "invalid integration field shape or limit",
        )
        .await;
    }
    let path = proxy_profiles_path();
    let mut doc: ProxyProfilesDoc = load_config(state, &path, "proxy_profiles_doc").await?;
    let before = doc.clone();
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
    save_config(state, &path, "proxy_profiles_doc", &before, &doc).await?;
    crate::logging::log_event_global("info", "integration.proxy_updated", json!({ "id": id }));
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
    let mut doc: ProxyProfilesDoc = load_config(state, &path, "proxy_profiles_doc").await?;
    let before = doc.clone();
    let removed = doc.profiles.remove(id).is_some();
    save_config(state, &path, "proxy_profiles_doc", &before, &doc).await?;
    crate::logging::log_event_global("warn", "integration.proxy_deleted", json!({ "id": id }));
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
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = proxy_profiles_path();
    let doc: ProxyProfilesDoc = load_config(state, &path, "proxy_profiles_doc").await?;
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
            crate::logging::log_event_global(
                "info",
                "integration.proxy_tested",
                json!({ "id": id, "ok": true }),
            );
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
            crate::logging::log_event_global(
                "warn",
                "integration.proxy_tested",
                json!({ "id": id, "ok": false }),
            );
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
