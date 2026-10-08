use anyhow::{anyhow, Context};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::time::Instant;
use uuid::Uuid;

use crate::config::{REQUEST_ID_HEADER, TRACE_ID_HEADER};
use crate::crypto::{
    build_canonical_string, compute_request_signature, derive_signing_key, transport_decrypt,
    transport_encrypt, verify_response_signature,
};
use crate::types::ApiErrorResponse;

pub(crate) fn wp_http_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .connect_timeout(std::time::Duration::from_secs(10))
}

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

/// GAP-06 收尾 (批 J): run-scoped outbound trace id.
///
/// One id per discovery/sync run, attached as `X-WPTSALL-Trace-Id` on every
/// WP-bound and vendor-bound request issued while the guard is alive, so
/// client, WP plugin and mock/ provider log lines correlate on one run id.
/// Scoped static state (not a threaded parameter) because the outbound choke
/// points sit dozens of layers below the run boundary; the guard restores the
/// previous value on drop. The worker executes run lanes sequentially, so at
/// most one run trace is active at a time — requests outside a run simply
/// carry no trace header (wire-compat with older peers).
static RUN_TRACE_ID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Read the active run trace id, if any (best-effort; poisoned state = None).
pub(crate) fn current_run_trace_id() -> Option<String> {
    RUN_TRACE_ID.lock().ok().and_then(|guard| guard.clone())
}

/// RAII guard restoring the previous run trace id on drop.
pub(crate) struct RunTraceIdGuard {
    previous: Option<String>,
}

impl Drop for RunTraceIdGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = RUN_TRACE_ID.lock() {
            *guard = self.previous.take();
        }
    }
}

/// Arm `id` as the active run trace for the guard's lifetime.
pub(crate) fn scoped_run_trace_id(id: String) -> RunTraceIdGuard {
    let previous = RUN_TRACE_ID.lock().ok().and_then(|mut guard| guard.replace(id));
    RunTraceIdGuard { previous }
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
pub(crate) fn process_wp_response(
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
    device_id: &str,
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

    // GAP-06 收尾: run-scoped trace correlation (best-effort diagnostic,
    // not part of the signed set — same class as X-Request-Id).
    if let Some(trace_id) = current_run_trace_id() {
        request = request.header(TRACE_ID_HEADER, trace_id);
    }

    // opus5 A-03 (AF-03): the device identity is threaded explicitly from the
    // boot-level WorkerConfig/AppState identity — never re-read from env
    // here, so a binding's requests always carry the device id its token was
    // paired with on the WP side.
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
    device_id: &str,
) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    wp_get_json_with_transport_and_secret(client, url, token, worker_id, device_id, None).await
}

/// Transport-aware typed GET request with optional route_secret.
pub(crate) async fn wp_get_json_with_transport_and_secret<T>(
    client: &Client,
    url: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
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
        device_id,
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
    device_id: &str,
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

    // GAP-06 收尾: run-scoped trace correlation (see wp_request_with_transport).
    if let Some(trace_id) = current_run_trace_id() {
        request = request.header(TRACE_ID_HEADER, trace_id);
    }

    // opus5 A-03 (AF-03): device identity threaded from boot-level state;
    // no env read at request time (see wp_request_with_transport).
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
