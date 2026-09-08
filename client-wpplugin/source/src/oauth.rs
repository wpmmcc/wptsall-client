use anyhow::{anyhow, Context};
use rand::Rng;
use reqwest::Client;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::auth::{build_request_id, request_json_encrypted};
use crate::config::REQUEST_ID_HEADER;
use crate::logging::{log_event, session_token_prefix};
use crate::types::OAuthTokenData;

const OAUTH_CALLBACK_TIMEOUT_SECS: u64 = 300;

/// DB key for cached session token (deliberately avoids "token" in the name).
const SESSION_CACHE_DB_KEY: &str = "session_cache";

/// CLI OAuth main entry: try cached token first, otherwise run browser flow.
pub(crate) async fn cli_oauth_login(
    client: &Client,
    server_base: &str,
    device_id: &str,
    log_file: &str,
    db_path: &str,
) -> anyhow::Result<String> {
    // 1. Try cached token from DB
    if let Some(token) = read_cached_token_db(db_path) {
        // Validate via heartbeat
        if validate_session_token(client, server_base, device_id, &token).await {
            let _ = log_event(
                log_file,
                "info",
                "oauth.cached_token_valid",
                json!({ "session_token_prefix": session_token_prefix(&token) }),
            );
            return Ok(token);
        }
        let _ = log_event(log_file, "info", "oauth.cached_token_expired", json!({}));
    }

    // 2. Run OAuth browser flow
    let token = oauth_browser_flow(client, server_base, device_id, log_file).await?;

    // 3. Cache the new token in DB
    write_cached_token_db(db_path, &token);

    let _ = log_event(
        log_file,
        "info",
        "oauth.login_ok",
        json!({ "session_token_prefix": session_token_prefix(&token) }),
    );

    Ok(token)
}

pub(crate) async fn validate_session_token(
    client: &Client,
    server_base: &str,
    device_id: &str,
    token: &str,
) -> bool {
    let hb_url = format!("{}/api/v1/client/heartbeat", server_base);
    let resp = client
        .post(hb_url)
        .header("X-Client-Session", token)
        .header(REQUEST_ID_HEADER, build_request_id("oauth-validate"))
        .json(&json!({
            "device_id": device_id,
            "client_version": env!("CARGO_PKG_VERSION")
        }))
        .send()
        .await;
    match resp {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

async fn oauth_browser_flow(
    client: &Client,
    server_base: &str,
    device_id: &str,
    log_file: &str,
) -> anyhow::Result<String> {
    let locale = crate::i18n::detect_cli_locale();

    // Generate PKCE code_verifier (64 random alphanumeric chars)
    let code_verifier: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    // code_challenge = BASE64URL(SHA256(code_verifier))
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let digest = hasher.finalize();
    let code_challenge = base64_url_encode_bytes(&digest);

    let state = uuid::Uuid::new_v4().to_string();

    // Bind a temporary TCP listener on a random port
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind callback listener")?;
    let port = listener.local_addr()?.port();

    let redirect_uri = format!("http://127.0.0.1:{}/oauth/callback", port);
    let authorize_url = format!(
        "{}/oauth/authorize?redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}&client_id={}",
        server_base,
        simple_urlencode(&redirect_uri),
        simple_urlencode(&code_challenge),
        simple_urlencode(&state),
        simple_urlencode(crate::config::CLIENT_ID),
    );

    // Print info and open browser
    eprintln!("{}", crate::i18n::t("cli.oauth_opening_browser", &locale));
    eprintln!("{}", crate::i18n::t("cli.oauth_manual_url", &locale));
    eprintln!("  {}", authorize_url);

    let _ = open::that(&authorize_url);

    let _ = log_event(
        log_file,
        "info",
        "oauth.browser_opened",
        json!({ "port": port }),
    );

    eprintln!("{}", crate::i18n::t("cli.oauth_waiting_callback", &locale));

    // Wait for callback with timeout
    let callback_result = tokio::time::timeout(
        Duration::from_secs(OAUTH_CALLBACK_TIMEOUT_SECS),
        wait_for_callback(&listener, &state),
    )
    .await;

    let auth_code = match callback_result {
        Ok(Ok((code, stream))) => {
            // Send success HTML response to browser before exchanging token
            let html = format!(
                "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{title}</title>\n\
                 <style>body{{font-family:sans-serif;display:flex;justify-content:center;align-items:center;min-height:100vh;background:#f6f7f9;}}\n\
                 .card{{background:#fff;border:1px solid #ddd;border-radius:12px;padding:32px;text-align:center;}}\n\
                 .ok{{color:#2fbf71;font-size:48px;margin-bottom:12px;}}</style></head>\n\
                 <body><div class=\"card\"><div class=\"ok\">&#10003;</div><h2>{title}</h2><p>{close_msg}</p>\n\
                 <script>setTimeout(function(){{window.close()}},2000)</script></div></body></html>",
                title = crate::i18n::t("oauth_callback.login_successful", "en"),
                close_msg = crate::i18n::t("oauth_callback.close_window", "en"),
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = write_to_stream(stream, response.as_bytes()).await;
            code
        }
        Ok(Err(err)) => {
            return Err(err).context("OAuth callback failed");
        }
        Err(_) => {
            eprintln!("{}", crate::i18n::t("cli.oauth_timeout", &locale));
            let _ = log_event(log_file, "error", "oauth.timeout", json!({}));
            return Err(anyhow!("OAuth authentication timed out"));
        }
    };

    // Exchange auth code for session token
    let token_url = format!("{}/api/v1/oauth/token", server_base);
    let token_body = json!({
        "grant_type": "authorization_code",
        "code": auth_code,
        "redirect_uri": redirect_uri,
        "code_verifier": code_verifier,
        "client_id": crate::config::CLIENT_ID,
        "device_id": device_id,
        "product_id": crate::config::PRODUCT_ID,
    });

    let token_resp: OAuthTokenData = request_json_encrypted(
        client.post(&token_url).json(&token_body),
        "oauth token exchange",
        &code_verifier,
    )
    .await?;

    eprintln!("{}", crate::i18n::t("cli.oauth_success", &locale));

    Ok(token_resp.session_token)
}

async fn wait_for_callback(
    listener: &TcpListener,
    expected_state: &str,
) -> anyhow::Result<(String, tokio::net::TcpStream)> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse GET /oauth/callback?code=X&state=Y HTTP/1.1
        let first_line = request.lines().next().unwrap_or("");
        if !first_line.contains("/oauth/callback") {
            // Not our callback, send 404 and continue listening
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes()).await;
            continue;
        }

        // Extract query string
        let path = first_line.split_whitespace().nth(1).unwrap_or("");
        let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = parse_query_string(query);

        let code = params.get("code").cloned().unwrap_or_default();
        let callback_state = params.get("state").cloned().unwrap_or_default();

        if code.is_empty() {
            let html = "<!doctype html><html><body><h3>Error</h3><p>No authorization code received.</p></body></html>";
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            return Err(anyhow!("No authorization code in callback"));
        }

        if callback_state != expected_state {
            let html =
                "<!doctype html><html><body><h3>Error</h3><p>State mismatch.</p></body></html>";
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            return Err(anyhow!("OAuth state mismatch"));
        }

        return Ok((code, stream));
    }
}

async fn write_to_stream(mut stream: tokio::net::TcpStream, data: &[u8]) -> anyhow::Result<()> {
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Token caching (DB-backed, AES-256-GCM encrypted via system_config)
// ---------------------------------------------------------------------------

pub(crate) fn read_cached_token_db(db_path: &str) -> Option<String> {
    let conn = crate::db::open_db(db_path).ok()?;
    let value = crate::db::system::get_decrypted_config(&conn, SESSION_CACHE_DB_KEY)?;
    let value = value.trim().to_string();
    if value.is_empty() || !value.starts_with("sess_") {
        return None;
    }
    Some(value)
}

pub(crate) fn write_cached_token_db(db_path: &str, token: &str) {
    if let Ok(conn) = crate::db::open_db(db_path) {
        let _ = crate::db::system::set_encrypted_config(&conn, SESSION_CACHE_DB_KEY, token);
    }
}

/// Clear cached session token from DB. Called on logout.
pub(crate) fn clear_cached_token_db(db_path: &str) {
    if let Ok(conn) = crate::db::open_db(db_path) {
        let _ = crate::db::system::set_system_config(&conn, SESSION_CACHE_DB_KEY, "");
    }
}

// ---------------------------------------------------------------------------
// Helpers (small, self-contained — no need to share with web_ui)
// ---------------------------------------------------------------------------

fn base64_url_encode_bytes(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    URL_SAFE_NO_PAD.encode(data)
}

fn simple_urlencode(s: &str) -> String {
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

fn parse_query_string(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            let key = urldecode(key);
            let value = urldecode(value);
            params.insert(key, value);
        }
    }
    params
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn exchange_token_for_test(client: &Client, server_base: &str) -> anyhow::Result<String> {
        let token_url = format!("{}/api/v1/oauth/token", server_base);
        let token_body = json!({
            "grant_type": "authorization_code",
            "code": "invalid-code",
            "redirect_uri": "http://127.0.0.1:8977/oauth/callback",
            "code_verifier": "verifier-test",
            "client_id": crate::config::CLIENT_ID,
            "device_id": "device-test",
            "product_id": crate::config::PRODUCT_ID,
        });

        let token_resp: OAuthTokenData = request_json_encrypted(
            client.post(&token_url).json(&token_body),
            "oauth token exchange",
            "verifier-test",
        )
        .await?;

        Ok(token_resp.session_token)
    }

    #[tokio::test]
    async fn oauth_token_exchange_preserves_structured_server_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let body = r#"{"success":false,"error":{"code":"INVALID_CODE","message":"Authorization code not found or already used"}}"#;
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let client = Client::new();
        let err = exchange_token_for_test(&client, &format!("http://{}", addr))
            .await
            .expect_err("oauth token exchange should surface server auth error");
        let err_text = format!("{:#}", err);

        assert!(
            !err_text.contains("invalid json response"),
            "structured OAuth server errors should not be collapsed into invalid json response: {}",
            err_text
        );
        assert!(
            err_text.contains("INVALID_CODE")
                || err_text.contains("Authorization code not found or already used"),
            "OAuth token exchange should preserve server error semantics: {}",
            err_text
        );
    }
}
