use anyhow::{anyhow, Context};
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

pub(crate) mod provider_recovery;



/// Validate a rendered provider URL before any component-side egress.
///
/// Provider templates are operator-controlled input, so this check is kept at
/// the final request boundary (after template rendering) and is shared by all
/// request variants.  Literal IPs AND DNS-resolved addresses are checked
/// against the metadata floor (S5/N-1, 12 批 A4): a hostname that resolves
/// to the instance-metadata service — directly, via an IPv4-mapped IPv6
/// literal, or via the AWS IMDS IPv6 endpoint — is rejected. Resolution
/// failure is allowed through (the request itself will fail); only resolved
/// forbidden addresses are blocked. An optional comma-separated
/// Translation provider URL policy for the local-first Client.
///
/// Product rule: operators may point a catalog/component at **any** http(s)
/// vendor endpoint (including loopback mock-api / local LLM). We only reject
/// clearly unsafe shapes: non-http schemes and credentials embedded in the URL.
/// Cloud metadata hostnames remain blocked as a hard SSRF floor; loopback and
/// private ranges stay reachable by product rule (the allowlist below is a
/// no-op compatibility hook, NOT an enforced gate).
pub(crate) fn assert_provider_url_allowed(raw: &str) -> anyhow::Result<()> {
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
    // url::Url::host_str() keeps IPv6 brackets ("[::1]"); strip them so
    // IpAddr::from_str and the resolver see the bare literal.
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(&host)
        .to_string();

    // Hard floor only: cloud instance-metadata endpoints (not translation APIs).
    if host == "metadata.google.internal"
        || host == "metadata"
        || host.ends_with(".metadata.google.internal")
    {
        return Err(anyhow!("provider host is blocked by egress policy"));
    }
    // S5+N-1 (07 audit, 12 批 A4): literal IPs are checked with IPv6
    // unmapping (the old V4-only match let `[::ffff:169.254.169.254]`
    // through); non-literal hostnames are resolved and every resolved
    // address is re-checked, closing the resolve-then-fetch metadata
    // window for DNS-rebinding shapes.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_egress_ip(ip) {
            return Err(anyhow!("provider host is blocked by egress policy"));
        }
    } else if let Ok(addrs) = (host.as_str(), 80).to_socket_addrs() {
        if resolved_addrs_hit_forbidden_egress(addrs) {
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

/// The metadata SSRF floor: the IPv4 instance-metadata address in every
/// spelling (dotted, IPv4-mapped IPv6, hex-mapped), the AWS IMDS IPv6
/// endpoint, and IPv6 link-local (the fe80::/10 counterpart of the
/// 169.254/16 metadata surface). Loopback, ULA and private ranges are
/// deliberately NOT here — local mock-api / local LLM product lanes
/// require them (09 审核口径).
pub(crate) fn is_forbidden_egress_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets() == [169, 254, 169, 254],
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return v4.octets() == [169, 254, 169, 254];
            }
            let seg = v6.segments();
            // fd00:ec2::254 — AWS IMDS dual-stack endpoint.
            seg == [0xfd00, 0x0ec2, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0254]
                // fe80::/10 — IPv6 link-local.
                || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}

/// True when any resolved address lands on the metadata floor. Split from
/// the resolver call so the decision is unit-testable without live DNS.
fn resolved_addrs_hit_forbidden_egress(
    mut addrs: impl Iterator<Item = std::net::SocketAddr>,
) -> bool {
    addrs.any(|addr| is_forbidden_egress_ip(addr.ip()))
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

/// Origin tuple for redirect-boundary decisions: (scheme, host, port).
/// Shared by the post-hoc redirect check and the reqwest redirect policy
/// so both always agree on what "same origin" means.
fn url_origin_tuple(u: &url::Url) -> (String, String, Option<u16>) {
    (
        u.scheme().to_ascii_lowercase(),
        u.host_str()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_ascii_lowercase(),
        u.port_or_known_default(),
    )
}

/// Reject redirects that leave the configured provider origin.  Reqwest
/// follows redirects by default; checking the final URL closes the common
/// cross-host redirect escape from an otherwise allowlisted endpoint.
/// With `provider_redirect_policy` armed on the client (N-2), a cross-origin
/// hop never happens — this check is the second layer that reports it if a
/// client without the policy slips through.
pub(super) fn assert_provider_redirect_origin(
    original: &str,
    final_url: &url::Url,
) -> anyhow::Result<()> {
    let initial =
        url::Url::parse(original.trim()).map_err(|_| anyhow!("provider URL is invalid"))?;
    if url_origin_tuple(&initial) != url_origin_tuple(final_url) {
        return Err(anyhow!("provider redirect crossed host boundary"));
    }
    assert_provider_url_allowed(final_url.as_str())
}

/// N-2 (07 audit, 12 批 A3): reqwest redirect policy for provider egress
/// clients. Same-origin redirects follow automatically (auth headers stay
/// on the trusted origin); a redirect that leaves the configured provider
/// origin is NEVER followed — the Authorization header must not be sent
/// cross-host, so the 30x surfaces to the caller and the response layer
/// reports the boundary crossing. Vault/KMS resolvers keep their stricter
/// `Policy::none()` (no redirect is legitimate there).
pub(crate) fn provider_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let Some(first) = attempt.previous().first() else {
            return attempt.follow();
        };
        if url_origin_tuple(first) == url_origin_tuple(attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
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

pub(crate) use self::chunking::{
    translate_rich_html_blocks, translate_rich_html_blocks_with_env,
    translate_text_with_constraints, translate_text_with_constraints_with_env,
};
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
    if base.scheme() != "https" && !(false && base.scheme() == "http") {
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
        if is_forbidden_secret_store_ip(ip) && !(false && ip.is_loopback()) {
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
    if base.scheme() != "https" && !(false && base.scheme() == "http") {
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
        if is_forbidden_secret_store_ip(ip) && !(false && ip.is_loopback()) {
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
    translate_text_via_component_with_env(
        client,
        runtime,
        input_text,
        source_lang,
        target_lang,
        None,
    )
    .await
}

/// A durable environment protects both synchronous and asynchronous provider
/// calls. Unknown submits are parked; completed results replay before credential
/// selection or egress. Known async jobs restore the frozen context and poll.
pub(crate) async fn translate_text_via_component_with_env(
    client: &Client,
    runtime: &ComponentRuntime,
    input_text: &str,
    source_lang: &str,
    target_lang: &str,
    async_env: Option<crate::db::async_jobs::AsyncJobEnv>,
) -> anyhow::Result<String> {
    // Fail-closed contract / content-format gate (libs/wptsall-contracts).
    crate::component_rt::contract::assert_runtime_ready_for_format(runtime, None)?;
    provider_recovery::validate_contract(&runtime.template)?;
    anyhow::ensure!(
        async_env.is_some()
            || runtime
                .template
                .async_poll
                .as_ref()
                .is_none_or(|poll| poll.reconcile.is_none()),
        "provider reconciliation template requires a persistent translation unit"
    );

    let async_env = async_env
        .map(|env| {
            env.for_runtime(
                runtime,
                &serde_json::json!(input_text),
                source_lang,
                target_lang,
            )
        })
        .transpose()?;
    // Read-only ready replay must remain available even if new claim writes
    // fail. Every path that might do provider work then acquires a unit owner
    // and rechecks recovery while holding that native authority.
    if let Some(env) = &async_env {
        if let crate::db::async_jobs::ProviderRecovery::Ready(result) =
            crate::db::async_jobs::recover_provider_operation(env).await?
        {
            let text = result
                .as_str()
                .ok_or_else(|| anyhow!("invalid saved text result"))?;
            super::content_safety::validate_interpolation_tokens(input_text, text)?;
            return Ok(text.to_string());
        }
    }
    validate_http_limits(&runtime.template)?;
    let async_env = match async_env {
        Some(env) => Some(crate::db::async_jobs::ProviderExecution::acquire(env).await?),
        None => None,
    };
    let resumed_job = match async_env.as_ref() {
        Some(env) => {
            let row = match crate::db::async_jobs::recover_provider_operation(env).await? {
                crate::db::async_jobs::ProviderRecovery::Ready(result) => {
                    let text = result
                        .as_str()
                        .ok_or_else(|| anyhow!("invalid saved text result"))?;
                    super::content_safety::validate_interpolation_tokens(input_text, text)?;
                    return Ok(text.to_string());
                }
                crate::db::async_jobs::ProviderRecovery::Polling(row) => {
                    crate::db::async_jobs::upsert_polling_job(
                        env,
                        &runtime.template.id,
                        &row.job_id,
                        &row.ctx,
                        source_lang,
                        target_lang,
                    )
                    .await?;
                    Some(row)
                }
                crate::db::async_jobs::ProviderRecovery::Vacant => {
                    crate::db::async_jobs::find_polling_job(env).await?
                }
            };
            anyhow::ensure!(
                row.is_none() || runtime.template.async_poll.is_some(),
                "saved provider job requires an async-poll template; retained"
            );
            crate::bindings::encrypt_for_save("{}")?;
            row
        }
        None => None,
    };

    // Use char count (not byte count) to correctly measure multi-byte text.
    let input_len = input_text.chars().count();

    // Dynamic key selection: if component has a key pool, acquire a key guard.
    // The guard is held for the duration of this translation (RAII concurrency counting).
    // Use select_key_with_limits to enforce max_input_chars per-key limits.
    let _key_guard;
    let mut ctx = resumed_job
        .as_ref()
        .map(|row| row.ctx.clone())
        .unwrap_or_else(|| runtime.auth_values.clone());

    if let Some(pool) = runtime.key_pool.as_ref().filter(|_| resumed_job.is_none()) {
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
    if let Some((pool, manager)) = runtime
        .oauth_pool
        .as_ref()
        .zip(runtime.oauth_manager.as_ref())
        .filter(|_| resumed_job.is_none())
    {
        manager.assert_proxy_profile(runtime.proxy_profile_id.as_deref())?;
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

    if resumed_job.is_none() {
        insert_component_default_values_context(&mut ctx, runtime.template.default_values.as_ref());
        insert_component_text_context(&mut ctx, input_text, source_lang, target_lang);
        apply_language_map(&runtime.language_map, &mut ctx);
    }
    let prior_attempts = resumed_job.as_ref().map(|job| job.attempts).unwrap_or(0);
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

    // GAP-04: crash resume — a surviving 'polling' row for this exact unit
    // (domain×relation×object×field×chunk×lane) means a previous process
    // already submitted (and the provider already billed) this job. Skip
    // the submit, restore the persisted ctx snapshot verbatim, keep
    // polling. Sync components use the same operation record for replay and
    // unknown-submit protection, without creating a polling projection.
    // 1) Submit stage (sync or async) — skipped entirely on resume.
    let submit_json = match &resumed_job {
        Some(row) => {
            ctx = row.ctx.clone();
            serde_json::Value::Null
        }
        None => {
            if let Some(env) = async_env.as_ref() {
                crate::db::async_jobs::begin_provider_runtime_intent(
                    env,
                    runtime,
                    &serde_json::json!(input_text),
                    &mut ctx,
                    source_lang,
                    target_lang,
                )
                .await?;
                crate::db::async_jobs::commit_provider_submit_context(env, &ctx).await?;
            }
            call_component_request_json(client, runtime, &runtime.template.request, &mut ctx)
                .await?
        }
    };
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
            super::content_safety::validate_interpolation_tokens(input_text, &translated)?;
            if let Some(env) = async_env.as_ref() {
                crate::db::async_jobs::save_provider_result(env, serde_json::json!(translated))
                    .await?;
            }
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
        return Err(anyhow!(
            "translated_text_path '{}' not found in response (component={})",
            translated_path,
            runtime.template.id
        ));
    };

    let job_id = match resumed_job {
        Some(row) => row.job_id,
        None => {
            let job_id = extract_json_path_string(&submit_json, &async_poll.job_id_path)
                .unwrap_or_default()
                .trim()
                .to_string();
            if job_id.is_empty() {
                return Err(anyhow!(
                    "async_poll.job_id_path '{}' not found in submit response (component={})",
                    async_poll.job_id_path,
                    runtime.template.id
                ));
            }
            ctx.insert("computed.job_id".to_string(), job_id.clone());
            ctx.insert("computed.async_job_id".to_string(), job_id.clone());
            apply_async_poll_submit_extract(
                async_poll,
                &submit_json,
                &mut ctx,
                &runtime.template.id,
            )?;

            // GAP-04: persist BEFORE the first poll — this instant opens
            // the crash window the row exists to cover. The snapshot is the
            // FULL render ctx (auth + defaults + input + computed.*), so a
            // resume replays the exact submit-derived context instead of
            // assuming an equivalent rebuild.
            if let Some(env) = async_env.as_ref() {
                crate::db::async_jobs::save_provider_job(
                    env,
                    &runtime.template.id,
                    &job_id,
                    &ctx,
                    source_lang,
                    target_lang,
                )
                .await?;
                crate::db::async_jobs::upsert_polling_job(
                    env,
                    &runtime.template.id,
                    &job_id,
                    &ctx,
                    source_lang,
                    target_lang,
                )
                .await
                .context("persist provider job before polling failed")?;
            }
            job_id
        }
    };
    if let Some(env) = async_env.as_ref() {
        crate::db::async_jobs::save_provider_job(
            env,
            &runtime.template.id,
            &job_id,
            &ctx,
            source_lang,
            target_lang,
        )
        .await?;
    }
    let interval_secs = async_poll.interval_seconds.unwrap_or(5).max(1);
    let timeout_secs = async_poll.timeout_seconds.unwrap_or(600).max(1);
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

    let deadline = PollDeadline::new(timeout_secs, &runtime.template.id)?;
    let mut attempts = 0u64;
    // GAP-04: the loop computes an outcome instead of returning directly so
    // the row closure (close on success / mark failed on error) always runs,
    // including on the timeout and failed-status arms.
    let outcome = loop {
        if let Err(error) = deadline.check(attempts) {
            break Err(error);
        }
        if attempts > 0 {
            if let Err(error) = deadline
                .wait(
                    attempts,
                    tokio::time::sleep(Duration::from_secs(interval_secs)),
                )
                .await
            {
                break Err(error);
            }
        }
        attempts = attempts.saturating_add(1);
        // GAP-04: per-attempt heartbeat — one cheap UPDATE per poll interval
        // gives crash forensics ("how far did the poll get") and keeps the
        // durable attempt history current. Age alone never authorizes deletion.
        if let Some(env) = async_env.as_ref() {
            crate::db::async_jobs::touch_polling_job(
                env,
                prior_attempts.saturating_add(attempts as i64),
            )
            .await
            .context("persist async progress before polling failed")?;
        }

        let poll_json = match deadline
            .wait(
                attempts,
                call_component_request_json(client, runtime, &async_poll.request, &mut ctx),
            )
            .await
        {
            Ok(result) => result?,
            Err(error) => break Err(error),
        };

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
                    break Err(anyhow!(
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
                break super::content_safety::validate_interpolation_tokens(
                    input_text,
                    &translated,
                )
                .map(|()| translated);
            }
            if done_by_status {
                break Err(anyhow!(
                    "component async poll done but output empty (component={})",
                    runtime.template.id
                ));
            }
        } else if done_by_status {
            break Err(anyhow!(
                "component async poll done but output missing (component={})",
                runtime.template.id
            ));
        }
    };

    // Success closes the projection only after saving the paid result.
    // Terminal errors retain 'failed' evidence, never authorize a new submit.
    match &outcome {
        Ok(result) => {
            if let Some(env) = async_env.as_ref() {
                crate::db::async_jobs::save_provider_result(env, serde_json::json!(result)).await?;
                crate::db::async_jobs::close_job(env)
                    .await
                    .context("persist async completion failed")?;
            }
        }
        Err(err) => {
            if let Some(env) = async_env.as_ref() {
                let err_snippet: String = format!("{:#}", err).chars().take(300).collect();
                crate::db::async_jobs::mark_job_failed(env, &err_snippet)
                    .await
                    .context("persist async terminal failure failed")?;
            }
        }
    }
    outcome
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
    translate_non_text_via_component_with_env(
        client,
        runtime,
        source_text,
        source_payload,
        source_ref,
        task_type,
        key,
        source_lang,
        target_lang,
        None,
    )
    .await
}

/// GAP-04 non-text lane (video / document translation — the 06 doc's named
/// cases): same durable-job contract as
/// [`translate_text_via_component_with_env`]. A persistent environment covers
/// synchronous/binary results as well as submit→poll. Evidence-query templates
/// require that environment before any provider egress.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn translate_non_text_via_component_with_env(
    client: &Client,
    runtime: &ComponentRuntime,
    source_text: &str,
    source_payload: Option<&Value>,
    source_ref: &str,
    task_type: &str,
    key: &str,
    source_lang: &str,
    target_lang: &str,
    async_env: Option<crate::db::async_jobs::AsyncJobEnv>,
) -> anyhow::Result<NonTextComponentOutcome> {
    provider_recovery::validate_contract(&runtime.template)?;
    anyhow::ensure!(
        async_env.is_some()
            || runtime
                .template
                .async_poll
                .as_ref()
                .is_none_or(|poll| poll.reconcile.is_none()),
        "provider reconciliation template requires a persistent translation unit"
    );
    let normalized_task_type = normalize_task_content_type(task_type);
    if !template_declares_real_non_text_output(&runtime.template, &normalized_task_type) {
        return Err(anyhow!(
            "component does not declare real non-text output path (task_type={}, component={})",
            normalized_task_type,
            runtime.template.id
        ));
    }

    let async_env = async_env
        .map(|env| {
            env.for_runtime(runtime, &serde_json::json!({
            "source_text": source_text, "source_payload": source_payload, "source_ref": source_ref,
            "task_type": normalized_task_type, "key": key,
        }), source_lang, target_lang)
        })
        .transpose()?;
    if let Some(env) = &async_env {
        if let crate::db::async_jobs::ProviderRecovery::Ready(result) =
            crate::db::async_jobs::recover_provider_operation(env).await?
        {
            return Ok(NonTextComponentOutcome {
                translated_ref: result["translated_ref"].as_str().unwrap().to_string(),
                translated_text: result["translated_text"].as_str().unwrap().to_string(),
            });
        }
    }
    validate_http_limits(&runtime.template)?;
    let async_env = match async_env {
        Some(env) => Some(crate::db::async_jobs::ProviderExecution::acquire(env).await?),
        None => None,
    };
    let resumed_job = match async_env.as_ref() {
        Some(env) => {
            let row = match crate::db::async_jobs::recover_provider_operation(env).await? {
                crate::db::async_jobs::ProviderRecovery::Ready(result) => {
                    return Ok(NonTextComponentOutcome {
                        translated_ref: result["translated_ref"].as_str().unwrap().to_string(),
                        translated_text: result["translated_text"].as_str().unwrap().to_string(),
                    });
                }
                crate::db::async_jobs::ProviderRecovery::Polling(row) => {
                    crate::db::async_jobs::upsert_polling_job(
                        env,
                        &runtime.template.id,
                        &row.job_id,
                        &row.ctx,
                        source_lang,
                        target_lang,
                    )
                    .await?;
                    Some(row)
                }
                crate::db::async_jobs::ProviderRecovery::Vacant => {
                    crate::db::async_jobs::find_polling_job(env).await?
                }
            };
            anyhow::ensure!(
                row.is_none() || runtime.template.async_poll.is_some(),
                "saved provider job requires an async-poll template; retained"
            );
            crate::bindings::encrypt_for_save("{}")?;
            row
        }
        None => None,
    };

    // Source headers are not size authority. Measure once before acquiring
    // credentials, then reuse those bytes for local metadata/encoding/upload.
    // Paid receipts and resumed jobs returned above must not refetch source.
    let verified_source = if resumed_job.is_none() {
        let mut budget = u64::from(
            runtime
                .template
                .constraints
                .as_ref()
                .and_then(|c| c.max_file_size_mb)
                .unwrap_or(0),
        ) * 1024
            * 1024;
        let mut needs_size_check = budget > 0;
        if let Some(pool) = &runtime.key_pool {
            budget =
                crate::component_rt::file_limits::strictest(budget, pool.source_byte_budget()?);
            needs_size_check |= pool.has_file_size_limits();
        }
        if let Some(pool) = &runtime.oauth_pool {
            budget =
                crate::component_rt::file_limits::strictest(budget, pool.source_byte_budget()?);
            needs_size_check |= pool.has_file_size_limits();
        }
        if needs_size_check && !source_ref.trim().is_empty() {
            Some(fetch_source_asset(client, source_ref, budget).await?)
        } else {
            None
        }
    } else {
        None
    };
    let file_size_mb = verified_source
        .as_ref()
        .map(|asset| asset.bytes.len() as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0);

    // Dynamic key selection: if component has a key pool, acquire a key guard.
    // The guard is held for the duration of this translation (RAII concurrency counting).
    // Use select_key_with_limits to enforce max_file_size_mb per-key limits pre-selection.
    let _key_guard;
    let mut ctx = resumed_job
        .as_ref()
        .map(|row| row.ctx.clone())
        .unwrap_or_else(|| runtime.auth_values.clone());

    if let Some(pool) = runtime.key_pool.as_ref().filter(|_| resumed_job.is_none()) {
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
    if let Some((pool, manager)) = runtime
        .oauth_pool
        .as_ref()
        .zip(runtime.oauth_manager.as_ref())
        .filter(|_| resumed_job.is_none())
    {
        manager.assert_proxy_profile(runtime.proxy_profile_id.as_deref())?;
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

    let mut selected_source_budget = u64::from(
        runtime
            .template
            .constraints
            .as_ref()
            .and_then(|c| c.max_file_size_mb)
            .unwrap_or(0),
    ) * 1024
        * 1024;
    for limit in [
        _key_guard.as_ref().map(|guard| guard.max_file_size_mb),
        _oauth_guard.as_ref().map(|guard| guard.max_file_size_mb),
    ]
    .into_iter()
    .flatten()
    {
        selected_source_budget = crate::component_rt::file_limits::strictest(
            selected_source_budget,
            crate::component_rt::file_limits::bytes_from_megabytes(limit)?,
        );
    }
    let source_request = SourceRequest {
        reference: source_ref,
        verified: verified_source.as_ref(),
        byte_budget: selected_source_budget,
    };

    if resumed_job.is_none() {
        insert_component_default_values_context(&mut ctx, runtime.template.default_values.as_ref());
        insert_component_text_context(&mut ctx, source_text, source_lang, target_lang);
        insert_component_non_text_context(&mut ctx, source_payload, source_ref, task_type, key);
        ensure_non_text_source_asset_metadata_context(
            client,
            runtime,
            &mut ctx,
            source_ref,
            verified_source.as_ref(),
        )
        .await?;
        maybe_insert_non_text_source_asset_context(
            client,
            runtime,
            &mut ctx,
            source_ref,
            verified_source.as_ref(),
        )
        .await?;
        apply_language_map(&runtime.language_map, &mut ctx);
    }
    let prior_attempts = resumed_job.as_ref().map(|job| job.attempts).unwrap_or(0);

    if resumed_job.is_none() {
        if let Some(env) = async_env.as_ref() {
            crate::db::async_jobs::begin_provider_runtime_intent(
                env,
                runtime,
                &serde_json::json!({
                    "source_text": source_text, "source_payload": source_payload,
                    "source_ref": source_ref, "task_type": normalized_task_type, "key": key,
                }),
                &mut ctx,
                source_lang,
                target_lang,
            )
            .await?;
        }
    }

    if let Some(prepare) = runtime
        .template
        .prepare
        .as_ref()
        .filter(|_| resumed_job.is_none())
    {
        let prepare_json = call_component_request_json_with_source(
            client,
            runtime,
            &prepare.request,
            &mut ctx,
            Some(source_request),
        )
        .await?;
        apply_prepare_extract(prepare, &prepare_json, &mut ctx, &runtime.template.id)?;
    }

    if let Some(source_upload) = runtime
        .template
        .source_upload
        .as_ref()
        .filter(|_| resumed_job.is_none())
    {
        upload_source_asset_to_vendor(
            client,
            runtime,
            source_upload,
            &mut ctx,
            source_ref,
            verified_source.as_ref(),
        )
        .await?;
    }

    // GAP-04: crash resume (non-text lane) — same contract as the text
    // lane: a surviving 'polling' row means the provider already has (and
    // billed) this job; skip the submit AND the prepare/upload ladder (the
    // persisted ctx snapshot carries their extracted state), restore the
    // snapshot verbatim, keep polling.
    // 1) Submit stage — skipped entirely on resume.
    if resumed_job.is_none() {
        if let Some(env) = async_env.as_ref() {
            crate::db::async_jobs::commit_provider_submit_context(env, &ctx).await?;
        }
    }
    if resumed_job.is_none() && request_expects_binary_response(&runtime.template.request) {
        let asset = call_component_request_binary_submit(
            client,
            runtime,
            &runtime.template.request,
            &mut ctx,
            Some(source_request),
        )
        .await?;
        let preferred_name = asset
            .filename
            .or_else(|| ctx.get("input.source_filename").cloned())
            .filter(|v| !v.trim().is_empty());
        let credit = match async_env.as_ref() {
            Some(env) => crate::db::async_jobs::provider_result_storage_credit(env).await?,
            None => None,
        };
        let local_path = crate::storage_capacity::with_result_credit(credit, async {
            persist_downloaded_provider_asset(&asset.bytes, preferred_name.as_deref())
        })
        .await?;
        let result = finalize_non_text_outcome(
            &ctx,
            runtime,
            &normalized_task_type,
            ExtractedNonTextOutcome {
                translated_ref: format!("file://{}", local_path),
                translated_text: String::new(),
            },
        )?;
        persist_provider_media_result(async_env.as_ref(), &result).await?;
        return Ok(result);
    }

    let submit_json = match &resumed_job {
        Some(row) => {
            ctx = row.ctx.clone();
            serde_json::Value::Null
        }
        None => {
            call_component_request_json_with_source(
                client,
                runtime,
                &runtime.template.request,
                &mut ctx,
                Some(source_request),
            )
            .await?
        }
    };

    // Best-effort sync extraction (some providers may complete immediately).
    // On resume there is no submit response — nothing to extract.
    if resumed_job.is_none() {
        let sync_outcome = extract_non_text_outcome_best_effort(
            runtime,
            &submit_json,
            &normalized_task_type,
            source_ref,
            /*allow_source_fallback*/ runtime.template.async_poll.is_none(),
            None,
        );
        if runtime.template.async_poll.is_none() {
            let result =
                finalize_non_text_outcome(&ctx, runtime, &normalized_task_type, sync_outcome)?;
            persist_provider_media_result(async_env.as_ref(), &result).await?;
            return Ok(result);
        }
        if !sync_outcome.translated_ref.trim().is_empty() {
            let result =
                finalize_non_text_outcome(&ctx, runtime, &normalized_task_type, sync_outcome)?;
            persist_provider_media_result(async_env.as_ref(), &result).await?;
            return Ok(result);
        }
    }

    // 2) Async poll stage
    let async_poll = runtime.template.async_poll.as_ref().expect("checked above");
    let job_id = match resumed_job {
        Some(row) => row.job_id,
        None => {
            let job_id = extract_json_path_string(&submit_json, &async_poll.job_id_path)
                .unwrap_or_default()
                .trim()
                .to_string();
            if job_id.is_empty() {
                return Err(anyhow!(
                    "async_poll.job_id_path '{}' not found in submit response (component={})",
                    async_poll.job_id_path,
                    runtime.template.id
                ));
            }
            ctx.insert("computed.job_id".to_string(), job_id.clone());
            ctx.insert("computed.async_job_id".to_string(), job_id.clone());
            apply_async_poll_submit_extract(
                async_poll,
                &submit_json,
                &mut ctx,
                &runtime.template.id,
            )?;

            // GAP-04: persist BEFORE the first poll (non-text lane).
            if let Some(env) = async_env.as_ref() {
                crate::db::async_jobs::save_provider_job(
                    env,
                    &runtime.template.id,
                    &job_id,
                    &ctx,
                    source_lang,
                    target_lang,
                )
                .await?;
                crate::db::async_jobs::upsert_polling_job(
                    env,
                    &runtime.template.id,
                    &job_id,
                    &ctx,
                    source_lang,
                    target_lang,
                )
                .await
                .context("persist provider job before polling failed")?;
            }
            job_id
        }
    };

    if let Some(env) = async_env.as_ref() {
        crate::db::async_jobs::save_provider_job(
            env,
            &runtime.template.id,
            &job_id,
            &ctx,
            source_lang,
            target_lang,
        )
        .await?;
    }

    let interval_secs = async_poll.interval_seconds.unwrap_or(5).max(1);
    let timeout_secs = async_poll.timeout_seconds.unwrap_or(900).max(1);
    let done_values = normalize_status_values(&async_poll.done_values);
    let failed_values = normalize_status_values(&async_poll.failed_values);
    let status_path = async_poll
        .status_path
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let deadline = PollDeadline::new(timeout_secs, &runtime.template.id)?;
    let mut attempts = 0u64;
    // GAP-04: outcome loop (see the text lane) so the row closure always
    // runs on every exit arm, including finalize errors.
    let outcome = loop {
        if let Err(error) = deadline.check(attempts) {
            break Err(error);
        }
        if attempts > 0 {
            if let Err(error) = deadline
                .wait(
                    attempts,
                    tokio::time::sleep(Duration::from_secs(interval_secs)),
                )
                .await
            {
                break Err(error);
            }
        }
        attempts = attempts.saturating_add(1);
        // GAP-04: per-attempt heartbeat.
        if let Some(env) = async_env.as_ref() {
            crate::db::async_jobs::touch_polling_job(
                env,
                prior_attempts.saturating_add(attempts as i64),
            )
            .await
            .context("persist async progress before polling failed")?;
        }

        let poll_json = match deadline
            .wait(
                attempts,
                call_component_request_json(client, runtime, &async_poll.request, &mut ctx),
            )
            .await
        {
            Ok(result) => result?,
            Err(error) => break Err(error),
        };

        let rendered_status_path =
            status_path.and_then(|path| resolve_template_json_path(path, &ctx));
        let status = rendered_status_path
            .as_deref()
            .and_then(|path| extract_json_path_string(&poll_json, path))
            .map(|v| normalize_status_value(&v))
            .filter(|v| !v.is_empty());

        if let Some(ref s) = status {
            if status_values_contains(&failed_values, s) {
                break Err(anyhow!(
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
            let credit = match async_env.as_ref() {
                Some(env) => crate::db::async_jobs::provider_result_storage_credit(env).await?,
                None => None,
            };
            break match deadline
                .wait(
                    attempts,
                    crate::storage_capacity::with_result_credit(
                        credit,
                        finalize_non_text_async(
                            client,
                            runtime,
                            &mut ctx,
                            &normalized_task_type,
                            source_ref,
                            &poll_json,
                        ),
                    ),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => Err(error),
            };
        }

        if done_by_status {
            break Err(anyhow!(
                "component async poll done but output missing (component={})",
                runtime.template.id
            ));
        }
    };

    // GAP-04 row closure (non-text lane).
    match &outcome {
        Ok(result) => {
            if let Some(env) = async_env.as_ref() {
                persist_provider_media_result(Some(env), result).await?;
                crate::db::async_jobs::close_job(env)
                    .await
                    .context("persist async completion failed")?;
            }
        }
        Err(err) => {
            if let Some(env) = async_env.as_ref() {
                let err_snippet: String = format!("{:#}", err).chars().take(300).collect();
                crate::db::async_jobs::mark_job_failed(env, &err_snippet)
                    .await
                    .context("persist async terminal failure failed")?;
            }
        }
    }
    outcome
}

async fn persist_provider_media_result(
    env: Option<&crate::db::async_jobs::ProviderExecution>,
    result: &NonTextComponentOutcome,
) -> anyhow::Result<()> {
    if let Some(env) = env {
        crate::db::async_jobs::save_provider_result(
            env,
            serde_json::json!({
                "translated_ref": result.translated_ref, "translated_text": result.translated_text,
            }),
        )
        .await?;
    }
    Ok(())
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
    source: Option<SourceRequest<'_>>,
) -> anyhow::Result<BinaryResponseAsset> {
    let budget = HttpBudget::new(
        request_spec.http_limits.as_ref(),
        HttpResponseKind::Binary,
        &runtime.template.id,
    )?;
    budget
        .wait(async {
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
            invoke_component_api_binary_with_source(
                client,
                runtime,
                request_spec,
                &rendered_url,
                ctx,
                sign_result,
                source,
            )
            .await
        })
        .await?
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













pub(crate) mod http_limits;
use self::http_limits::*;
