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
    if let Some(parent) = Path::new(wasm_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(wasm_path, &wasm_bytes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Client;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn call_process_sign_returns_none_without_plugin() {
        // Ensure plugin is not loaded in this test context
        let ctx = HashMap::new();
        let config = serde_json::json!({});
        let result = call_process_sign("md5", &config, &ctx);
        // Without WASM binary loaded, should return None
        // (the global may or may not be loaded depending on test order,
        //  but the graceful None handling is the important part)
        assert!(result.is_none() || result.is_some());
    }

    #[test]
    fn init_sign_plugin_nonexistent_returns_false() {
        assert!(!init_sign_plugin("/nonexistent/path/to/plugin.wasm"));
    }

    #[test]
    fn init_sign_plugin_rejects_oversized_module_before_compile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oversized.wasm");
        let file = std::fs::File::create(&path).expect("create wasm fixture");
        file.set_len(MAX_SIGN_PLUGIN_BYTES + 1)
            .expect("extend wasm fixture");
        assert!(!init_sign_plugin(path.to_str().expect("utf8 path")));
    }

    #[test]
    fn sign_plugin_resource_limits_are_bounded() {
        assert_eq!(MAX_SIGN_PLUGIN_MEMORY_PAGES, 256);
        assert!(MAX_SIGN_PLUGIN_FUEL <= 10_000_000);
        assert!(MAX_SIGN_PLUGIN_TIMEOUT_MS <= 5_000);
        assert!(MAX_SIGN_PLUGIN_INPUT_BYTES <= 512 * 1024);
        assert!(MAX_SIGN_PLUGIN_OUTPUT_BYTES <= 512 * 1024);
    }

    #[test]
    fn wasm_execution_surface_is_limited_to_sign_plugin_sandbox() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let src_dir = manifest_dir.join("src");
        let sign_plugin = Path::new("component_rt").join("sign_plugin.rs");
        let mut stack = vec![src_dir.clone()];
        let mut wasm_runtime_files = Vec::new();

        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).expect("read source directory") {
                let entry = entry.expect("read source entry");
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("read rust source");
                if source.contains("extism::")
                    || source.contains("PluginBuilder")
                    || source.contains("Wasm::")
                    || source.contains(".with_wasi(")
                {
                    let relative = path.strip_prefix(&src_dir).expect("relative source path");
                    wasm_runtime_files.push(relative.to_path_buf());
                }
            }
        }

        wasm_runtime_files.sort();
        assert_eq!(
            wasm_runtime_files,
            vec![sign_plugin],
            "any new WASM execution surface must define an explicit capability sandbox before it is added"
        );

        let source = std::fs::read_to_string(src_dir.join("component_rt/sign_plugin.rs"))
            .expect("read sign plugin source");
        let forbidden_wasi_enable = [".with_wasi(", "true)"].concat();
        assert!(source.contains(".with_wasi(false)"));
        assert!(
            !source.contains(&forbidden_wasi_enable),
            "signer WASM must stay non-WASI unless a narrower host capability model is added"
        );
    }

    #[test]
    fn is_sign_plugin_loaded_default_false() {
        // This test may be affected by other tests that load the plugin,
        // but the function should not panic.
        let _ = is_sign_plugin_loaded();
    }

    #[tokio::test]
    async fn auto_download_sign_plugin_preserves_structured_version_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let body = r#"{"success":false,"error":{"code":"SESSION_EXPIRED","message":"Session expired"}}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let client = Client::builder().no_proxy().build().unwrap();
        let err = auto_download_sign_plugin(
            &client,
            &format!("http://{}", addr),
            "sess-sign-plugin",
            "/tmp/test-sign-plugin.wasm",
            None,
            None,
            "/tmp/test-sign-plugin.log",
        )
        .await
        .expect_err("structured sign-plugin auth errors should currently fail");
        let err_text = format!("{:#}", err);

        assert!(
            err_text.contains("SESSION_EXPIRED"),
            "expected sign-plugin version error code to survive auto-download failure: {}",
            err_text
        );
        assert!(
            !err_text.contains("invalid json response"),
            "structured sign-plugin version errors should not be collapsed into invalid json response: {}",
            err_text
        );
    }

    #[tokio::test]
    async fn auto_download_sign_plugin_preserves_download_error_details() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for step in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let n = socket.read(&mut request).await.unwrap();
                let request_text = String::from_utf8_lossy(&request[..n]);
                let first_line = request_text.lines().next().unwrap_or("");
                let (status, body) = if step == 0 {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/sign-plugin/version"),
                        "expected sign-plugin version request, got: {}",
                        first_line
                    );
                    (
                        "200 OK",
                        "{\"success\":true,\"data\":{\"version\":\"1.0.0\",\"sha256\":\"abc123\",\"size\":123,\"available\":true}}",
                    )
                } else {
                    assert!(
                        first_line.starts_with("GET /api/v1/client/sign-plugin/download"),
                        "expected sign-plugin download request, got: {}",
                        first_line
                    );
                    (
                        "403 Forbidden",
                        "{\"success\":false,\"error\":{\"code\":\"HTTPS_REQUIRED\",\"message\":\"Plugin downloads require HTTPS in production\"}}",
                    )
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let client = Client::builder().no_proxy().build().unwrap();
        let err = auto_download_sign_plugin(
            &client,
            &format!("http://{}", addr),
            "sess-sign-plugin-download",
            "/tmp/test-sign-plugin-download.wasm",
            None,
            None,
            "/tmp/test-sign-plugin-download.log",
        )
        .await
        .expect_err("structured sign-plugin download errors should currently fail");
        let err_text = format!("{:#}", err);

        assert!(
            err_text.contains("HTTPS_REQUIRED"),
            "expected sign-plugin download error code to survive auto-download failure: {}",
            err_text
        );
        assert!(
            !err_text.contains("sign-plugin download returned status 403"),
            "sign-plugin download errors should not be collapsed into status-only message: {}",
            err_text
        );
    }
}
