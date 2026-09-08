use anyhow::{anyhow, Context};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::time::Instant;
use uuid::Uuid;

use crate::config::REQUEST_ID_HEADER;
use crate::crypto::{
    build_canonical_string, compute_request_signature, derive_signing_key, transport_decrypt,
    transport_encrypt, verify_response_signature,
};
use crate::types::ApiErrorResponse;

fn parse_retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    let raw = headers.get("Retry-After")?.to_str().ok()?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(secs) = raw.parse::<u64>() {
        if secs > 0 {
            return Some(secs.saturating_mul(1000));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub(crate) struct UpstreamApiError {
    pub(crate) status: String,
    pub(crate) code: String,
    pub(crate) message: String,
}

impl std::fmt::Display for UpstreamApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for UpstreamApiError {}

pub(crate) fn http_status_line(status: reqwest::StatusCode) -> String {
    let reason = status.canonical_reason().unwrap_or("Unknown");
    format!("{} {}", status.as_u16(), reason)
}

fn parse_upstream_api_error(body: &str, status: reqwest::StatusCode) -> Option<UpstreamApiError> {
    parse_api_error_response(body).map(|api_error| UpstreamApiError {
        status: http_status_line(status),
        code: api_error.error.code,
        message: api_error.error.message,
    })
}

pub(crate) async fn request_json<T>(
    builder: reqwest::RequestBuilder,
    label: &str,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let request_id = build_request_id(label);
    let response = builder
        .header(REQUEST_ID_HEADER, request_id)
        .send()
        .await
        .with_context(|| format!("{}: request failed", label))?;
    let status = response.status();
    let url = response.url().to_string();
    let body = response
        .text()
        .await
        .with_context(|| format!("{}: read body failed ({})", label, url))?;

    if body.trim().is_empty() {
        return Err(anyhow!(
            "{}: empty response body (status={}, url={})",
            label,
            status,
            url
        ));
    }

    if let Some(api_error) = parse_upstream_api_error(&body, status) {
        return Err(api_error.into());
    }

    serde_json::from_str::<T>(&body).with_context(|| {
        let snippet = if body.len() > 220 {
            format!("{}...", &body[..body.floor_char_boundary(220)])
        } else {
            body.clone()
        };
        format!(
            "{}: invalid json response (status={}, url={}, body={})",
            label, status, url, snippet
        )
    })
}

// -- Server response encryption envelope (RFC #183) --

fn skip_catalog_signature_check() -> bool {
    std::env::var("WPTSALL_SKIP_SIGNATURE_CHECK")
        .ok()
        .as_deref()
        == Some("true")
}

/// Like `request_json()`, but handles Server-side response encryption.
/// Server always returns encrypted envelopes for sensitive endpoints (v1.2.1+).
/// The response is decrypted transparently using HKDF-SHA256 + AES-256-GCM
/// with the provided `ikm` (session_token or code_verifier).
pub(crate) async fn request_json_encrypted<T>(
    builder: reqwest::RequestBuilder,
    label: &str,
    ikm: &str,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    request_json_encrypted_with_signing_key(builder, label, ikm, None).await
}

/// Like `request_json_encrypted`, but verifies RSA catalog signatures when present
/// (L1/L2 lists) using the server signing public key PEM.
pub(crate) async fn request_json_encrypted_with_signing_key<T>(
    builder: reqwest::RequestBuilder,
    label: &str,
    ikm: &str,
    trusted_public_key_pem: Option<&str>,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let request_id = build_request_id(label);
    let response = builder
        .header(REQUEST_ID_HEADER, request_id)
        .send()
        .await
        .with_context(|| format!("{}: request failed", label))?;
    let status = response.status();
    let url = response.url().to_string();
    let body = response
        .text()
        .await
        .with_context(|| format!("{}: read body failed ({})", label, url))?;

    if body.trim().is_empty() {
        return Err(anyhow!(
            "{}: empty response body (status={}, url={})",
            label,
            status,
            url
        ));
    }

    let plaintext_json = try_decrypt_response_body(&body, ikm, trusted_public_key_pem)
        .with_context(|| {
            format!(
                "{}: encrypted response detected but decryption failed (status={}, url={})",
                label, status, url
            )
        })?;
    let json_str = plaintext_json.as_deref().unwrap_or(&body);

    if let Some(api_error) = parse_upstream_api_error(json_str, status) {
        return Err(api_error.into());
    }

    serde_json::from_str::<T>(json_str).with_context(|| {
        let snippet = if json_str.len() > 220 {
            format!("{}...", &json_str[..json_str.floor_char_boundary(220)])
        } else {
            json_str.to_string()
        };
        format!(
            "{}: invalid json response (status={}, url={}, body={})",
            label, status, url, snippet
        )
    })
}

/// Try to detect and decrypt an encrypted Server response envelope.
/// When `signature` is present, verifies RSA catalog signature before decrypt.
pub(crate) fn try_decrypt_response_body(
    body: &str,
    ikm: &str,
    trusted_public_key_pem: Option<&str>,
) -> anyhow::Result<Option<String>> {
    client_runtime_core::signed_catalog::try_decrypt_server_response_body(
        body,
        ikm,
        trusted_public_key_pem,
        skip_catalog_signature_check(),
    )
}

pub(crate) fn build_request_id(label: &str) -> String {
    let normalized = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let prefix = if normalized.is_empty() {
        "req".to_string()
    } else {
        normalized.chars().take(24).collect()
    };
    format!("{}-{}", prefix, Uuid::new_v4().simple())
}

pub(crate) fn parse_api_error_response(body: &str) -> Option<ApiErrorResponse> {
    serde_json::from_str::<ApiErrorResponse>(body)
        .ok()
        .filter(|resp| !resp.success)
}

/// Whether WP REST bodies should use AES transport encryption.
///
/// Default (`https_optional`, WP.org-friendly): encrypt on **HTTP only**; on
/// HTTPS rely on TLS (matches `Transport_Middleware` default policy).
/// Opt-in: set env `WPTSALL_WP_TRANSPORT_ENCRYPT=always` to encrypt HTTPS too.
pub(crate) fn requires_transport_encryption(url: &str) -> bool {
    let lower = url.trim().to_lowercase();
    let is_http = lower.starts_with("http://");
    let is_https = lower.starts_with("https://");
    if !is_http && !is_https {
        return false;
    }
    match std::env::var("WPTSALL_WP_TRANSPORT_ENCRYPT")
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "always" | "1" | "true" => true,
        "off" | "0" | "false" => false,
        _ => is_http,
    }
}

pub(crate) fn apply_wp_protocol_headers(
    mut request: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    request = request.header("X-WPTSALL-Protocol-Version", "2").header(
        crate::contract_capabilities::CONTRACT_CAPABILITIES_HEADER,
        crate::contract_capabilities::client_contract_capabilities_json(),
    );
    request
}

/// Extract the canonical path+query component from a URL for request signing.
///
/// The path excludes the `/wp-json` prefix and uses a sorted query string so
/// Client and WP can sign the same bytes deterministically.
fn extract_url_path_with_query(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return "/".to_string();
    };

    let raw_path = parsed.path();
    let path = raw_path.strip_prefix("/wp-json").unwrap_or(raw_path);

    let mut query_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if query_pairs.is_empty() {
        return path.to_string();
    }

    query_pairs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let query = query_pairs
        .into_iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                canonical_query_component(&key),
                canonical_query_component(&value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{}", path, query)
}

fn canonical_query_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Generate request signing headers (Protocol v2).
///
/// Returns `(timestamp, nonce, signature)` to be set as
/// `X-WPTSALL-Timestamp`, `X-WPTSALL-Signature-Nonce`, `X-WPTSALL-Signature`.
#[allow(dead_code)]
pub(crate) fn sign_request(
    method: &str,
    url: &str,
    token: &str,
    body: &[u8],
) -> (String, String, String) {
    sign_request_with_headers(method, url, token, body, &[])
}

pub(crate) fn sign_request_with_headers(
    method: &str,
    url: &str,
    token: &str,
    body: &[u8],
    signed_headers: &[(&str, &str)],
) -> (String, String, String) {
    let signing_key = derive_signing_key(token);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();
    let nonce = Uuid::new_v4().to_string();
    let path_with_query = extract_url_path_with_query(url);
    let canonical = build_canonical_string(
        method,
        &path_with_query,
        &timestamp,
        &nonce,
        body,
        signed_headers,
    );
    let signature = compute_request_signature(&signing_key, &canonical);
    (timestamp, nonce, signature)
}

/// Process a WP transport response: decrypt if encrypted, verify response signature.
fn process_wp_response(
    body_text: &str,
    transport_header: &str,
    response_sig_header: Option<&str>,
    token: &str,
    url: &str,
    encrypt: bool,
) -> anyhow::Result<serde_json::Value> {
    // Determine the plaintext JSON bytes for signature verification
    let (value, plaintext_bytes) = if transport_header == "encrypted" {
        let envelope: serde_json::Value = serde_json::from_str(body_text)
            .with_context(|| "parse encrypted response envelope failed")?;
        let algorithm = envelope
            .get("algorithm")
            .and_then(|v| v.as_str())
            .unwrap_or("aes-256-gcm-v1");
        if algorithm != "aes-256-gcm-v1" {
            return Err(anyhow!(
                "unsupported transport response algorithm: {}",
                algorithm
            ));
        }
        let payload = envelope
            .get("encrypted_payload")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing encrypted_payload in response"))?;
        let nonce = envelope
            .get("nonce")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing nonce in response"))?;
        let decrypted = transport_decrypt(payload, nonce, token)
            .with_context(|| "transport decrypt response failed")?;
        let val: serde_json::Value = serde_json::from_slice(&decrypted)
            .with_context(|| "parse decrypted response json failed")?;
        (val, decrypted)
    } else {
        let val: serde_json::Value = serde_json::from_str(body_text)
            .with_context(|| format!("parse wp transport response json failed ({})", url))?;
        (val, body_text.as_bytes().to_vec())
    };

    verify_wp_response_signature_for_plaintext(
        token,
        &plaintext_bytes,
        response_sig_header,
        url,
        encrypt,
    )?;

    Ok(value)
}

fn decode_wp_error_body(
    body_text: &str,
    transport_header: &str,
    response_sig_header: Option<&str>,
    token: &str,
    url: &str,
    transport_required: bool,
) -> String {
    match process_wp_response(
        body_text,
        transport_header,
        response_sig_header,
        token,
        url,
        transport_required,
    ) {
        Ok(value) => serde_json::to_string(&value).unwrap_or_else(|_| body_text.to_string()),
        Err(_) => body_text.to_string(),
    }
}

/// Verify the WP v2 response signature against plaintext response bytes.
///
/// When `required` is true (Protocol v2 path), missing signature is treated as
/// an error. When false, signature is optional but still verified if present.
pub(crate) fn verify_wp_response_signature_for_plaintext(
    token: &str,
    plaintext_bytes: &[u8],
    response_sig_header: Option<&str>,
    url: &str,
    required: bool,
) -> anyhow::Result<()> {
    let skip_sig_check = std::env::var("WPTSALL_SKIP_SIGNATURE_CHECK")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    let maybe_sig = response_sig_header
        .map(str::trim)
        .filter(|sig| !sig.is_empty());

    if required && maybe_sig.is_none() {
        if skip_sig_check {
            eprintln!(
                "[WARN] WPTSALL_SKIP_SIGNATURE_CHECK=true — response signature missing for {} (development only!)",
                url
            );
            return Ok(());
        }
        return Err(anyhow!(
            "response signature missing but required by Protocol v2 ({})",
            url
        ));
    }

    if let Some(sig) = maybe_sig {
        let signing_key = derive_signing_key(token);
        if !verify_response_signature(&signing_key, plaintext_bytes, sig) {
            if skip_sig_check {
                eprintln!(
                    "[WARN] WPTSALL_SKIP_SIGNATURE_CHECK=true — response signature verification failed for {} (development only!)",
                    url
                );
                return Ok(());
            }
            return Err(anyhow!("response signature verification failed ({})", url));
        }
    }

    Ok(())
}

/// Build a request with transport encryption + request signing (Protocol v2).
///
/// `route_secret`: optional route secret used to build the secret-scoped URL.
/// The Client no longer sends a duplicate `X-Route-Secret` header because the
/// secret is already present in the path and some HTTP stacks may not preserve
/// the duplicate header consistently enough for signature verification.
pub(crate) async fn wp_request_with_transport(
    client: &Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    worker_id: &str,
    body: &serde_json::Value,
    _route_secret: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let started = Instant::now();
    let request_id = build_request_id("wp-transport");
    let is_get = method == reqwest::Method::GET;
    let transport_required = requires_transport_encryption(url);
    let encrypt_body = transport_required && !is_get;

    // Determine the body bytes for signing (before encryption).
    // GET requests sign an empty body.
    let body_bytes_for_signing: Vec<u8> = if is_get {
        Vec::new()
    } else {
        serde_json::to_vec(body).with_context(|| "serialize request body failed")?
    };

    // The route secret is already encoded into the WP client URL path.
    // Keep the duplicate header only as a best-effort compatibility signal,
    // but do not include it in the signed-header canonical set because some
    // HTTP stacks may normalize or drop the duplicate header in transit.
    let signed_headers: Vec<(&str, &str)> = Vec::new();
    let (timestamp, sig_nonce, signature) = sign_request_with_headers(
        method.as_str(),
        url,
        token,
        &body_bytes_for_signing,
        &signed_headers,
    );

    let mut request = apply_wp_protocol_headers(client.request(method, url))
        .header("X-WPTSALL-Client-Token", token)
        .header("X-WPTSALL-Worker-Id", worker_id)
        .header("X-Client-Version", env!("CARGO_PKG_VERSION"))
        .header("X-WPTSALL-Timestamp", &timestamp)
        .header("X-WPTSALL-Signature-Nonce", &sig_nonce)
        .header("X-WPTSALL-Signature", &signature)
        .header(REQUEST_ID_HEADER, request_id);

    // The worker id is seeded from the client's stable local device id.  Allow
    // an explicit override for deployments that intentionally use a distinct
    // WP device identity, but never silently omit the device binding.
    let device_id = crate::config::env_or("WPTSALL_WP_DEVICE_ID", worker_id);
    if !device_id.is_empty() {
        request = request.header("X-WPTSALL-Device-Id", device_id);
    }

    if encrypt_body {
        let (encrypted_payload, nonce) = transport_encrypt(&body_bytes_for_signing, token)
            .with_context(|| "transport encrypt failed")?;
        let envelope = serde_json::json!({
            "encrypted_payload": encrypted_payload,
            "nonce": nonce,
            "algorithm": "aes-256-gcm-v1",
        });
        request = request
            .header("X-WPTSALL-Transport", "encrypted")
            .header("X-WPTSALL-Nonce", &nonce)
            .json(&envelope);
    } else if is_get && transport_required {
        // Protocol v2: GET requests send transport headers to trigger encrypted
        // responses from the WP Plugin, but do not encrypt the (empty) body.
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        use rand::RngCore;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce_b64 = URL_SAFE_NO_PAD.encode(nonce_bytes);
        request = request
            .header("X-WPTSALL-Transport", "encrypted")
            .header("X-WPTSALL-Nonce", &nonce_b64);
    } else if !is_get {
        request = request.json(body);
    }

    let response = request.send().await.with_context(|| {
        format!(
            "wp transport request failed (url={}, elapsed_ms={})",
            url,
            started.elapsed().as_millis()
        )
    })?;
    let status = response.status();
    let retry_after_ms = parse_retry_after_ms(response.headers());
    let transport_header = response
        .headers()
        .get("X-WPTSALL-Transport")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let response_sig = response
        .headers()
        .get("X-WPTSALL-Response-Signature")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body_text = response.text().await.with_context(|| {
        format!(
            "wp transport response read failed (url={}, elapsed_ms={})",
            url,
            started.elapsed().as_millis()
        )
    })?;

    if !status.is_success() {
        let retry_after_note = retry_after_ms
            .map(|v| format!(", retry_after_ms={}", v))
            .unwrap_or_default();
        let decoded_body = decode_wp_error_body(
            &body_text,
            &transport_header,
            response_sig.as_deref(),
            token,
            url,
            transport_required,
        );
        return Err(anyhow!(
            "wp transport non-2xx (status={}, url={}, elapsed_ms={}, body={}{})",
            status,
            url,
            started.elapsed().as_millis(),
            if decoded_body.len() > 500 {
                &decoded_body[..decoded_body.floor_char_boundary(500)]
            } else {
                &decoded_body
            },
            retry_after_note
        ));
    }

    process_wp_response(
        &body_text,
        &transport_header,
        response_sig.as_deref(),
        token,
        url,
        transport_required,
    )
}

/// Transport-aware typed GET request to a WP endpoint.
///
/// Sends an empty JSON body (`{}`) through `wp_request_with_transport` and
/// deserializes the (possibly decrypted) response into `T`.
#[allow(dead_code)]
pub(crate) async fn wp_get_json_with_transport<T>(
    client: &Client,
    url: &str,
    token: &str,
    worker_id: &str,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    wp_get_json_with_transport_and_secret(client, url, token, worker_id, None).await
}

/// Transport-aware typed GET request with optional route_secret.
pub(crate) async fn wp_get_json_with_transport_and_secret<T>(
    client: &Client,
    url: &str,
    token: &str,
    worker_id: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let value = wp_request_with_transport(
        client,
        reqwest::Method::GET,
        url,
        token,
        worker_id,
        &serde_json::json!({}),
        route_secret,
    )
    .await?;
    let result: T = serde_json::from_value(value)
        .with_context(|| format!("wp_get_json_with_transport: deserialize failed ({})", url))?;
    Ok(result)
}

/// Transport-aware POST request to a WP endpoint with extra headers.
///
/// Encrypts/decrypts the request/response when required, signs the request
/// with HMAC-SHA256, and allows adding custom headers such as `Idempotency-Key`.
///
/// Always sends `X-Client-Version`.
pub(crate) async fn wp_post_with_transport_and_headers(
    client: &Client,
    url: &str,
    token: &str,
    worker_id: &str,
    body: &serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> anyhow::Result<serde_json::Value> {
    let started = Instant::now();
    let request_id = build_request_id("wp-transport");
    let encrypt = requires_transport_encryption(url);

    let body_bytes = serde_json::to_vec(body).with_context(|| "serialize request body failed")?;

    let signed_headers: Vec<(&str, &str)> = extra_headers
        .iter()
        .copied()
        .filter(|(key, value)| {
            !key.trim().is_empty()
                && !value.trim().is_empty()
                && !key.eq_ignore_ascii_case("X-Route-Secret")
        })
        .collect();
    let (timestamp, sig_nonce, signature) =
        sign_request_with_headers("POST", url, token, &body_bytes, &signed_headers);

    let mut request = apply_wp_protocol_headers(client.request(reqwest::Method::POST, url))
        .header("X-WPTSALL-Client-Token", token)
        .header("X-WPTSALL-Worker-Id", worker_id)
        .header("X-Client-Version", env!("CARGO_PKG_VERSION"))
        .header("X-WPTSALL-Timestamp", &timestamp)
        .header("X-WPTSALL-Signature-Nonce", &sig_nonce)
        .header("X-WPTSALL-Signature", &signature)
        .header(REQUEST_ID_HEADER, request_id);

    let device_id = crate::config::env_or("WPTSALL_WP_DEVICE_ID", worker_id);
    if !device_id.is_empty() {
        request = request.header("X-WPTSALL-Device-Id", device_id);
    }

    for (key, value) in extra_headers {
        if key.eq_ignore_ascii_case("X-Route-Secret") {
            continue;
        }
        request = request.header(*key, *value);
    }

    if encrypt {
        let (encrypted_payload, nonce) =
            transport_encrypt(&body_bytes, token).with_context(|| "transport encrypt failed")?;
        let envelope = serde_json::json!({
            "encrypted_payload": encrypted_payload,
            "nonce": nonce,
            "algorithm": "aes-256-gcm-v1",
        });
        request = request
            .header("X-WPTSALL-Transport", "encrypted")
            .header("X-WPTSALL-Nonce", &nonce)
            .json(&envelope);
    } else {
        request = request.json(body);
    }

    let response = request.send().await.with_context(|| {
        format!(
            "wp transport request failed (url={}, elapsed_ms={})",
            url,
            started.elapsed().as_millis()
        )
    })?;
    let status = response.status();
    let retry_after_ms = parse_retry_after_ms(response.headers());
    let transport_header = response
        .headers()
        .get("X-WPTSALL-Transport")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let response_sig = response
        .headers()
        .get("X-WPTSALL-Response-Signature")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body_text = response.text().await.with_context(|| {
        format!(
            "wp transport response read failed (url={}, elapsed_ms={})",
            url,
            started.elapsed().as_millis()
        )
    })?;

    if !status.is_success() {
        let retry_after_note = retry_after_ms
            .map(|v| format!(", retry_after_ms={}", v))
            .unwrap_or_default();
        return Err(anyhow!(
            "wp transport non-2xx (status={}, url={}, elapsed_ms={}, body={}{})",
            status,
            url,
            started.elapsed().as_millis(),
            if body_text.len() > 200 {
                &body_text[..body_text.floor_char_boundary(200)]
            } else {
                &body_text
            },
            retry_after_note
        ));
    }

    process_wp_response(
        &body_text,
        &transport_header,
        response_sig.as_deref(),
        token,
        url,
        encrypt,
    )
}

/// Detect WP transport 401 caused by HMAC signature mismatch (token rotation).
///
/// WP Plugin returns HTTP 401 + `signature_invalid` when the Client token has
/// rotated and the old HMAC-derived key no longer matches.  This is distinct
/// from Server OAuth session errors (handled by `is_auth_error_message`).
///
/// Note: WP 403 from Client API availability/license gating is permanent,
/// and should not be treated as a token-rotation retry path.
pub(crate) fn is_wp_token_rotation_error(err_text: &str) -> bool {
    let msg = err_text.to_lowercase();
    // Must be a WP transport 401 (not a Server session error)
    (msg.contains("status=401") || msg.contains("status 401")) && msg.contains("wp transport")
}

pub(crate) fn is_auth_error_message(err_text: &str) -> bool {
    let msg = err_text.to_lowercase();
    // WP transport 401 is token rotation, not a Server session error.
    // It is handled separately by is_wp_token_rotation_error.
    if msg.contains("wp transport") {
        return false;
    }
    msg.contains("session_revoked")
        || msg.contains("session_expired")
        || msg.contains("session_required")
        || msg.contains("not_logged_in")
        || msg.contains("unauthorized")
        || msg.contains("status=401")
        || msg.contains("status 401")
        || msg.contains("relogin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::TestEnvVarGuard as EnvVarGuard;

    // -----------------------------------------------------------------------
    // build_request_id
    // -----------------------------------------------------------------------

    #[test]
    fn build_request_id_includes_label() {
        let id = build_request_id("client login");
        assert!(id.starts_with("client-login-"), "id: {}", id);
    }

    #[test]
    fn build_request_id_normalizes_special_chars() {
        let id = build_request_id("task.pull (status=pending)");
        // Should not contain dots, parens, equals
        let prefix = id.split('-').take(3).collect::<Vec<_>>().join("-");
        assert!(
            prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "prefix should be normalized: {}",
            prefix
        );
    }

    #[test]
    fn build_request_id_truncates_long_label() {
        let long_label = "a".repeat(100);
        let id = build_request_id(&long_label);
        // Prefix should be at most 24 chars
        let parts: Vec<&str> = id.splitn(2, '-').collect();
        assert!(parts[0].len() <= 24, "prefix too long: {}", parts[0]);
    }

    #[test]
    fn build_request_id_empty_label() {
        let id = build_request_id("");
        assert!(id.starts_with("req-"), "id: {}", id);
    }

    #[test]
    fn build_request_id_unique() {
        let id1 = build_request_id("test");
        let id2 = build_request_id("test");
        assert_ne!(id1, id2, "each call should produce unique id");
    }

    // -----------------------------------------------------------------------
    // parse_api_error_response
    // -----------------------------------------------------------------------

    #[test]
    fn parse_api_error_valid() {
        let body = r#"{"success": false, "error": {"code": "session_expired", "message": "Your session has expired"}}"#;
        let parsed = parse_api_error_response(body);
        assert!(parsed.is_some());
        let resp = parsed.unwrap();
        assert!(!resp.success);
        assert_eq!(resp.error.code, "session_expired");
    }

    #[test]
    fn parse_api_error_success_true_returns_none() {
        let body = r#"{"success": true, "error": {"code": "ok", "message": "all good"}}"#;
        let parsed = parse_api_error_response(body);
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_api_error_invalid_json_returns_none() {
        assert!(parse_api_error_response("not json").is_none());
        assert!(parse_api_error_response("").is_none());
    }

    // -----------------------------------------------------------------------
    // is_auth_error_message
    // -----------------------------------------------------------------------

    #[test]
    fn is_auth_error_detects_session_errors() {
        assert!(is_auth_error_message(
            "session_revoked: kicked by new login"
        ));
        assert!(is_auth_error_message("session_expired"));
        assert!(is_auth_error_message("session_required"));
        assert!(is_auth_error_message("NOT_LOGGED_IN: please login first"));
        assert!(is_auth_error_message("Unauthorized access"));
    }

    #[test]
    fn is_auth_error_detects_401_status() {
        assert!(is_auth_error_message("request failed: status=401"));
        assert!(is_auth_error_message("got status 401 from server"));
    }

    #[test]
    fn is_auth_error_detects_relogin() {
        assert!(is_auth_error_message("relogin required"));
    }

    #[test]
    fn is_auth_error_case_insensitive() {
        assert!(is_auth_error_message("SESSION_REVOKED"));
        assert!(is_auth_error_message("Unauthorized"));
    }

    #[test]
    fn is_auth_error_rejects_normal_errors() {
        assert!(!is_auth_error_message("network timeout"));
        assert!(!is_auth_error_message("parse error"));
        assert!(!is_auth_error_message("status=500"));
        assert!(!is_auth_error_message(""));
    }

    // -----------------------------------------------------------------------
    // is_wp_token_rotation_error
    // -----------------------------------------------------------------------

    #[test]
    fn wp_token_rotation_detects_wp_transport_401() {
        assert!(is_wp_token_rotation_error(
            "wp transport non-2xx (status=401, url=https://blog.wpmm.cc/..., body={\"code\":\"signature_invalid\"})"
        ));
        assert!(is_wp_token_rotation_error(
            "wp transport request failed: status 401"
        ));
    }

    #[test]
    fn wp_token_rotation_rejects_server_401() {
        // Server session errors are NOT WP token rotation
        assert!(!is_wp_token_rotation_error("status=401 session_revoked"));
        assert!(!is_wp_token_rotation_error("got status 401 from server"));
    }

    #[test]
    fn wp_token_rotation_rejects_non_401() {
        assert!(!is_wp_token_rotation_error(
            "wp transport non-2xx (status=403, url=...)"
        ));
        assert!(!is_wp_token_rotation_error("status=500"));
        assert!(!is_wp_token_rotation_error("network timeout"));
        assert!(!is_wp_token_rotation_error(""));
    }

    // -----------------------------------------------------------------------
    // requires_transport_encryption
    // -----------------------------------------------------------------------

    #[test]
    fn transport_encryption_required_for_http() {
        // Env-locked with a NEUTRAL value (default branch: http requires
        // encryption) so concurrent env users cannot flip the outcome.
        let _guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "default-auto");
        assert!(requires_transport_encryption("http://example.com/api"));
    }

    #[test]
    fn transport_encryption_not_required_for_https_by_default() {
        // Env-locked instead of a bare remove_var: these tests run
        // concurrently and the env is process-global. The neutral value
        // exercises the default branch (https opt-out).
        let _guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "default-auto");
        assert!(!requires_transport_encryption("https://example.com/api"));
    }

    #[test]
    fn transport_encryption_https_when_env_always() {
        let _guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "always");
        assert!(requires_transport_encryption("https://example.com/api"));
    }

    #[test]
    fn transport_encryption_required_case_insensitive() {
        // Env-locked: concurrent tests must not flip the opt-in flag.
        let _guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "default-auto");
        assert!(requires_transport_encryption("HTTP://EXAMPLE.COM/api"));
        assert!(!requires_transport_encryption("HTTPS://example.com/api"));
    }

    #[test]
    fn transport_encryption_not_required_for_other_schemes() {
        assert!(!requires_transport_encryption("ftp://example.com/file"));
        assert!(!requires_transport_encryption("ws://example.com/sock"));
        assert!(!requires_transport_encryption(""));
        assert!(!requires_transport_encryption("not-a-url"));
    }

    #[test]
    fn extract_url_path_with_query_strips_wp_json_and_sorts_query() {
        let url = "https://blog.wpmm.cc/wp-json/wptsall/v2/abc/client/media-upload/status?z=9&upload_id=up-1&a=1";
        let path = extract_url_path_with_query(url);
        assert_eq!(
            path,
            "/wptsall/v2/abc/client/media-upload/status?a=1&upload_id=up-1&z=9"
        );
    }

    #[test]
    fn extract_url_path_with_query_uses_rfc3986_encoding() {
        let url =
            "https://blog.wpmm.cc/wp-json/wptsall/v2/abc/client/content?search=hello world&tag=a%2Bb";
        let path = extract_url_path_with_query(url);
        assert_eq!(
            path,
            "/wptsall/v2/abc/client/content?search=hello%20world&tag=a%2Bb"
        );
    }

    #[test]
    fn canonical_signature_changes_when_signed_headers_change() {
        let base = crate::crypto::build_canonical_string(
            "POST",
            "/wptsall/v2/abc/client/media-upload",
            "1700000000",
            "nonce-1",
            b"payload",
            &[("X-WPTSALL-Task-ID", "1")],
        );
        let changed = crate::crypto::build_canonical_string(
            "POST",
            "/wptsall/v2/abc/client/media-upload",
            "1700000000",
            "nonce-1",
            b"payload",
            &[("X-WPTSALL-Task-ID", "2")],
        );
        assert_ne!(
            base, changed,
            "signed header changes must affect canonical string"
        );
    }

    // -----------------------------------------------------------------------
    // try_decrypt_response_body (RFC #183 envelope detection)
    // -----------------------------------------------------------------------

    /// Build an encrypted envelope JSON string for testing.
    fn build_test_encrypted_envelope(plaintext: &[u8], kdf_info: &str, ikm: &str) -> String {
        use aes_gcm::aead::generic_array::GenericArray;
        use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit};
        use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
        use base64::Engine;
        use hkdf::Hkdf;
        use rand::RngCore;
        use sha2::Sha256;

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce_b64url = URL_SAFE_NO_PAD.encode(nonce_bytes);

        let hk = Hkdf::<Sha256>::new(Some(nonce_b64url.as_bytes()), ikm.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(kdf_info.as_bytes(), &mut key).unwrap();

        let hk_nonce = Hkdf::<Sha256>::new(Some(kdf_info.as_bytes()), nonce_b64url.as_bytes());
        let mut gcm_nonce = [0u8; 12];
        hk_nonce
            .expand(b"wptsall-response-nonce-v1", &mut gcm_nonce)
            .unwrap();

        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let encrypted = cipher
            .encrypt(GenericArray::from_slice(&gcm_nonce), plaintext)
            .unwrap();

        let payload_b64 = STANDARD.encode(&encrypted);

        serde_json::json!({
            "success": true,
            "encrypted": true,
            "data": {
                "encrypted_payload": payload_b64,
                "nonce": nonce_b64url,
                "algorithm": "AES-256-GCM",
                "kdf_version": "hkdf-sha256-v1",
                "kdf_info": kdf_info
            }
        })
        .to_string()
    }

    #[test]
    fn try_decrypt_detects_encrypted_envelope() {
        let plaintext = br#"{"success":true,"data":{"items":[{"id":"d1"}]}}"#;
        let ikm = "sess_test_token_123";
        let body = build_test_encrypted_envelope(plaintext, "wptsall-domains-v1", ikm);

        let result = try_decrypt_response_body(&body, ikm, None).unwrap();
        assert!(result.is_some(), "should detect and decrypt envelope");
        let decrypted = result.unwrap();
        assert_eq!(decrypted, String::from_utf8_lossy(plaintext));
    }

    #[test]
    fn try_decrypt_returns_none_for_plaintext() {
        let body = r#"{"success":true,"data":{"items":[]}}"#;
        let result = try_decrypt_response_body(body, "any-ikm", None).unwrap();
        assert!(result.is_none(), "plain response should return None");
    }

    #[test]
    fn try_decrypt_returns_none_for_non_json() {
        let result = try_decrypt_response_body("not json at all", "ikm", None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_decrypt_returns_none_when_encrypted_is_false() {
        let body = r#"{"success":true,"encrypted":false,"data":{"items":[]}}"#;
        let result = try_decrypt_response_body(body, "ikm", None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_decrypt_returns_none_when_encrypted_missing() {
        let body = r#"{"success":true,"data":{"items":[]}}"#;
        let result = try_decrypt_response_body(body, "ikm", None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_decrypt_returns_error_with_wrong_ikm() {
        let plaintext = b"secret";
        let body = build_test_encrypted_envelope(plaintext, "wptsall-domains-v1", "correct-ikm");
        let result = try_decrypt_response_body(&body, "wrong-ikm", None);
        assert!(result.is_err(), "wrong IKM must fail decryption");
    }

    #[test]
    fn try_decrypt_rejects_unsupported_algorithm() {
        let body = serde_json::json!({
            "success": true,
            "encrypted": true,
            "data": {
                "encrypted_payload": "ZmFrZQ==",
                "nonce": "ZmFrZQ",
                "algorithm": "ChaCha20-Poly1305",
                "kdf_version": "hkdf-sha256-v1",
                "kdf_info": "wptsall-domains-v1"
            }
        })
        .to_string();
        let result = try_decrypt_response_body(&body, "ikm", None);
        assert!(result.is_err());
    }

    #[test]
    fn try_decrypt_rejects_unsupported_kdf_version() {
        let body = serde_json::json!({
            "success": true,
            "encrypted": true,
            "data": {
                "encrypted_payload": "ZmFrZQ==",
                "nonce": "ZmFrZQ",
                "algorithm": "AES-256-GCM",
                "kdf_version": "hkdf-sha512-v1",
                "kdf_info": "wptsall-domains-v1"
            }
        })
        .to_string();
        let result = try_decrypt_response_body(&body, "ikm", None);
        assert!(result.is_err());
    }

    #[test]
    fn verify_wp_response_signature_requires_header_when_required() {
        let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", String::new());
        let result = verify_wp_response_signature_for_plaintext(
            "wptc1.test.123.sig",
            br#"{"success":true}"#,
            None,
            "https://example.com/wp-json/wptsall/v2/abc/client/media-upload",
            true,
        );
        assert!(result.is_err());
    }

    #[test]
    fn verify_wp_response_signature_fails_on_invalid_signature() {
        let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", String::new());
        let result = verify_wp_response_signature_for_plaintext(
            "wptc1.test.123.sig",
            br#"{"success":true}"#,
            Some("invalid-signature"),
            "https://example.com/wp-json/wptsall/v2/abc/client/media-upload",
            true,
        );
        assert!(result.is_err());
    }

    #[test]
    fn verify_wp_response_signature_passes_on_valid_signature() {
        let token = "wptc1.test.123.sig";
        let body = r#"{"success":true,"data":{"ok":1}}"#;
        let signing_key = derive_signing_key(token);
        let sig = crate::crypto::compute_request_signature(&signing_key, body);

        let result = verify_wp_response_signature_for_plaintext(
            token,
            body.as_bytes(),
            Some(sig.as_str()),
            "https://example.com/wp-json/wptsall/v2/abc/client/media-upload",
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn verify_wp_response_signature_can_skip_missing_header_in_dev_mode() {
        let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());
        let result = verify_wp_response_signature_for_plaintext(
            "wptc1.test.123.sig",
            br#"{"success":true}"#,
            None,
            "https://example.com/wp-json/wptsall/v2/abc/client/ping",
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn verify_wp_response_signature_can_skip_invalid_signature_in_dev_mode() {
        let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());
        let result = verify_wp_response_signature_for_plaintext(
            "wptc1.test.123.sig",
            br#"{"success":true}"#,
            Some("invalid-signature"),
            "https://example.com/wp-json/wptsall/v2/abc/client/ping",
            true,
        );
        assert!(result.is_ok());
    }
}
