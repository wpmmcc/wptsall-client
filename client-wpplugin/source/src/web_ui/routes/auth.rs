use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::auth::{request_json, request_json_encrypted, UpstreamApiError};
use crate::config::env_or;
use crate::logging::{log_event, snippet, unix_ts};
use crate::types::{ApiResponse, OAuthTokenData, WebUiRuntimeControl, WebUiState};
use crate::web_ui::{fetch_components_for_session, fetch_domains_for_session};

use super::errors::{
    maybe_write_upstream_api_error, write_error_response, write_error_response_with_status,
    write_session_required,
};
use super::http::{parse_query_string, write_http_response};
use super::{redacted_domain_status_items, sync_local_components_from_server, update_state_error};

pub(super) async fn handle_domains_refresh(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };
    match fetch_domains_for_session(&client, &server_base, &session_token).await {
        Ok(domains) => {
            let mut guard = state.lock().await;
            guard.domains = domains;
            guard.last_error.clear();
            guard.last_event = "domains.refreshed".to_string();
            guard.updated_at = unix_ts();
            let payload = json!({ "success": true, "data": { "domains": redacted_domain_status_items(&guard.domains) } });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            update_state_error(state, &err, "domains.refresh_failed").await;
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            write_error_response(socket, "DOMAINS_REFRESH_FAILED", &format!("{:#}", err)).await
        }
    }
}

pub(super) async fn handle_logout(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    runtime_control: &WebUiRuntimeControl,
) -> anyhow::Result<()> {
    runtime_control
        .worker_running
        .store(false, Ordering::SeqCst);
    let handle_opt = {
        let mut handle_guard = runtime_control.worker_handle.lock().await;
        handle_guard.take()
    };
    if let Some(handle) = handle_opt {
        handle.abort();
        let _ = handle.await;
    }
    let logout_result = {
        let guard = state.lock().await;
        if let Some(ref token) = guard.session_token {
            let server_base = guard.server_base.clone();
            let tok = token.clone();
            let client = guard.http_client.clone();
            drop(guard);
            Some(
                request_json::<ApiResponse<Value>>(
                    client
                        .post(format!("{}/api/v1/client/logout", server_base))
                        .header("X-Client-Session", &tok),
                    "client logout",
                )
                .await,
            )
        } else {
            None
        }
    };

    if let Some(Err(err)) = logout_result {
        {
            let mut guard = state.lock().await;
            guard.worker_loop_running = false;
            guard.worker_status = "idle".to_string();
            guard.updated_at = unix_ts();
        }

        // SESSION_REVOKED is the special "session already gone" case: the
        // server has no record of this token (rotated, expired, or
        // explicitly invalidated). The target local state is "logged out",
        // so fall through to the success path and clear local state.
        // Other errors (LOCK_ERROR, network, 5xx) keep the fail-closed
        // behavior so we don't silently drop a session the server might
        // still honour.
        let is_session_revoked = err
            .downcast_ref::<UpstreamApiError>()
            .map(|u| u.code == "SESSION_REVOKED")
            .unwrap_or(false);
        if !is_session_revoked {
            update_state_error(state, &err, "auth.logout_failed").await;
            if let Some(response) = maybe_write_upstream_api_error(socket, &err).await {
                return response;
            }
            return write_error_response(socket, "LOGOUT_FAILED", &format!("{:#}", err)).await;
        }
        // Mark this as an informational logout, not a failure.
        {
            let mut guard = state.lock().await;
            guard.last_event = "auth.logout_revoked".to_string();
            guard.last_error.clear();
            guard.updated_at = unix_ts();
        }
    }

    let db_path = env_or("WPTSALL_DB_PATH", "./runtime/wptsall.db");
    crate::oauth::clear_cached_token_db(&db_path);
    let _ = std::fs::remove_file(crate::config::session_token_file());

    {
        let mut guard = state.lock().await;
        guard.session_token = None;
        guard.domains.clear();
        guard.components.clear();
        guard.worker_loop_running = false;
        guard.worker_status = "idle".to_string();
        guard.last_error.clear();
        guard.last_event = "auth.logout".to_string();
        guard.updated_at = unix_ts();
    }

    let payload = json!({ "success": true, "data": { "logged_in": false } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_oauth_start(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    use rand::Rng;
    use sha2::{Digest, Sha256};

    let (server_base, _device_id) = {
        let guard = state.lock().await;
        (guard.server_base.clone(), guard.device_id.clone())
    };
    if server_base.trim().is_empty() {
        return write_error_response_with_status(
            socket,
            "503 Service Unavailable",
            "SERVER_CONTROL_PLANE_NOT_CONFIGURED",
            "OAuth login requires an explicitly configured WPTSALL_SERVER_BASE or WPTSALL_SERVER_URL.",
        )
        .await;
    }

    let code_verifier: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let digest = hasher.finalize();
    let code_challenge = base64_url_encode_bytes(&digest);

    let oauth_state = uuid::Uuid::new_v4().to_string();

    {
        let mut guard = state.lock().await;
        guard.oauth_code_verifier = Some(code_verifier);
        guard.oauth_state = Some(oauth_state.clone());
    }

    let redirect_uri = format!("{}/oauth/callback", crate::web_ui::web_ui_loopback_origin());
    let authorize_url = format!(
        "{}/oauth/authorize?redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&client_id={}",
        server_base,
        simple_urlencode(&redirect_uri),
        simple_urlencode(&code_challenge),
        simple_urlencode(&oauth_state),
        simple_urlencode("wptsall-client")
    );

    let payload = json!({
        "success": true,
        "data": { "authorize_url": authorize_url }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_oauth_callback(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    query: &str,
    log_file: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    let code = params.get("code").cloned().unwrap_or_default();
    let callback_state = params.get("state").cloned().unwrap_or_default();

    if code.is_empty() {
        let html = format!(
            "<!doctype html><html><body><h3>{}</h3><p>{}</p></body></html>",
            crate::i18n::t("oauth_callback.login_failed", "en"),
            crate::i18n::t("oauth_callback.no_auth_code", "en"),
        );
        return write_http_response(
            socket,
            "400 Bad Request",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    }

    let (server_base, code_verifier, expected_state, device_id, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.oauth_code_verifier.clone(),
            guard.oauth_state.clone(),
            guard.device_id.clone(),
            guard.http_client.clone(),
        )
    };

    if server_base.trim().is_empty() {
        let html = format!(
            "<!doctype html><html><body><h3>{}</h3><p>{}</p></body></html>",
            crate::i18n::t("oauth_callback.login_failed", "en"),
            "OAuth control-plane endpoint is not configured.",
        );
        return write_http_response(
            socket,
            "503 Service Unavailable",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    }

    let expected_state = expected_state.unwrap_or_default();
    if callback_state != expected_state {
        let html = format!(
            "<!doctype html><html><body><h3>{}</h3><p>{}</p></body></html>",
            crate::i18n::t("oauth_callback.login_failed", "en"),
            crate::i18n::t("oauth_callback.state_mismatch", "en"),
        );
        return write_http_response(
            socket,
            "400 Bad Request",
            "text/html; charset=utf-8",
            html.as_bytes(),
        )
        .await;
    }

    let code_verifier = match code_verifier {
        Some(v) => v,
        None => {
            let html = format!(
                "<!doctype html><html><body><h3>{}</h3><p>{}</p></body></html>",
                crate::i18n::t("oauth_callback.login_failed", "en"),
                crate::i18n::t("oauth_callback.no_session", "en"),
            );
            return write_http_response(
                socket,
                "400 Bad Request",
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
            .await;
        }
    };

    let redirect_uri = format!("{}/oauth/callback", crate::web_ui::web_ui_loopback_origin());
    let token_url = format!("{}/api/v1/oauth/token", server_base);

    let token_body = json!({
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirect_uri,
        "code_verifier": code_verifier,
        "client_id": "wptsall-client",
        "device_id": device_id
    });
    let token_resp: OAuthTokenData = match request_json_encrypted(
        client.post(&token_url).json(&token_body),
        "oauth token exchange",
        &code_verifier,
    )
    .await
    {
        Ok(resp) => resp,
        Err(err) => {
            let _ = log_event(
                log_file,
                "warning",
                "oauth.token_exchange_error",
                json!({ "error": format!("{:#}", err) }),
            );
            let (status, detail) = if let Some(upstream) = err.downcast_ref::<UpstreamApiError>() {
                (
                    upstream.status.clone(),
                    format!("{}: {}", upstream.code, upstream.message),
                )
            } else {
                (
                    "500 Internal Server Error".to_string(),
                    format!(
                        "{}{}",
                        crate::i18n::t("oauth_callback.token_exchange_error_prefix", "en"),
                        err
                    ),
                )
            };
            let html = format!(
                "<!doctype html><html><body><h3>{}</h3><p>{}</p><script>setTimeout(function(){{window.close()}},3000)</script></body></html>",
                crate::i18n::t("oauth_callback.login_failed", "en"),
                detail,
            );
            return write_http_response(
                socket,
                &status,
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
            .await;
        }
    };

    let session_token = token_resp.session_token.clone();
    let db_path = env_or("WPTSALL_DB_PATH", "./runtime/wptsall.db");
    crate::oauth::write_cached_token_db(&db_path, &session_token);
    {
        let mut guard = state.lock().await;
        guard.session_token = Some(session_token.clone());
        guard.last_error.clear();
        guard.last_event = "oauth.login_ok".to_string();
        guard.updated_at = unix_ts();
    }

    let mut bootstrap_domains_warning = String::new();
    let domains = match fetch_domains_for_session(&client, &server_base, &session_token).await {
        Ok(domains) => domains,
        Err(err) => {
            let _ = log_event(
                log_file,
                "warning",
                "oauth.bootstrap_domains_failed",
                json!({ "error": format!("{:#}", err) }),
            );
            // Graceful degradation: keep the session alive even if domains fetch fails.
            // The worker will retry on next heartbeat cycle.
            bootstrap_domains_warning = snippet(&format!("{:#}", err));
            Vec::new()
        }
    };
    let mut bootstrap_components_warning = String::new();
    let components = match fetch_components_for_session(&client, &server_base, &session_token).await
    {
        Ok(components) => components,
        Err(err) => {
            let _ = log_event(
                log_file,
                "warning",
                "oauth.bootstrap_components_failed",
                json!({ "error": format!("{:#}", err) }),
            );
            bootstrap_components_warning = snippet(&format!("{:#}", err));
            Vec::new()
        }
    };
    let local_components_backfilled = match sync_local_components_from_server(&components) {
        Ok(count) => count,
        Err(err) => {
            let _ = log_event(
                log_file,
                "warning",
                "oauth.local_components_backfill_failed",
                json!({
                    "error": snippet(&format!("{:#}", err))
                }),
            );
            0
        }
    };
    if local_components_backfilled > 0 {
        let _ = log_event(
            log_file,
            "info",
            "oauth.local_components_backfilled",
            json!({
                "updated": local_components_backfilled
            }),
        );
    }

    {
        let mut guard = state.lock().await;
        guard.session_token = Some(session_token);
        guard.domains = domains;
        guard.components = components;
        guard.oauth_code_verifier = None;
        guard.oauth_state = None;
        let has_warning =
            !bootstrap_components_warning.is_empty() || !bootstrap_domains_warning.is_empty();
        guard.last_error = if !bootstrap_components_warning.is_empty() {
            bootstrap_components_warning.clone()
        } else {
            bootstrap_domains_warning.clone()
        };
        guard.last_event = if has_warning {
            "oauth.login_partial".to_string()
        } else {
            "oauth.login_ok".to_string()
        };
        guard.updated_at = unix_ts();
    }

    let has_warning =
        !bootstrap_components_warning.is_empty() || !bootstrap_domains_warning.is_empty();
    let _ = log_event(
        log_file,
        if has_warning { "warning" } else { "info" },
        if has_warning {
            "oauth.login_partial"
        } else {
            "oauth.login_ok"
        },
        json!({
            "components_warning": bootstrap_components_warning,
            "domains_warning": bootstrap_domains_warning
        }),
    );

    let html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title>\n<style>body{{font-family:sans-serif;display:flex;justify-content:center;align-items:center;min-height:100vh;background:#f6f7f9;}}\n.card{{background:#fff;border:1px solid #ddd;border-radius:12px;padding:32px;text-align:center;}}\n.ok{{color:#2fbf71;font-size:48px;margin-bottom:12px;}}</style></head>\n<body><div class=\"card\"><div class=\"ok\">&#10003;</div><h2>{title}</h2><p>{close_msg}</p>\n<script>setTimeout(function(){{window.close()}},2000)</script></div></body></html>",
        title = crate::i18n::t("oauth_callback.login_successful", "en"),
        close_msg = crate::i18n::t("oauth_callback.close_window", "en"),
    );
    write_http_response(
        socket,
        "200 OK",
        "text/html; charset=utf-8",
        html.as_bytes(),
    )
    .await
}

pub(super) fn base64_url_encode_bytes(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(data)
}

pub(super) fn simple_urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    out
}
