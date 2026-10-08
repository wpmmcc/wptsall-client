use anyhow::Context;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;

use crate::component_rt::oauth::OAuthHttpClient;
use crate::types::ProxyProfile;

/// NET-01 (ISS gap doc / FM-TIMEOUT, batch H 2026-09-22): the pool builders
/// used to configure no timeout at all, so a proxy that accepts and never
/// responds wedged the component call indefinitely. All pool clients now
/// carry the SAME bounded profile as the worker's own HTTP client
/// (worker.rs: 15s total / 10s connect / 90s pool idle). Env overrides exist
/// for the fault-injection lane (small values shrink the proof window).
fn pool_timeouts() -> (Duration, Duration) {
    fn env_secs(key: &str, default: u64) -> u64 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .unwrap_or(default)
    }
    (
        Duration::from_secs(env_secs("WPTSALL_PROXY_TOTAL_TIMEOUT_SECS", 15)),
        Duration::from_secs(env_secs("WPTSALL_PROXY_CONNECT_TIMEOUT_SECS", 10)),
    )
}

fn apply_timeouts(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let (total, connect) = pool_timeouts();
    builder
        .timeout(total)
        .connect_timeout(connect)
        .pool_idle_timeout(Duration::from_secs(90))
}

#[allow(dead_code)]
pub(crate) struct ProxyClientPool {
    server_client: Client,
    direct_client: Client,
    proxied_clients: HashMap<String, Client>,
    direct_oauth_client: OAuthHttpClient,
    proxied_oauth_clients: HashMap<String, OAuthHttpClient>,
}

#[allow(dead_code)]
impl ProxyClientPool {
    pub(crate) fn new(profiles: &HashMap<String, ProxyProfile>) -> anyhow::Result<Self> {
        Self::build(profiles, true)
    }

    pub(crate) fn from_frozen(profiles: &HashMap<String, ProxyProfile>) -> anyhow::Result<Self> {
        Self::build(profiles, false)
    }

    fn build(
        profiles: &HashMap<String, ProxyProfile>,
        resolve_credentials: bool,
    ) -> anyhow::Result<Self> {
        // N-2 (07 audit, 12 批 A3): redirect policy is set at client build
        // time — reqwest's default silently follows up to 10 hops, sending
        // the (custom auth) headers cross-host on the way. The server-plane
        // client talks to one fixed origin and never legitimately
        // redirects (Policy::none); the provider-plane clients follow
        // same-origin hops only (auth headers stay on the trusted origin).
        let server_client = apply_timeouts(
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none()),
        )
        .build()
        .with_context(|| "build server http client failed")?;
        let direct_client = apply_timeouts(
            Client::builder()
                .no_proxy()
                .redirect(crate::component_rt::runner::provider_redirect_policy()),
        )
        .build()
        .with_context(|| "build direct http client failed")?;
        let direct_oauth_client = OAuthHttpClient::direct()?;

        let mut proxied_clients = HashMap::new();
        let mut proxied_oauth_clients = HashMap::new();
        for (profile_id, profile) in profiles {
            if !profile.enabled {
                continue;
            }
            validate_proxy_profile(profile)
                .with_context(|| format!("invalid proxy profile '{}'", profile_id))?;
            let proxy_url = build_proxy_url(profile);
            let proxy = reqwest::Proxy::all(&proxy_url)
                .with_context(|| format!("build proxy for profile '{}' failed", profile_id))?;

            let username = if resolve_credentials {
                crate::component_rt::runner::resolve_credential_reference(&profile.username)
                    .with_context(|| {
                        format!("resolve proxy username for '{}' failed", profile_id)
                    })?
            } else {
                profile.username.clone()
            };
            let password = if resolve_credentials {
                crate::component_rt::runner::resolve_credential_reference(&profile.password)
                    .with_context(|| {
                        format!("resolve proxy password for '{}' failed", profile_id)
                    })?
            } else {
                profile.password.clone()
            };
            let proxy = if !username.is_empty() {
                proxy.basic_auth(&username, &password)
            } else {
                proxy
            };
            let oauth_client = OAuthHttpClient::proxied(profile_id, proxy.clone())?;

            let client = apply_timeouts(
                Client::builder()
                    .no_proxy()
                    .proxy(proxy)
                    .redirect(crate::component_rt::runner::provider_redirect_policy()),
            )
            .build()
            .with_context(|| format!("build proxied client for '{}' failed", profile_id))?;
            proxied_clients.insert(profile_id.clone(), client);
            proxied_oauth_clients.insert(profile_id.clone(), oauth_client);
        }

        Ok(Self {
            server_client,
            direct_client,
            proxied_clients,
            direct_oauth_client,
            proxied_oauth_clients,
        })
    }

    pub(crate) fn get_client(&self, profile_id: Option<&str>) -> anyhow::Result<&Client> {
        match profile_id {
            Some(id) => self.proxied_clients.get(id).ok_or_else(|| {
                anyhow::anyhow!("configured proxy is unavailable; direct fallback refused")
            }),
            None => Ok(&self.direct_client),
        }
    }

    pub(crate) fn get_server_client(&self) -> &Client {
        &self.server_client
    }

    pub(crate) fn get_oauth_client(
        &self,
        profile_id: Option<&str>,
    ) -> anyhow::Result<&OAuthHttpClient> {
        match profile_id {
            Some(id) => self.proxied_oauth_clients.get(id).ok_or_else(|| {
                anyhow::anyhow!("configured OAuth proxy is unavailable; direct fallback refused")
            }),
            None => Ok(&self.direct_oauth_client),
        }
    }
}

/// Validate a proxy profile before constructing a reqwest client.
///
/// Proxy hosts are operator-selected and may intentionally be loopback (for a
/// local corporate proxy or test fixture), so this is not the provider SSRF
/// policy.  It is a syntax/ambiguity boundary that prevents URL injection,
/// unsupported protocols and port truncation.  The destination provider URL
/// remains subject to the component egress guard.
pub(crate) fn validate_proxy_profile(profile: &ProxyProfile) -> anyhow::Result<()> {
    if !matches!(
        profile.protocol.as_str(),
        "http" | "https" | "socks5" | "socks5h"
    ) {
        anyhow::bail!("proxy protocol must be http, https, socks5 or socks5h");
    }
    let host = profile.host.trim();
    if host.is_empty()
        || host.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        || host.contains("//")
        || host.contains('/')
        || host.contains('@')
        || host.contains('?')
        || host.contains('#')
    {
        anyhow::bail!("proxy host is invalid");
    }
    if profile.port == 0 {
        anyhow::bail!("proxy port must be between 1 and 65535");
    }
    Ok(())
}

#[allow(dead_code)]
fn build_proxy_url(profile: &ProxyProfile) -> String {
    match profile.protocol.as_str() {
        "socks5" => format!("socks5://{}:{}", profile.host, profile.port),
        "socks5h" => format!("socks5h://{}:{}", profile.host, profile.port),
        "https" => format!("https://{}:{}", profile.host, profile.port),
        _ => format!("http://{}:{}", profile.host, profile.port),
    }
}
