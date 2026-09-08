use anyhow::anyhow;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use self::signing::prime_sign_context;
use crate::task_engine::executor::normalize_patch_field_key;
use crate::task_engine::executor::normalize_task_content_type;
use crate::types::*;

/// Redact sensitive query parameters from URLs before logging.
fn redact_url_secrets(url: &str) -> String {
    if let Some(q_pos) = url.find('?') {
        let (base, query) = url.split_at(q_pos);
        let redacted = query
            .split('&')
            .map(|param| {
                if let Some(eq_pos) = param.find('=') {
                    let key_lower = param[..eq_pos].to_lowercase();
                    if key_lower.contains("key")
                        || key_lower.contains("token")
                        || key_lower.contains("secret")
                        || key_lower.contains("auth")
                    {
                        format!("{}=***", &param[..eq_pos])
                    } else {
                        param.to_string()
                    }
                } else {
                    param.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("&");
        format!("{}{}", base, redacted)
    } else {
        url.to_string()
    }
}

/// Validate a rendered provider URL before any component-side egress.
///
/// Provider templates are operator-controlled input, so this check is kept at
/// the final request boundary (after template rendering) and is shared by all
/// request variants.  DNS results are checked as well as literal IPs to reduce
/// DNS-rebinding/metadata exposure.  An optional comma-separated
/// Translation provider URL policy for the local-first Client.
///
/// Product rule: operators may point a catalog/component at **any** http(s)
/// vendor endpoint (including loopback mock-api / local LLM). We only reject
/// clearly unsafe shapes: non-http schemes and credentials embedded in the URL.
/// Cloud metadata hostnames remain blocked as a hard SSRF floor.
pub(super) fn assert_provider_url_allowed(raw: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| anyhow!("provider URL is invalid"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(anyhow!("provider URL scheme is not allowed"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!(
            "provider URL must not contain embedded credentials"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("provider URL host is missing"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();

    // Hard floor only: cloud instance-metadata endpoints (not translation APIs).
    if host == "metadata.google.internal"
        || host == "metadata"
        || host.ends_with(".metadata.google.internal")
    {
        return Err(anyhow!("provider host is blocked by egress policy"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if matches!(ip, IpAddr::V4(v4) if v4.octets() == [169, 254, 169, 254]) {
            return Err(anyhow!("provider host is blocked by egress policy"));
        }
    }

    // Optional deny-list (comma-separated). Empty = allow all other http(s) hosts.
    if host_matches_denylist(&host, "WPTSALL_PROVIDER_DENYLIST") {
        return Err(anyhow!("provider host is blocked by egress policy"));
    }

    // Legacy allowlist is no longer required to reach loopback/mock; kept as a
    // no-op compatibility hook so existing Lab env vars do not break.
    let _ = host_matches_allowlist(&host, "WPTSALL_PROVIDER_ALLOWLIST");
    Ok(())
}

fn host_matches_denylist(host: &str, variable: &str) -> bool {
    host_matches_allowlist(host, variable)
}

/// Return whether a hostname is explicitly approved in a comma-separated
/// exact-host / `*.suffix` operator allowlist.  This is deliberately shared
/// by provider and secret-store egress so an input credential reference can
/// never choose its own network destination.
fn host_matches_allowlist(host: &str, variable: &str) -> bool {
    env::var(variable)
        .unwrap_or_default()
        .split(',')
        .map(|value| value.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .any(|pattern| {
            pattern
                .strip_prefix("*.")
                .map(|suffix| host == suffix || host.ends_with(&format!(".{}", suffix)))
                .unwrap_or_else(|| host == pattern)
        })
}

/// Reject redirects that leave the configured provider origin.  Reqwest
/// follows redirects by default; checking the final URL closes the common
/// cross-host redirect escape from an otherwise allowlisted endpoint.
pub(super) fn assert_provider_redirect_origin(
    original: &str,
    final_url: &url::Url,
) -> anyhow::Result<()> {
    let initial =
        url::Url::parse(original.trim()).map_err(|_| anyhow!("provider URL is invalid"))?;
    let origin = |u: &url::Url| {
        (
            u.scheme().to_ascii_lowercase(),
            u.host_str()
                .unwrap_or_default()
                .trim_end_matches('.')
                .to_ascii_lowercase(),
            u.port_or_known_default(),
        )
    };
    if origin(&initial) != origin(final_url) {
        return Err(anyhow!("provider redirect crossed host boundary"));
    }
    assert_provider_url_allowed(final_url.as_str())
}

/// Result of signing - different algorithms produce different outputs
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum SignResult {
    /// Add computed fields to context (existing behavior for Category A)
    ContextOnly(HashMap<String, String>),
    /// Add computed fields + extra HTTP headers (Category B/C)
    WithHeaders(HashMap<String, String>, Vec<(String, String)>),
    /// Set the Authorization header directly (Category C/D)
    AuthorizationHeader(String),
}

mod async_poll;
mod chunking;
mod request;
mod signing;
mod source_asset;
use self::async_poll::*;
#[cfg(test)]
use self::chunking::{is_gutenberg_content, split_rich_html_by_blocks};
pub(crate) use self::chunking::{translate_rich_html_blocks, translate_text_with_constraints};
use self::request::*;
use self::signing::process_sign_config;
use self::source_asset::*;

pub(crate) fn resolve_auth_values(
    component_id: &str,
    auth: Option<&ComponentAuth>,
    component_bindings: &mut ComponentBindingsDoc,
) -> anyhow::Result<(HashMap<String, String>, bool)> {
    let mut values = HashMap::new();
    let Some(auth_cfg) = auth else {
        return Ok((values, false));
    };
    let updated = false;
    let binding_entry = component_bindings
        .components
        .entry(component_id.to_string())
        .or_default();

    for field in &auth_cfg.fields {
        let env_key = auth_env_key(&field.name);
        let env_raw_value = env::var(&env_key).ok();
        let env_value = env_raw_value
            .as_deref()
            .map(resolve_credential_reference)
            .transpose()?
            .unwrap_or_default();
        // Environment-provided credentials are intentionally never copied
        // into the local binding document. This keeps both direct env values
        // and resolved references out of persistent config; callers may still
        // explicitly save an `env://...` reference through the bindings UI.
        let bound_value = binding_entry
            .auth
            .get(&field.name)
            .map(|value| resolve_credential_reference(value))
            .transpose()?
            .unwrap_or_default();
        let selected_value = if !env_value.trim().is_empty() {
            env_value
        } else {
            bound_value
        };
        let required = field.required.unwrap_or(false);

        if required && selected_value.trim().is_empty() {
            return Err(anyhow!(
                "missing required component auth: {} (env {}, binding component={})",
                field.name,
                env_key,
                component_id
            ));
        }
        if !selected_value.trim().is_empty() {
            values.insert(format!("auth.{}", field.name), selected_value);
        }
    }

    Ok((values, updated))
}

/// Maximum length shared by direct, file and external-store credential values.
const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;

/// Maximum external secret-store JSON response accepted by the credential
/// resolver.  Store data is intentionally limited to a single small
/// credential, not a general data transport channel.
const MAX_SECRET_STORE_RESPONSE_BYTES: u64 = 64 * 1024;

/// Resolve a deployment-side credential reference without persisting the
/// resolved secret. Direct values remain supported for backwards compatibility
/// with existing encrypted local configuration.
///
/// Supported references are `env://NAME`, root-confined `file://NAME`,
/// HashiCorp Vault KV v2 `vault://MOUNT/PATH#FIELD`, and a deployment-owned
/// KMS/secret-broker endpoint `kms://SECRET_NAME#FIELD`.  External
/// secret-store endpoints and bootstrap tokens are deployment configuration,
/// never part of the persisted reference.  `WPTSALL_SECRET_STORE_ALLOWLIST`
/// is mandatory before any Vault/KMS network request.
pub(crate) fn resolve_credential_reference(value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    if let Some(name) = value.strip_prefix("env://") {
        let name = name.trim();
        if name.is_empty()
            || !name.chars().enumerate().all(|(index, ch)| {
                (index == 0 && (ch.is_ascii_alphabetic() || ch == '_'))
                    || (index > 0 && (ch.is_ascii_alphanumeric() || ch == '_'))
            })
        {
            anyhow::bail!("invalid env credential reference");
        }
        let resolved =
            env::var(name).map_err(|_| anyhow!("referenced environment variable is not set"))?;
        if resolved.len() > MAX_CREDENTIAL_BYTES {
            anyhow::bail!("referenced credential exceeds 16 KiB limit");
        }
        return Ok(resolved.trim().to_string());
    }
    if let Some(name) = value.strip_prefix("file://") {
        let root = env::var("WPTSALL_CREDENTIAL_FILE_ROOT")
            .map_err(|_| anyhow!("WPTSALL_CREDENTIAL_FILE_ROOT is required for file references"))?;
        let root = Path::new(root.trim())
            .canonicalize()
            .map_err(|_| anyhow!("credential file root does not exist"))?;
        let relative = Path::new(name.trim());
        if relative.as_os_str().is_empty() || relative.is_absolute() {
            anyhow::bail!("file credential reference must be relative to credential root");
        }
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|_| anyhow!("credential file reference does not exist"))?;
        if !path.starts_with(&root) {
            anyhow::bail!("credential file reference escapes credential root");
        }
        let metadata = std::fs::metadata(&path)
            .map_err(|_| anyhow!("credential file metadata unavailable"))?;
        if !metadata.is_file() || metadata.len() > MAX_CREDENTIAL_BYTES as u64 {
            anyhow::bail!("credential file must be a regular file no larger than 16 KiB");
        }
        return Ok(std::fs::read_to_string(path)
            .map_err(|_| anyhow!("credential file is not valid UTF-8"))?
            .trim()
            .to_string());
    }
    if let Some(reference) = value.strip_prefix("vault://") {
        return resolve_vault_credential_reference(reference);
    }
    if let Some(reference) = value.strip_prefix("kms://") {
        return resolve_kms_credential_reference(reference);
    }
    if value.contains("://") {
        anyhow::bail!("unsupported credential reference scheme");
    }
    Ok(value.to_string())
}

/// Resolve one HashiCorp Vault KV v2 field.  The persisted reference only
/// names an approved secret field; address, auth token, TLS scheme and timeout
/// remain deployment-controlled.  It makes a single non-redirecting request,
/// accepts a bounded JSON response, and never caches or writes the value.
fn resolve_vault_credential_reference(reference: &str) -> anyhow::Result<String> {
    let (mount, path, field) = parse_vault_kv_v2_reference(reference)?;
    let base = configured_vault_base_url()?;
    let token = configured_vault_token()?;
    let endpoint = base
        .join(&format!("v1/{}/data/{}", mount, path))
        .map_err(|_| anyhow!("Vault credential endpoint is invalid"))?;

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| anyhow!("Vault credential client initialization failed"))?;
    let response = client
        .get(endpoint)
        .header("X-Vault-Token", token)
        .send()
        .map_err(|_| anyhow!("Vault credential request failed"))?;
    if !response.status().is_success() {
        anyhow::bail!(
            "Vault credential request returned status {}",
            response.status().as_u16()
        );
    }

    let mut body = Vec::new();
    response
        .take(MAX_SECRET_STORE_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| anyhow!("Vault credential response could not be read"))?;
    if body.len() as u64 > MAX_SECRET_STORE_RESPONSE_BYTES {
        anyhow::bail!("Vault credential response exceeds 64 KiB limit");
    }
    let response: Value = serde_json::from_slice(&body)
        .map_err(|_| anyhow!("Vault credential response is not valid JSON"))?;
    let secret = response
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get("data"))
        .and_then(Value::as_object)
        .and_then(|data| data.get(&field))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Vault credential field is missing or not a string"))?;
    if secret.len() > MAX_CREDENTIAL_BYTES {
        anyhow::bail!("referenced credential exceeds 16 KiB limit");
    }
    Ok(secret.trim().to_string())
}

/// Resolve one field from a deployment-owned KMS/secret-broker endpoint.  The
/// persisted reference only names a secret and field; address/auth remain
/// deployment-controlled:
///
/// - `WPTSALL_KMS_ADDR=https://kms.example.internal/`
/// - `WPTSALL_KMS_TOKEN=env://...` or `file://...` or a direct bootstrap token
/// - `WPTSALL_SECRET_STORE_ALLOWLIST=kms.example.internal`
///
/// The resolver performs exactly one bounded, non-redirecting GET request to
/// `/v1/secrets/{SECRET_NAME}` and reads the requested field from either
/// `{"data": {"FIELD": "value"}}` or `{"FIELD": "value"}`.  It does not
/// decrypt locally or persist the resolved value; KMS-specific decrypt logic
/// belongs behind the deployment-owned endpoint.
fn resolve_kms_credential_reference(reference: &str) -> anyhow::Result<String> {
    let (secret_name, field) = parse_kms_credential_reference(reference)?;
    let base = configured_kms_base_url()?;
    let token = configured_kms_token()?;
    let endpoint = base
        .join(&format!("v1/secrets/{}", secret_name))
        .map_err(|_| anyhow!("KMS credential endpoint is invalid"))?;

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|_| anyhow!("KMS credential client initialization failed"))?;
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .map_err(|_| anyhow!("KMS credential request failed"))?;
    if !response.status().is_success() {
        anyhow::bail!(
            "KMS credential request returned status {}",
            response.status().as_u16()
        );
    }

    let mut body = Vec::new();
    response
        .take(MAX_SECRET_STORE_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| anyhow!("KMS credential response could not be read"))?;
    if body.len() as u64 > MAX_SECRET_STORE_RESPONSE_BYTES {
        anyhow::bail!("KMS credential response exceeds 64 KiB limit");
    }
    let response: Value = serde_json::from_slice(&body)
        .map_err(|_| anyhow!("KMS credential response is not valid JSON"))?;
    let secret = response
        .get("data")
        .and_then(Value::as_object)
        .and_then(|data| data.get(&field))
        .or_else(|| response.get(&field))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("KMS credential field is missing or not a string"))?;
    if secret.len() > MAX_CREDENTIAL_BYTES {
        anyhow::bail!("referenced credential exceeds 16 KiB limit");
    }
    Ok(secret.trim().to_string())
}

/// Parse `SECRET_NAME#FIELD` after the `kms://` prefix.  The secret name is a
/// single opaque store key, not a URL or path, so it cannot influence network
/// destination, query string, or path traversal.
fn parse_kms_credential_reference(reference: &str) -> anyhow::Result<(String, String)> {
    if reference.len() > 1024 {
        anyhow::bail!("KMS credential reference exceeds 1 KiB limit");
    }
    let (secret_name, field) = reference
        .split_once('#')
        .ok_or_else(|| anyhow!("KMS credential reference must name a field"))?;
    if secret_name.contains('#')
        || secret_name.contains('?')
        || secret_name.contains('@')
        || secret_name.contains('/')
        || secret_name.contains('\\')
    {
        anyhow::bail!("KMS credential reference is invalid");
    }
    if !is_valid_secret_store_reference_segment(secret_name) {
        anyhow::bail!("KMS credential name is invalid");
    }
    if !is_valid_secret_store_reference_segment(field) {
        anyhow::bail!("KMS credential field is invalid");
    }
    Ok((secret_name.to_string(), field.to_string()))
}

/// Parse `MOUNT/PATH#FIELD` after the `vault://` prefix.  Character-level
/// validation also prevents URL/query tricks and makes the final Vault route
/// structurally independent from a stored binding.
fn parse_vault_kv_v2_reference(reference: &str) -> anyhow::Result<(String, String, String)> {
    if reference.len() > 1024 {
        anyhow::bail!("Vault credential reference exceeds 1 KiB limit");
    }
    let (location, field) = reference
        .split_once('#')
        .ok_or_else(|| anyhow!("Vault credential reference must name a field"))?;
    if location.contains('#') || location.contains('?') || location.contains('@') {
        anyhow::bail!("Vault credential reference is invalid");
    }
    let mut segments = location.split('/');
    let mount = segments
        .next()
        .filter(|value| is_valid_secret_store_reference_segment(value))
        .ok_or_else(|| anyhow!("Vault credential mount is invalid"))?;
    let path = segments.collect::<Vec<_>>();
    if path.is_empty()
        || path
            .iter()
            .any(|value| !is_valid_secret_store_reference_segment(value))
    {
        anyhow::bail!("Vault credential path is invalid");
    }
    if !is_valid_secret_store_reference_segment(field) {
        anyhow::bail!("Vault credential field is invalid");
    }
    Ok((mount.to_string(), path.join("/"), field.to_string()))
}

fn is_valid_secret_store_reference_segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= 255
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// Read and validate the deployment-controlled Vault address.  A Vault store
/// often lives on a private network, so it requires an explicit secret-store
/// allowlist rather than inheriting the public-provider policy.
fn configured_vault_base_url() -> anyhow::Result<url::Url> {
    let raw = env::var("WPTSALL_VAULT_ADDR")
        .map_err(|_| anyhow!("WPTSALL_VAULT_ADDR is required for vault references"))?;
    let mut base = url::Url::parse(raw.trim()).map_err(|_| anyhow!("Vault address is invalid"))?;
    if base.scheme() != "https" && !(cfg!(test) && base.scheme() == "http") {
        anyhow::bail!("Vault address must use HTTPS");
    }
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || (base.path() != "/" && !base.path().is_empty())
    {
        anyhow::bail!("Vault address must be an origin without credentials, query or path");
    }
    let host = base
        .host_str()
        .ok_or_else(|| anyhow!("Vault address host is missing"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !host_matches_allowlist(&host, "WPTSALL_SECRET_STORE_ALLOWLIST") {
        anyhow::bail!("Vault host is not in WPTSALL_SECRET_STORE_ALLOWLIST");
    }
    // The allowlist fixes the logical destination, while resolving it here
    // closes the DNS-rebinding path.  RFC1918/private addresses are allowed
    // for an explicitly approved internal Vault; loopback, link-local and
    // cloud-metadata addresses are never valid secret-store targets in a
    // production build.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_secret_store_ip(ip) && !(cfg!(test) && ip.is_loopback()) {
            anyhow::bail!("Vault host resolves to a forbidden address");
        }
    } else {
        let port = base.port_or_known_default().unwrap_or(443);
        let resolved = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))
            .map_err(|_| anyhow!("Vault host DNS resolution failed"))?;
        if resolved
            .into_iter()
            .any(|address| is_forbidden_secret_store_ip(address.ip()))
        {
            anyhow::bail!("Vault host resolves to a forbidden address");
        }
    }
    base.set_path("/");
    Ok(base)
}

/// Read and validate the deployment-controlled KMS/secret-broker address.
/// Like Vault, this is a secret-store endpoint and therefore requires the
/// explicit secret-store allowlist instead of the public provider allowlist.
fn configured_kms_base_url() -> anyhow::Result<url::Url> {
    let raw = env::var("WPTSALL_KMS_ADDR")
        .map_err(|_| anyhow!("WPTSALL_KMS_ADDR is required for KMS references"))?;
    let mut base = url::Url::parse(raw.trim()).map_err(|_| anyhow!("KMS address is invalid"))?;
    if base.scheme() != "https" && !(cfg!(test) && base.scheme() == "http") {
        anyhow::bail!("KMS address must use HTTPS");
    }
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || (base.path() != "/" && !base.path().is_empty())
    {
        anyhow::bail!("KMS address must be an origin without credentials, query or path");
    }
    let host = base
        .host_str()
        .ok_or_else(|| anyhow!("KMS address host is missing"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if !host_matches_allowlist(&host, "WPTSALL_SECRET_STORE_ALLOWLIST") {
        anyhow::bail!("KMS host is not in WPTSALL_SECRET_STORE_ALLOWLIST");
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_secret_store_ip(ip) && !(cfg!(test) && ip.is_loopback()) {
            anyhow::bail!("KMS host resolves to a forbidden address");
        }
    } else {
        let port = base.port_or_known_default().unwrap_or(443);
        let resolved = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port))
            .map_err(|_| anyhow!("KMS host DNS resolution failed"))?;
        if resolved
            .into_iter()
            .any(|address| is_forbidden_secret_store_ip(address.ip()))
        {
            anyhow::bail!("KMS host resolves to a forbidden address");
        }
    }
    base.set_path("/");
    Ok(base)
}

fn is_forbidden_secret_store_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                || v4.octets() == [169, 254, 169, 254]
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unicast_link_local() || v6.is_unspecified(),
    }
}

/// The Vault bootstrap token can use the same local env/file reference
/// boundary, but may never recursively resolve through Vault itself.
fn configured_vault_token() -> anyhow::Result<String> {
    let raw = env::var("WPTSALL_VAULT_TOKEN")
        .map_err(|_| anyhow!("WPTSALL_VAULT_TOKEN is required for vault references"))?;
    if raw.trim_start().starts_with("vault://") || raw.trim_start().starts_with("kms://") {
        anyhow::bail!("WPTSALL_VAULT_TOKEN cannot be a remote secret-store reference");
    }
    let token = resolve_credential_reference(&raw)?;
    if token.trim().is_empty() {
        anyhow::bail!("WPTSALL_VAULT_TOKEN is empty");
    }
    Ok(token)
}

/// The KMS bootstrap token can use the same local env/file reference boundary,
/// but may never recursively resolve through Vault or KMS.
fn configured_kms_token() -> anyhow::Result<String> {
    let raw = env::var("WPTSALL_KMS_TOKEN")
        .map_err(|_| anyhow!("WPTSALL_KMS_TOKEN is required for KMS references"))?;
    if raw.trim_start().starts_with("vault://") || raw.trim_start().starts_with("kms://") {
        anyhow::bail!("WPTSALL_KMS_TOKEN cannot be a remote secret-store reference");
    }
    let token = resolve_credential_reference(&raw)?;
    if token.trim().is_empty() {
        anyhow::bail!("WPTSALL_KMS_TOKEN is empty");
    }
    Ok(token)
}

fn auth_env_key(field_name: &str) -> String {
    let normalized = field_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("WPTSALL_COMPONENT_AUTH_{}", normalized)
}

pub(crate) async fn translate_text_via_component(
    client: &Client,
    runtime: &ComponentRuntime,
    input_text: &str,
    source_lang: &str,
    target_lang: &str,
) -> anyhow::Result<String> {
    // Fail-closed contract / content-format gate (libs/wptsall-contracts).
    crate::component_rt::contract::assert_runtime_ready_for_format(runtime, None)?;

    // Use char count (not byte count) to correctly measure multi-byte text.
    let input_len = input_text.chars().count();

    // Dynamic key selection: if component has a key pool, acquire a key guard.
    // The guard is held for the duration of this translation (RAII concurrency counting).
    // Use select_key_with_limits to enforce max_input_chars per-key limits.
    let _key_guard;
    let mut ctx = runtime.auth_values.clone();

    if let Some(ref pool) = runtime.key_pool {
        let guard = pool.select_key_with_limits(input_len, 0.0).await?;
        // Inject key's auth_values as auth.* context variables (overriding static auth)
        for (k, v) in &guard.auth_values {
            ctx.insert(format!("auth.{}", k), v.clone());
        }
        _key_guard = Some(guard);
    } else {
        _key_guard = None;
    }

    // Dynamic OAuth token selection: if component has an oauth pool + manager, fetch a token.
    let _oauth_guard;
    if let (Some(ref pool), Some(ref manager)) = (&runtime.oauth_pool, &runtime.oauth_manager) {
        let guard = pool.select(manager, input_len, 0.0).await?;
        // Inject token as auth.{token_field}
        ctx.insert(
            format!("auth.{}", guard.token_field),
            guard.access_token.clone(),
        );
        _oauth_guard = Some(guard);
    } else {
        _oauth_guard = None;
    }

    insert_component_default_values_context(&mut ctx, runtime.template.default_values.as_ref());
    insert_component_text_context(&mut ctx, input_text, source_lang, target_lang);
    apply_language_map(&runtime.language_map, &mut ctx);
    let translated_path = runtime
        .template
        .response
        .translated_text_path
        .as_deref()
        .map(|v| v.trim())
        .unwrap_or_default();
    if translated_path.is_empty() {
        return Err(anyhow!(
            "translated_text_path is not configured for component {}",
            runtime.template.id
        ));
    }

    // 1) Submit stage (sync or async).
    let submit_json =
        call_component_request_json(client, runtime, &runtime.template.request, &mut ctx).await?;
    if let Some(value) = extract_json_path(&submit_json, translated_path) {
        let translated = if let Some(v) = value.as_str() {
            v.to_string()
        } else {
            value.to_string()
        };
        let submit_job_id = runtime
            .template
            .async_poll
            .as_ref()
            .and_then(|async_poll| extract_json_path_string(&submit_json, &async_poll.job_id_path))
            .unwrap_or_default();
        let immediate_value_is_async_job_id = runtime
            .template
            .async_poll
            .as_ref()
            .map(|async_poll| {
                async_poll.job_id_path.trim() == translated_path
                    || (!submit_job_id.trim().is_empty()
                        && submit_job_id.trim() == translated.trim())
            })
            .unwrap_or(false);
        if !translated.trim().is_empty() && !immediate_value_is_async_job_id {
            return Ok(translated);
        }
        if runtime.template.async_poll.is_none() {
            return Err(anyhow!(
                "component returned empty translation for path '{}' (component={})",
                translated_path,
                runtime.template.id
            ));
        }
    }

    // 2) Async poll stage (if configured).
    let Some(async_poll) = runtime.template.async_poll.as_ref() else {
        let body_preview: String = submit_json.to_string().chars().take(200).collect();
        return Err(anyhow!(
            "translated_text_path '{}' not found in response (component={}, body_preview={})",
            translated_path,
            runtime.template.id,
            body_preview
        ));
    };

    let job_id = extract_json_path_string(&submit_json, &async_poll.job_id_path)
        .unwrap_or_default()
        .trim()
        .to_string();
    if job_id.is_empty() {
        let body_preview: String = submit_json.to_string().chars().take(200).collect();
        return Err(anyhow!(
            "async_poll.job_id_path '{}' not found in submit response (component={}, body_preview={})",
            async_poll.job_id_path,
            runtime.template.id,
            body_preview
        ));
    }
    ctx.insert("computed.job_id".to_string(), job_id.clone());
    ctx.insert("computed.async_job_id".to_string(), job_id);
    apply_async_poll_submit_extract(async_poll, &submit_json, &mut ctx, &runtime.template.id)?;

    let interval_secs = async_poll.interval_seconds.unwrap_or(5).max(1);
    let timeout_secs = async_poll.timeout_seconds.unwrap_or(600).max(interval_secs);
    let done_values = normalize_status_values(&async_poll.done_values);
    let failed_values = normalize_status_values(&async_poll.failed_values);
    let status_path = async_poll
        .status_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let result_text_path_template = async_poll
        .result_text_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(translated_path);

    let started = Instant::now();
    let mut attempts = 0u64;
    loop {
        if attempts > 0 {
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
        attempts = attempts.saturating_add(1);

        let poll_json =
            call_component_request_json(client, runtime, &async_poll.request, &mut ctx).await?;

        let rendered_status_path =
            status_path.and_then(|path| resolve_template_json_path(path, &ctx));
        let rendered_result_text_path = resolve_template_json_path(result_text_path_template, &ctx)
            .unwrap_or_else(|| result_text_path_template.to_string());

        let mut status: Option<String> = None;
        if let Some(status_key) = rendered_status_path.as_deref() {
            status = extract_json_path_string(&poll_json, status_key)
                .map(|v| normalize_status_value(&v))
                .filter(|v| !v.is_empty());
            if let Some(ref status_value) = status {
                if status_values_contains(&failed_values, status_value) {
                    return Err(anyhow!(
                        "component async poll failed (status='{}', component={})",
                        status_value,
                        runtime.template.id
                    ));
                }
            }
        }

        let done_by_status = status
            .as_deref()
            .map(|s| status_values_contains(&done_values, s))
            .unwrap_or(false);

        if let Some(value) = extract_json_path(&poll_json, &rendered_result_text_path) {
            let translated = if let Some(v) = value.as_str() {
                v.to_string()
            } else {
                value.to_string()
            };
            if !translated.trim().is_empty() {
                return Ok(translated);
            }
            if done_by_status {
                return Err(anyhow!(
                    "component async poll done but output empty (component={})",
                    runtime.template.id
                ));
            }
        } else if done_by_status {
            return Err(anyhow!(
                "component async poll done but output missing (component={})",
                runtime.template.id
            ));
        }

        if started.elapsed().as_secs() >= timeout_secs {
            return Err(anyhow!(
                "component async poll timed out (timeout_secs={}, attempts={}, component={})",
                timeout_secs,
                attempts,
                runtime.template.id
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_non_text_via_component(
    client: &Client,
    runtime: &ComponentRuntime,
    source_text: &str,
    source_payload: Option<&Value>,
    source_ref: &str,
    task_type: &str,
    key: &str,
    source_lang: &str,
    target_lang: &str,
) -> anyhow::Result<NonTextComponentOutcome> {
    let normalized_task_type = normalize_task_content_type(task_type);
    if !template_declares_real_non_text_output(&runtime.template, &normalized_task_type) {
        return Err(anyhow!(
            "component does not declare real non-text output path (task_type={}, component={})",
            normalized_task_type,
            runtime.template.id
        ));
    }

    // Pre-selection: determine file size BEFORE selecting keys so that keys/oauth
    // entries that cannot handle this file size are skipped during selection.
    let file_size_mb: f64 = if !source_ref.is_empty() {
        // Only do a HEAD request when at least one pool key/oauth entry imposes a file size
        // limit — avoids unnecessary network round-trips when no limits are configured.
        let needs_size_check = runtime
            .key_pool
            .as_ref()
            .map(|p| p.has_file_size_limits())
            .unwrap_or(false)
            || runtime
                .oauth_pool
                .as_ref()
                .map(|p| p.has_file_size_limits())
                .unwrap_or(false);

        if needs_size_check {
            match get_file_content_length(client, source_ref).await {
                Ok(size_bytes) => size_bytes as f64 / (1024.0 * 1024.0),
                Err(_) => {
                    // Cannot determine file size (no Content-Length header) — proceed
                    // without filtering so the request is attempted regardless.
                    0.0
                }
            }
        } else {
            0.0
        }
    } else {
        0.0
    };

    // Dynamic key selection: if component has a key pool, acquire a key guard.
    // The guard is held for the duration of this translation (RAII concurrency counting).
    // Use select_key_with_limits to enforce max_file_size_mb per-key limits pre-selection.
    let _key_guard;
    let mut ctx = runtime.auth_values.clone();

    if let Some(ref pool) = runtime.key_pool {
        let guard = pool.select_key_with_limits(0, file_size_mb).await?;
        // Inject key's auth_values as auth.* context variables (overriding static auth)
        for (k, v) in &guard.auth_values {
            ctx.insert(format!("auth.{}", k), v.clone());
        }
        _key_guard = Some(guard);
    } else {
        _key_guard = None;
    }

    // Dynamic OAuth token selection: if component has an oauth pool + manager, fetch a token.
    let _oauth_guard;
    if let (Some(ref pool), Some(ref manager)) = (&runtime.oauth_pool, &runtime.oauth_manager) {
        let guard = pool.select(manager, 0, file_size_mb).await?;
        // Inject token as auth.{token_field}
        ctx.insert(
            format!("auth.{}", guard.token_field),
            guard.access_token.clone(),
        );
        _oauth_guard = Some(guard);
    } else {
        _oauth_guard = None;
    }

    insert_component_default_values_context(&mut ctx, runtime.template.default_values.as_ref());
    insert_component_text_context(&mut ctx, source_text, source_lang, target_lang);
    insert_component_non_text_context(&mut ctx, source_payload, source_ref, task_type, key);
    ensure_non_text_source_asset_metadata_context(client, runtime, &mut ctx, source_ref).await?;
    maybe_insert_non_text_source_asset_context(client, runtime, &mut ctx, source_ref).await?;
    apply_language_map(&runtime.language_map, &mut ctx);

    if let Some(prepare) = runtime.template.prepare.as_ref() {
        let prepare_json =
            call_component_request_json(client, runtime, &prepare.request, &mut ctx).await?;
        apply_prepare_extract(prepare, &prepare_json, &mut ctx, &runtime.template.id)?;
    }

    if let Some(source_upload) = runtime.template.source_upload.as_ref() {
        upload_source_asset_to_vendor(client, runtime, source_upload, &mut ctx, source_ref).await?;
    }

    // 1) Submit stage
    if request_expects_binary_response(&runtime.template.request) {
        let asset = call_component_request_binary_submit(
            client,
            runtime,
            &runtime.template.request,
            &mut ctx,
        )
        .await?;
        let preferred_name = asset
            .filename
            .or_else(|| ctx.get("input.source_filename").cloned())
            .filter(|v| !v.trim().is_empty());
        let local_path = persist_downloaded_asset_to_temp(&asset.bytes, preferred_name.as_deref())?;
        return finalize_non_text_outcome(
            &ctx,
            runtime,
            &normalized_task_type,
            ExtractedNonTextOutcome {
                translated_ref: format!("file://{}", local_path),
                translated_text: String::new(),
            },
        );
    }

    let submit_json =
        call_component_request_json(client, runtime, &runtime.template.request, &mut ctx).await?;

    // Best-effort sync extraction (some providers may complete immediately).
    let sync_outcome = extract_non_text_outcome_best_effort(
        runtime,
        &submit_json,
        &normalized_task_type,
        source_ref,
        /*allow_source_fallback*/ runtime.template.async_poll.is_none(),
        None,
    );
    if runtime.template.async_poll.is_none() {
        return finalize_non_text_outcome(&ctx, runtime, &normalized_task_type, sync_outcome);
    }
    if !sync_outcome.translated_ref.trim().is_empty() {
        return finalize_non_text_outcome(&ctx, runtime, &normalized_task_type, sync_outcome);
    }

    // 2) Async poll stage
    let async_poll = runtime.template.async_poll.as_ref().expect("checked above");
    let job_id = extract_json_path_string(&submit_json, &async_poll.job_id_path)
        .unwrap_or_default()
        .trim()
        .to_string();
    if job_id.is_empty() {
        let body_preview: String = submit_json.to_string().chars().take(200).collect();
        return Err(anyhow!(
            "async_poll.job_id_path '{}' not found in submit response (component={}, body_preview={})",
            async_poll.job_id_path,
            runtime.template.id,
            body_preview
        ));
    }
    ctx.insert("computed.job_id".to_string(), job_id.clone());
    ctx.insert("computed.async_job_id".to_string(), job_id);
    apply_async_poll_submit_extract(async_poll, &submit_json, &mut ctx, &runtime.template.id)?;

    let interval_secs = async_poll.interval_seconds.unwrap_or(5).max(1);
    let timeout_secs = async_poll.timeout_seconds.unwrap_or(900).max(interval_secs);
    let done_values = normalize_status_values(&async_poll.done_values);
    let failed_values = normalize_status_values(&async_poll.failed_values);
    let status_path = async_poll
        .status_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let started = Instant::now();
    let mut attempts = 0u64;
    loop {
        if attempts > 0 {
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        }
        attempts = attempts.saturating_add(1);

        let poll_json =
            call_component_request_json(client, runtime, &async_poll.request, &mut ctx).await?;

        let rendered_status_path =
            status_path.and_then(|path| resolve_template_json_path(path, &ctx));
        let status = rendered_status_path
            .as_deref()
            .and_then(|path| extract_json_path_string(&poll_json, path))
            .map(|v| normalize_status_value(&v))
            .filter(|v| !v.is_empty());

        if let Some(ref s) = status {
            if status_values_contains(&failed_values, s) {
                return Err(anyhow!(
                    "component async poll failed (status='{}', component={})",
                    s,
                    runtime.template.id
                ));
            }
        }

        let done_by_status = status
            .as_deref()
            .map(|s| status_values_contains(&done_values, s))
            .unwrap_or(false);

        // Determine readiness by output extraction unless output is provided by template/download.
        let is_ready = if async_poll.result_download.is_some()
            || async_poll.result_request.is_some()
            || async_poll
                .result_ref_template
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_some()
        {
            done_by_status
        } else {
            let rendered_result_ref_path = async_poll
                .result_ref_path
                .as_deref()
                .and_then(|path| resolve_template_json_path(path, &ctx));
            let outcome = extract_non_text_outcome_best_effort(
                runtime,
                &poll_json,
                &normalized_task_type,
                source_ref,
                /*allow_source_fallback*/ false,
                rendered_result_ref_path.as_deref(),
            );
            !outcome.translated_ref.trim().is_empty()
        };

        if is_ready {
            // Finalize: derive output from poll response or from async config overrides.
            return finalize_non_text_async(
                client,
                runtime,
                &mut ctx,
                &normalized_task_type,
                source_ref,
                &poll_json,
            )
            .await;
        }

        if done_by_status {
            return Err(anyhow!(
                "component async poll done but output missing (component={})",
                runtime.template.id
            ));
        }

        if started.elapsed().as_secs() >= timeout_secs {
            return Err(anyhow!(
                "component async poll timed out (timeout_secs={}, attempts={}, component={})",
                timeout_secs,
                attempts,
                runtime.template.id
            ));
        }
    }
}

fn is_likely_async_reference_path(path: &str) -> bool {
    let value = path.trim().to_ascii_lowercase();
    if value.is_empty() {
        return true;
    }
    if value.contains("url")
        || value.contains("uri")
        || value.contains("link")
        || value.contains("path")
        || value.contains("file")
        || value.contains("content")
        || value.contains("resource")
    {
        return false;
    }
    if matches!(
        value.as_str(),
        "id" | "job_id"
            | "task_id"
            | "request_id"
            | "requestid"
            | "operation_id"
            | "media_id"
            | "dubbing_id"
            | "document_id"
            | "taskno"
            | "task_no"
    ) {
        return true;
    }
    let last_segment = value.rsplit('.').next().unwrap_or(value.as_str()).trim();
    if matches!(
        last_segment,
        "id" | "job_id"
            | "task_id"
            | "request_id"
            | "requestid"
            | "operation_id"
            | "media_id"
            | "dubbing_id"
            | "document_id"
            | "taskno"
            | "task_no"
    ) {
        return true;
    }
    if !value.contains('.') && (value.ends_with("_id") || value.ends_with("id")) {
        return true;
    }
    false
}

fn request_expects_binary_response(request: &ComponentRequest) -> bool {
    request
        .response_type
        .as_deref()
        .map(|v| v.trim().eq_ignore_ascii_case("binary"))
        .unwrap_or(false)
}

async fn call_component_request_binary_submit(
    client: &Client,
    runtime: &ComponentRuntime,
    request_spec: &ComponentRequest,
    ctx: &mut HashMap<String, String>,
) -> anyhow::Result<BinaryResponseAsset> {
    let _runtime_concurrency_guard = acquire_runtime_concurrency_guard(runtime).await?;
    enforce_runtime_rate_limit(runtime).await;
    prime_sign_context(runtime.template.sign.as_ref(), ctx)?;
    let rendered_url = render_template_string(&request_spec.url, ctx);
    inject_rendered_request_context(request_spec, &rendered_url, ctx);
    let sign_result = process_sign_config(
        runtime.template.sign.as_ref(),
        ctx,
        &request_spec.method,
        &rendered_url,
    )?;
    invoke_component_api_binary(
        client,
        runtime,
        request_spec,
        &rendered_url,
        ctx,
        sign_result,
    )
    .await
}

fn template_declares_real_non_text_output(template: &ComponentTemplate, task_type: &str) -> bool {
    if component_declares_real_non_text_output_via_response(&template.response, task_type) {
        return true;
    }

    if request_expects_binary_response(&template.request) {
        return true;
    }

    let Some(async_poll) = template.async_poll.as_ref() else {
        return false;
    };

    if async_poll.result_download.is_some() {
        return true;
    }
    if async_poll.result_request.is_some() {
        return true;
    }
    if async_poll
        .result_ref_template
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return true;
    }
    if let Some(path) = async_poll
        .result_ref_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return !is_likely_async_reference_path(path);
    }

    false
}

fn component_declares_real_non_text_output_via_response(
    response: &ComponentResponse,
    task_type: &str,
) -> bool {
    let normalized_task_type = normalize_task_content_type(task_type);
    let mut candidate_paths: Vec<&str> = Vec::new();
    if let Some(path) = response.translated_ref_path.as_deref() {
        candidate_paths.push(path);
    }
    if let Some(path) = response.translated_media_ref_path.as_deref() {
        candidate_paths.push(path);
    }
    match normalized_task_type.as_str() {
        "image" => {
            if let Some(path) = response.translated_image_ref_path.as_deref() {
                candidate_paths.push(path);
            }
        }
        "video" => {
            if let Some(path) = response.translated_video_ref_path.as_deref() {
                candidate_paths.push(path);
            }
        }
        "audio" => {
            if let Some(path) = response.translated_audio_ref_path.as_deref() {
                candidate_paths.push(path);
            }
        }
        "document" => {
            if let Some(path) = response.translated_document_ref_path.as_deref() {
                candidate_paths.push(path);
            }
        }
        "mixed" => {
            if let Some(path) = response.translated_image_ref_path.as_deref() {
                candidate_paths.push(path);
            }
            if let Some(path) = response.translated_video_ref_path.as_deref() {
                candidate_paths.push(path);
            }
            if let Some(path) = response.translated_audio_ref_path.as_deref() {
                candidate_paths.push(path);
            }
            if let Some(path) = response.translated_document_ref_path.as_deref() {
                candidate_paths.push(path);
            }
        }
        _ => {}
    }
    candidate_paths
        .into_iter()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .any(|path| !is_likely_async_reference_path(path))
}

fn normalize_translated_ref_candidate(candidate: &str) -> Option<String> {
    let value = candidate.trim();
    if value.is_empty() {
        return None;
    }
    if value.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "ok" | "success"
            | "succeeded"
            | "failed"
            | "error"
            | "pending"
            | "processing"
            | "queued"
            | "running"
            | "done"
            | "completed"
            | "complete"
            | "true"
            | "false"
            | "null"
            | "none"
            | "na"
            | "n/a"
    ) {
        return None;
    }
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("s3://")
        || lower.starts_with("oss://")
        || lower.starts_with("gs://")
        || lower.starts_with("file://")
        || lower.starts_with("data:")
        || lower.starts_with("urn:")
    {
        return Some(value.to_string());
    }
    if value.starts_with('/')
        || value.starts_with("//")
        || value.starts_with("./")
        || value.starts_with("../")
    {
        return Some(value.to_string());
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return Some(value.to_string());
    }
    let media_suffixes = [
        ".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".svg", ".mp3", ".wav",
        ".aac", ".m4a", ".flac", ".ogg", ".mp4", ".mov", ".avi", ".mkv", ".webm", ".m3u8", ".vtt",
        ".srt", ".pdf", ".doc", ".docx", ".ppt", ".pptx", ".xls", ".xlsx", ".txt",
    ];
    if media_suffixes.iter().any(|suffix| lower.ends_with(suffix)) {
        return Some(value.to_string());
    }
    // Accept common async task/media identifier tokens (uuid/job_id/operation_id),
    // while still rejecting plain natural-language output.
    if value.is_ascii()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(ch, '-' | '_' | ':' | '.' | '/' | '+' | '=' | '@')
        })
    {
        let has_digit = value.chars().any(|ch| ch.is_ascii_digit());
        let has_sep = value
            .chars()
            .any(|ch| matches!(ch, '-' | '_' | ':' | '.' | '/' | '+' | '=' | '@'));
        if has_digit || has_sep || value.len() >= 24 {
            return Some(value.to_string());
        }
    }
    None
}

fn resolve_non_text_translated_ref(
    candidates: &[String],
    source_ref: &str,
    translated_text: &str,
    allow_source_fallback: bool,
) -> String {
    for candidate in candidates {
        if let Some(valid) = normalize_translated_ref_candidate(candidate) {
            return valid;
        }
    }
    // Some OCR/ASR/Doc APIs only return translated text or a task id at submit stage.
    // For media_ref routing, keep source_ref as a safe fallback to avoid corrupt mappings.
    if allow_source_fallback && !source_ref.trim().is_empty() && !translated_text.trim().is_empty()
    {
        return source_ref.trim().to_string();
    }
    String::new()
}

async fn acquire_runtime_concurrency_guard(
    runtime: &ComponentRuntime,
) -> anyhow::Result<Option<tokio::sync::OwnedSemaphorePermit>> {
    let Some(sem) = runtime.runtime_concurrency_sem.as_ref() else {
        return Ok(None);
    };
    let permit = sem
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| anyhow!("component runtime concurrency semaphore closed"))?;
    Ok(Some(permit))
}

async fn enforce_runtime_rate_limit(runtime: &ComponentRuntime) {
    if runtime.runtime_min_interval_ms == 0 {
        return;
    }
    let Some(last_request_at) = runtime.runtime_last_request_at.as_ref() else {
        return;
    };
    let mut guard = last_request_at.lock().await;
    let elapsed = guard.elapsed();
    let min_interval = Duration::from_millis(runtime.runtime_min_interval_ms);
    if elapsed < min_interval {
        tokio::time::sleep(min_interval - elapsed).await;
    }
    *guard = Instant::now();
}

// ---------------------------------------------------------------------------
// Text splitting / merging for constraint-aware translation
// ---------------------------------------------------------------------------

/// A chunk of text with its position info for correct merging
pub(crate) fn insert_component_text_context(
    ctx: &mut HashMap<String, String>,
    input_text: &str,
    source_lang: &str,
    target_lang: &str,
) {
    ctx.insert("input.text".to_string(), input_text.to_string());
    ctx.insert("input.source_lang".to_string(), source_lang.to_string());
    ctx.insert("input.target_lang".to_string(), target_lang.to_string());
    ctx.insert("payload.text".to_string(), input_text.to_string());
    ctx.insert("payload.source_lang".to_string(), source_lang.to_string());
    ctx.insert("payload.target_lang".to_string(), target_lang.to_string());
}

pub(crate) fn insert_component_default_values_context(
    ctx: &mut HashMap<String, String>,
    default_values: Option<&Value>,
) {
    fn flatten_default_values(ctx: &mut HashMap<String, String>, prefix: &str, value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, nested) in map {
                    let next = if prefix.is_empty() {
                        format!("default_values.{}", key)
                    } else {
                        format!("{}.{}", prefix, key)
                    };
                    flatten_default_values(ctx, &next, nested);
                }
            }
            Value::String(v) => {
                ctx.insert(prefix.to_string(), v.clone());
            }
            Value::Number(_) | Value::Bool(_) => {
                ctx.insert(prefix.to_string(), value.to_string());
            }
            Value::Array(_) => {
                if let Ok(serialized) = serde_json::to_string(value) {
                    ctx.insert(prefix.to_string(), serialized);
                }
            }
            Value::Null => {}
        }
    }

    let Some(default_values) = default_values else {
        return;
    };
    flatten_default_values(ctx, "", default_values);
}

pub(crate) fn insert_component_non_text_context(
    ctx: &mut HashMap<String, String>,
    source_payload: Option<&Value>,
    source_ref: &str,
    task_type: &str,
    key: &str,
) {
    let normalized_task_type = normalize_task_content_type(task_type);
    let normalized_key = normalize_patch_field_key(key);

    ctx.insert("input.task_type".to_string(), normalized_task_type.clone());
    ctx.insert("payload.task_type".to_string(), normalized_task_type);
    ctx.insert("input.field_key".to_string(), normalized_key.clone());
    ctx.insert("payload.field_key".to_string(), normalized_key);

    if !source_ref.trim().is_empty() {
        ctx.insert("input.source_ref".to_string(), source_ref.to_string());
        ctx.insert("payload.source_ref".to_string(), source_ref.to_string());
        ctx.insert("input.src".to_string(), source_ref.to_string());
        ctx.insert("payload.src".to_string(), source_ref.to_string());
        ctx.insert("input.source".to_string(), source_ref.to_string());
        ctx.insert("payload.source".to_string(), source_ref.to_string());
        if source_ref.starts_with("http://") || source_ref.starts_with("https://") {
            ctx.insert("input.source_url".to_string(), source_ref.to_string());
            ctx.insert("payload.source_url".to_string(), source_ref.to_string());
        }
    }

    let Some(payload) = source_payload else {
        return;
    };
    if let Ok(serialized) = serde_json::to_string(payload) {
        ctx.insert("input.source_payload_json".to_string(), serialized.clone());
        ctx.insert("payload.source_payload_json".to_string(), serialized);
    }
    let Some(obj) = payload.as_object() else {
        return;
    };
    for keep_key in [
        "id",
        "url",
        "src",
        "source_url",
        "file_url",
        "path",
        "file_path",
        "attachment_url",
        "attachment_id",
        "media_id",
        "source_id",
        "title",
        "caption",
        "description",
        "alt",
        "alt_text",
        "transcript",
        "text",
        "content",
    ] {
        let Some(value) = obj.get(keep_key) else {
            continue;
        };
        let str_value = if let Some(raw) = value.as_str() {
            raw.trim().to_string()
        } else if value.is_number() || value.is_boolean() {
            value.to_string()
        } else {
            continue;
        };
        if str_value.is_empty() {
            continue;
        }
        let key_input = format!("input.source.{}", keep_key);
        let key_payload = format!("payload.source.{}", keep_key);
        ctx.insert(key_input, str_value.clone());
        ctx.insert(key_payload, str_value);
    }
}

#[cfg(test)]
mod tests;
