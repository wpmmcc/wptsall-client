use anyhow::{anyhow, Context};
use extism::{Manifest, Plugin, PluginBuilder, Wasm};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crate::auth::{
    build_request_id, http_status_line, parse_api_error_response, request_json, UpstreamApiError,
};
use crate::config::REQUEST_ID_HEADER;
use crate::crypto::verify_component_signature;
use crate::logging::log_event;
use crate::types::ApiResponse;

/// Mirror of sign-plugin SignInput (kept local to avoid cross-crate dep).
#[derive(Serialize)]
struct SignInput {
    algorithm: String,
    config: Value,
    context: HashMap<String, String>,
}

/// Mirror of sign-plugin SignOutput.
#[derive(Deserialize, Debug)]
pub(crate) struct SignPluginOutput {
    pub(crate) success: bool,
    pub(crate) result_type: String,
    pub(crate) computed: HashMap<String, String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) authorization: Option<String>,
    pub(crate) error: Option<String>,
}

static SIGN_PLUGIN: Mutex<Option<Plugin>> = Mutex::new(None);

/// Default path for the sign plugin WASM file.
pub(crate) const DEFAULT_SIGN_PLUGIN_PATH: &str = "./plugins/wptsall-sign.wasm";

/// Hard upper bound for the optional signing module.  The sign plugin only
/// implements deterministic request-signature algorithms; accepting an
/// arbitrarily large Wasm payload would turn a configuration/update endpoint
/// into a memory/compile exhaustion vector.
pub(crate) const MAX_SIGN_PLUGIN_BYTES: u64 = 8 * 1024 * 1024;

/// Runtime resource ceilings for the optional signer.  The signer is kept
/// deliberately small and deterministic: it receives one JSON request and
/// must return one JSON response.  These limits make a malicious-but-valid
/// module fail closed instead of consuming an unbounded amount of CPU or
/// linear memory.
pub(crate) const MAX_SIGN_PLUGIN_MEMORY_PAGES: u32 = 256; // 16 MiB (64 KiB pages)
pub(crate) const MAX_SIGN_PLUGIN_FUEL: u64 = 5_000_000;
pub(crate) const MAX_SIGN_PLUGIN_TIMEOUT_MS: u64 = 2_000;
pub(crate) const MAX_SIGN_PLUGIN_INPUT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SIGN_PLUGIN_OUTPUT_BYTES: usize = 256 * 1024;

/// Try to load the WASM sign plugin from the given path.
/// Returns `true` if the plugin was loaded successfully, `false` otherwise.
/// If the file does not exist, silently returns `false`.
pub(crate) fn init_sign_plugin(wasm_path: &str) -> bool {
    let path = Path::new(wasm_path);
    if !path.exists() {
        return false;
    }
    if std::fs::metadata(path)
        .map(|metadata| metadata.len() > MAX_SIGN_PLUGIN_BYTES)
        .unwrap_or(true)
    {
        return false;
    }
    let wasm_bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let mut manifest =
        Manifest::new([Wasm::data(wasm_bytes)]).with_memory_max(MAX_SIGN_PLUGIN_MEMORY_PAGES);
    manifest.timeout_ms = Some(MAX_SIGN_PLUGIN_TIMEOUT_MS);
    // The signer is a pure function over the supplied JSON input.  Do not
    // enable WASI: this prevents a downloaded/signing module from reading the
    // host filesystem, environment, clocks or opening sockets.  Any future
    // capability must be introduced as an explicit, narrowly scoped host
    // function rather than by turning WASI back on globally.
    let plugin = match PluginBuilder::new(&manifest)
        .with_wasi(false)
        .with_fuel_limit(MAX_SIGN_PLUGIN_FUEL)
        .build()
    {
        Ok(p) => p,
        Err(_) => return false,
    };
    if let Ok(mut guard) = SIGN_PLUGIN.lock() {
        *guard = Some(plugin);
        true
    } else {
        false
    }
}

/// Call the WASM sign plugin's `process_sign` function.
/// Returns `None` if the plugin is not loaded or if the call fails.
pub(crate) fn call_process_sign(
    algorithm: &str,
    config: &Value,
    context: &HashMap<String, String>,
) -> Option<SignPluginOutput> {
    let mut guard = SIGN_PLUGIN.lock().ok()?;
    let plugin = guard.as_mut()?;

    let input = SignInput {
        algorithm: algorithm.to_string(),
        config: config.clone(),
        context: context.clone(),
    };
    let input_json = serde_json::to_vec(&input).ok()?;
    if input_json.len() > MAX_SIGN_PLUGIN_INPUT_BYTES {
        return None;
    }

    let output_bytes = plugin
        .call::<&[u8], Vec<u8>>("process_sign", &input_json)
        .ok()?;
    if output_bytes.len() > MAX_SIGN_PLUGIN_OUTPUT_BYTES {
        // A signer response is a small JSON envelope.  Reject an oversized
        // guest allocation before deserializing it or exposing it to the
        // request pipeline.
        return None;
    }
    serde_json::from_slice::<SignPluginOutput>(&output_bytes).ok()
}

/// Check whether the WASM sign plugin is loaded.
pub(crate) fn is_sign_plugin_loaded() -> bool {
    SIGN_PLUGIN
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Auto-download from server
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
struct SignPluginVersionData {
    version: String,
    sha256: String,
    #[allow(dead_code)]
    size: u64,
    available: bool,
}

/// Check server for sign-plugin updates and download if needed.
///
/// Returns `Ok(true)` if a new plugin was downloaded, `Ok(false)` if local
/// is already up-to-date or server has no plugin, `Err` on failure.
pub(crate) async fn auto_download_sign_plugin(
    client: &Client,
    server_base: &str,
    session_token: &str,
    wasm_path: &str,
    trusted_public_key: Option<&str>,
    trusted_signing_key_id: Option<&str>,
    log_file: &str,
) -> anyhow::Result<bool> {
    // 1. Query server for version info
    let version_url = format!("{}/api/v1/client/sign-plugin/version", server_base);
    let version_resp: ApiResponse<SignPluginVersionData> = request_json(
        client
            .get(&version_url)
            .header("X-Client-Session", session_token)
            .header(REQUEST_ID_HEADER, build_request_id("sign-plugin-version")),
        "sign-plugin version",
    )
    .await?;

    if !version_resp.success || !version_resp.data.available {
        let _ = log_event(
            log_file,
            "info",
            "sign_plugin.server_not_available",
            json!({ "version": version_resp.data.version }),
        );
        return Ok(false);
    }

    let server_sha256 = version_resp.data.sha256.to_lowercase();
    let server_version = &version_resp.data.version;

    // 2. Check if local file already matches
    if let Ok(local_bytes) = std::fs::read(wasm_path) {
        let local_hash = sha256_hex(&local_bytes);
        if local_hash == server_sha256 {
            let _ = log_event(
                log_file,
                "info",
                "sign_plugin.up_to_date",
                json!({
                    "version": server_version,
                    "sha256": server_sha256,
                    "path": wasm_path
                }),
            );
            return Ok(false);
        }
        let _ = log_event(
            log_file,
            "info",
            "sign_plugin.hash_mismatch",
            json!({
                "local_sha256": local_hash,
                "server_sha256": server_sha256
            }),
        );
    }

    // 3. Download from server
    let download_url = format!("{}/api/v1/client/sign-plugin/download", server_base);
    let response = client
        .get(&download_url)
        .header("X-Client-Session", session_token)
        .header(REQUEST_ID_HEADER, build_request_id("sign-plugin-download"))
        .send()
        .await
        .with_context(|| "sign-plugin download request failed")?;

    if let Some(content_length) = response.content_length() {
        if content_length > MAX_SIGN_PLUGIN_BYTES {
            return Err(anyhow!(
                "sign-plugin download exceeds {} byte limit",
                MAX_SIGN_PLUGIN_BYTES
            ));
        }
    }

    if !response.status().is_success() {
        let status = response.status();
        let url = response.url().to_string();
        let body = response
            .text()
            .await
            .with_context(|| "sign-plugin download read error body failed")?;
        if let Some(api_error) = parse_api_error_response(&body) {
            return Err(UpstreamApiError {
                status: http_status_line(status),
                code: api_error.error.code,
                message: api_error.error.message,
            }
            .into());
        }
        let snippet = if body.len() > 220 {
            format!("{}...", &body[..body.floor_char_boundary(220)])
        } else {
            body
        };
        return Err(anyhow!(
            "sign-plugin download returned status {} (url={}, body={})",
            status,
            url,
            snippet
        ));
    }

    let resp_sha256 = response
        .headers()
        .get("X-Plugin-SHA256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let resp_signature = response
        .headers()
        .get("X-Plugin-Signature")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let resp_signing_key_id = response
        .headers()
        .get("X-Plugin-Signing-Key-Id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);

    let wasm_bytes = response
        .bytes()
        .await
        .with_context(|| "sign-plugin download read body failed")?;

    if wasm_bytes.len() as u64 > MAX_SIGN_PLUGIN_BYTES {
        return Err(anyhow!(
            "sign-plugin download exceeds {} byte limit",
            MAX_SIGN_PLUGIN_BYTES
        ));
    }

    // 4. Verify SHA-256
    let downloaded_hash = sha256_hex(&wasm_bytes);
    if downloaded_hash != server_sha256 {
        return Err(anyhow!(
            "sign-plugin SHA-256 mismatch: expected {}, got {}",
            server_sha256,
            downloaded_hash
        ));
    }
    // Also verify against response header if present
    if !resp_sha256.is_empty() && resp_sha256 != downloaded_hash {
        return Err(anyhow!(
            "sign-plugin SHA-256 header mismatch: header {}, computed {}",
            resp_sha256,
            downloaded_hash
        ));
    }

    if let (Some(local_id), Some(server_id)) = (
        trusted_signing_key_id
            .map(str::trim)
            .filter(|v| !v.is_empty()),
        resp_signing_key_id.as_deref(),
    ) {
        if local_id != server_id {
            return Err(anyhow!(
                "sign-plugin signing key mismatch: local {}, server {}",
                local_id,
                server_id
            ));
        }
    }

    // 5. Verify RSA signature
    let skip_sig_check = std::env::var("WPTSALL_SKIP_SIGNATURE_CHECK")
        .ok()
        .as_deref()
        == Some("true");
    match (resp_signature.as_deref(), trusted_public_key) {
        (Some(sig), Some(pub_pem)) => {
            verify_component_signature(&wasm_bytes, sig, pub_pem)
                .with_context(|| "sign-plugin RSA signature verification failed")?;
        }
        (Some(_sig), None) if skip_sig_check => {
            let _ = log_event(
                log_file,
                "warn",
                "sign_plugin.signature_skip",
                json!({ "note": "WPTSALL_SKIP_SIGNATURE_CHECK=true" }),
            );
        }
        (Some(_sig), None) => {
            return Err(anyhow!(
                "sign-plugin has signature but no trusted public key configured"
            ));
        }
        (None, _) if skip_sig_check => {
            let _ = log_event(
                log_file,
                "warn",
                "sign_plugin.no_signature",
                json!({ "note": "server did not provide signature, skipping check" }),
            );
        }
        (None, _) => {
            return Err(anyhow!(
                "sign-plugin download has no X-Plugin-Signature header"
            ));
        }
    }

    // 6. Save to local path
    crate::bindings::atomic_file::install(Path::new(wasm_path), &wasm_bytes)
        .with_context(|| format!("failed to write sign-plugin to {}", wasm_path))?;

    let _ = log_event(
        log_file,
        "info",
        "sign_plugin.downloaded",
        json!({
            "version": server_version,
            "sha256": server_sha256,
            "size": wasm_bytes.len(),
            "path": wasm_path
        }),
    );

    Ok(true)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}
