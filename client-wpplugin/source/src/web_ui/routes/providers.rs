//! POST /api/providers/test — one-click vendor/API-key connectivity probe
//! without requiring a local component or slot binding.

use anyhow::Context;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::bindings::load_vendor_keys;
use crate::types::WebUiState;

use super::components::generate_openai_compatible_template;
use super::errors::write_error_response;
use super::http::write_http_response;
use super::vendor_keys_path;

/// POST /api/providers/test
pub(super) async fn handle_providers_test(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return write_error_response(
                socket,
                "INVALID_JSON",
                "expected JSON body for provider test",
            )
            .await;
        }
    };

    let vendor_id = req
        .get("vendor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let key_id = req
        .get("key_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let text = req
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Hello")
        .to_string();
    let source_lang = req
        .get("source_lang")
        .and_then(|v| v.as_str())
        .unwrap_or("en_US")
        .to_string();
    let target_lang = req
        .get("target_lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh_CN")
        .to_string();
    let base_url = req
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.openai.com")
        .to_string();
    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("gpt-4o-mini")
        .to_string();

    let mut auth_values: HashMap<String, String> = HashMap::new();
    if let Some(obj) = req.get("auth_values").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            let name = k.trim().trim_start_matches("auth.");
            if name.is_empty() {
                continue;
            }
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    auth_values.insert(format!("auth.{name}"), s.trim().to_string());
                }
            }
        }
    }
    if auth_values.is_empty() {
        if let Some(kid) = key_id.as_ref() {
            let doc = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
            let Some(key) = doc.keys.get(kid.as_str()) else {
                return write_error_response(
                    socket,
                    "KEY_NOT_FOUND",
                    &format!("vendor key '{kid}' not found"),
                )
                .await;
            };
            for (k, v) in &key.auth_values {
                let name = k.trim().trim_start_matches("auth.");
                if !name.is_empty() && !v.trim().is_empty() {
                    auth_values.insert(format!("auth.{name}"), v.trim().to_string());
                }
            }
        }
    }
    if auth_values.is_empty() {
        return write_error_response(
            socket,
            "INVALID_AUTH",
            "provide auth_values or key_id with stored secrets",
        )
        .await;
    }

    let key_vendor = key_id.as_ref().and_then(|kid| {
        load_vendor_keys(&vendor_keys_path())
            .ok()
            .and_then(|doc| doc.keys.get(kid.as_str()).map(|k| k.vendor_id.clone()))
    });
    let resolved_vendor = match resolve_probe_vendor(&vendor_id, key_vendor.as_deref()) {
        Ok(vendor) => vendor,
        Err(_) => {
            // Y-6 (tasks/5.3falsh2/12 批 B): the probe report's vendor field
            // is the attribution anchor for catalog logging — an empty
            // vendor_id with no reverse-lookup key must fail closed
            // instead of silently attributing the probe to "unknown".
            return write_error_response(
                socket,
                "VENDOR_UNRESOLVED",
                "vendor_id is empty and key_id has no stored vendor key; provide vendor_id or a valid key_id so the probe report keeps a real vendor anchor",
            )
            .await;
        }
    };

    let tpl_json =
        generate_openai_compatible_template(&base_url, &model, None, None, Some(64), None);
    let start = std::time::Instant::now();
    let result = async {
        let mut template: crate::types::ComponentTemplate = serde_json::from_value(tpl_json)
            .with_context(|| "invalid generated openai-compatible template")?;
        crate::component_rt::loader::apply_local_component_kind_to_template(&mut template, "text");
        let runtime = crate::types::ComponentRuntime {
            language_map: template
                .constraints
                .as_ref()
                .map(|c| c.language_map.clone())
                .unwrap_or_default(),
            template,
            auth_values,
            supported_business_lines: vec![],
            supported_content_formats: vec![],
            supported_formats: vec![],
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
            proxy_profile_id: None,
        };
        let api_client = Client::builder().timeout(Duration::from_secs(60)).build()?;
        let translated = crate::component_rt::runner::translate_text_via_component(
            &api_client,
            &runtime,
            &text,
            &source_lang,
            &target_lang,
        )
        .await?;
        Ok::<String, anyhow::Error>(translated)
    }
    .await;

    let latency_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(translated_text) => {
            crate::logging::log_event_global(
                "info",
                "providers.tested",
                json!({ "vendor_id": resolved_vendor, "ok": true, "latency_ms": latency_ms }),
            );
            let payload = json!({
                "success": true,
                "data": {
                    "healthy": true,
                    "latency_ms": latency_ms,
                    "translated_text": translated_text,
                    "model_resolved": model,
                    "vendor_id": resolved_vendor,
                    "base_url_used": base_url,
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
        Err(err) => {
            let message = format!("{:#}", err);
            let (code, hint) = classify_provider_test_error(&message);
            crate::logging::log_event_global(
                "warn",
                "providers.tested",
                json!({ "vendor_id": resolved_vendor, "ok": false, "code": code, "latency_ms": latency_ms }),
            );
            let payload = json!({
                "success": false,
                "error": { "code": code, "message": message, "hint": hint },
                "data": {
                    "healthy": false,
                    "latency_ms": latency_ms,
                    "vendor_id": resolved_vendor,
                    "model_resolved": model,
                    "base_url_used": base_url,
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
    }
}

/// Y-6 (tasks/5.3falsh2/12 批 B) decision core: the provider probe's
/// vendor attribution must have an anchor.
/// - vendor_id non-empty → passes through as given (existing contract).
/// - vendor_id empty + key_id reverse-lookup hit → the stored key's
///   vendor_id.
/// - vendor_id empty + no reverse-lookup → Err: the probe used to fall
///   back to the literal "unknown", letting fabricated vendors run the
///   test channel with zero catalog attribution. It now fails closed.
fn resolve_probe_vendor(vendor_id: &str, key_vendor: Option<&str>) -> Result<String, ()> {
    let vendor = vendor_id.trim();
    if !vendor.is_empty() {
        return Ok(vendor.to_string());
    }
    key_vendor
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or(())
}

fn classify_provider_test_error(message: &str) -> (&'static str, &'static str) {
    let lower = message.to_ascii_lowercase();
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("authentication")
    {
        return (
            "AUTH_REJECTED",
            "Check the API key / auth_values and vendor account access",
        );
    }
    if lower.contains("ssrf") || lower.contains("blocked") || lower.contains("not allowlisted") {
        return (
            "PROVIDER_URL_BLOCKED",
            "Provider URL is blocked by the local allowlist / SSRF guard",
        );
    }
    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("network")
    {
        return (
            "NETWORK_FAILED",
            "Network error reaching the provider — check base_url, proxy, and outbound access",
        );
    }
    (
        "PROVIDER_TEST_FAILED",
        "Provider probe failed — see error message for details",
    )
}
#[cfg(test)]
mod tests {
    use super::{classify_provider_test_error, resolve_probe_vendor};

    #[test]
    fn classifies_auth_network_and_ssrf_errors() {
        assert_eq!(
            classify_provider_test_error("401 unauthorized").0,
            "AUTH_REJECTED"
        );
        assert_eq!(
            classify_provider_test_error("connection timed out").0,
            "NETWORK_FAILED"
        );
        assert_eq!(
            classify_provider_test_error("URL blocked by SSRF guard").0,
            "PROVIDER_URL_BLOCKED"
        );
        assert_eq!(
            classify_provider_test_error("model not found").0,
            "PROVIDER_TEST_FAILED"
        );
    }

    /// Y-6: no more "unknown" fallback — the probe's vendor attribution
    /// fails closed when neither vendor_id nor a reverse-lookup key
    /// provides an anchor.
    #[test]
    fn resolve_probe_vendor_requires_an_anchor() {
        // Non-empty vendor_id passes through (trimmed).
        assert_eq!(
            resolve_probe_vendor(" openai ", None).as_deref(),
            Ok("openai")
        );
        // Empty vendor_id + reverse-lookup hit resolves to the key's vendor.
        assert_eq!(
            resolve_probe_vendor("", Some("deepseek")).as_deref(),
            Ok("deepseek")
        );
        // Empty vendor_id + no reverse-lookup fails closed — never "unknown".
        assert_eq!(resolve_probe_vendor("", None), Err(()));
        // Whitespace-only or empty stored vendor on the key is no anchor.
        assert_eq!(resolve_probe_vendor("", Some("   ")), Err(()));
        assert_eq!(resolve_probe_vendor("", Some("")), Err(()));
    }
}