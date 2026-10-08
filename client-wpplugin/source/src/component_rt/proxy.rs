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
#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use serde_json::json;
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::{timeout, Duration};

    use crate::types::{ComponentRequest, ComponentResponse, ComponentRuntime, ComponentTemplate};

    fn credential_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn lock_credential_env() -> std::sync::MutexGuard<'static, ()> {
        credential_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvCleanup(&'static [&'static str]);

    impl Drop for EnvCleanup {
        fn drop(&mut self) {
            for key in self.0 {
                std::env::remove_var(key);
            }
        }
    }

    #[test]
    fn build_proxy_url_http() {
        let profile = ProxyProfile {
            name: "test".into(),
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: 8080,
            username: String::new(),
            password: String::new(),
            enabled: true,
        };
        assert_eq!(build_proxy_url(&profile), "http://127.0.0.1:8080");
    }

    #[tokio::test]
    async fn remaining_http_proxy_also_protects_https_provider_connections() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_addr = destination.local_addr().unwrap();
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy.local_addr().unwrap();
        let proxy_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let direct_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits = proxy_hits.clone();
        let proxy_server = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).starts_with("CONNECT "));
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = socket
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        });
        let hits = direct_hits.clone();
        let direct_server = tokio::spawn(async move {
            let (socket, _) = destination.accept().await.unwrap();
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(socket);
        });
        let profiles = HashMap::from([(
            "owned-http-proxy".into(),
            ProxyProfile {
                name: "owned".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: proxy_addr.port(),
                username: String::new(),
                password: String::new(),
                enabled: true,
            },
        )]);
        let pool = ProxyClientPool::new(&profiles).unwrap();
        let _ = pool
            .get_client(Some("owned-http-proxy"))
            .unwrap()
            .get(format!("https://{destination_addr}/owned"))
            .send()
            .await;
        proxy_server.abort();
        direct_server.abort();
        assert_eq!(
            direct_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "HTTPS provider must not bypass an HTTP proxy"
        );
        assert_eq!(proxy_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn build_proxy_url_socks5() {
        let profile = ProxyProfile {
            name: "test".into(),
            protocol: "socks5".into(),
            host: "proxy.example.com".into(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            enabled: true,
        };
        assert_eq!(build_proxy_url(&profile), "socks5://proxy.example.com:1080");
    }

    #[test]
    fn build_proxy_url_socks5h() {
        let profile = ProxyProfile {
            name: "test".into(),
            protocol: "socks5h".into(),
            host: "proxy.example.com".into(),
            port: 1080,
            username: String::new(),
            password: String::new(),
            enabled: true,
        };
        assert_eq!(
            build_proxy_url(&profile),
            "socks5h://proxy.example.com:1080"
        );
    }

    #[test]
    fn new_empty_profiles() {
        let profiles = HashMap::new();
        let pool = ProxyClientPool::new(&profiles).unwrap();
        // Should have server and direct clients, no proxied
        assert!(pool.proxied_clients.is_empty());
    }

    #[test]
    fn get_client_returns_direct_for_none() {
        let pool = ProxyClientPool::new(&HashMap::new()).unwrap();
        let _client = pool.get_client(None);
    }

    #[test]
    fn get_client_refuses_unknown_profile() {
        let pool = ProxyClientPool::new(&HashMap::new()).unwrap();
        assert!(pool.get_client(Some("nonexistent")).is_err());
    }

    #[test]
    fn get_server_client_works() {
        let pool = ProxyClientPool::new(&HashMap::new()).unwrap();
        let _client = pool.get_server_client();
    }

    #[test]
    fn disabled_profile_skipped() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "disabled".to_string(),
            ProxyProfile {
                name: "disabled".into(),
                protocol: "http".into(),
                host: "127.0.0.1".into(),
                port: 8080,
                username: String::new(),
                password: String::new(),
                enabled: false,
            },
        );
        let pool = ProxyClientPool::new(&profiles).unwrap();
        assert!(pool.proxied_clients.is_empty());
    }

    #[test]
    fn proxy_profile_validation_rejects_ambiguous_endpoint() {
        let profile = ProxyProfile {
            name: "bad".into(),
            protocol: "ftp".into(),
            host: "proxy.example.com".into(),
            port: 8080,
            username: String::new(),
            password: String::new(),
            enabled: true,
        };
        assert!(validate_proxy_profile(&profile).is_err());
    }

    #[test]
    fn proxy_pool_resolves_credential_references_without_mutating_profile() {
        let _lock = lock_credential_env();
        let _env_cleanup = EnvCleanup(&["WPTSALL_PROXY_USERNAME", "WPTSALL_PROXY_PASSWORD"]);
        std::env::set_var("WPTSALL_PROXY_USERNAME", "env-user");
        std::env::set_var("WPTSALL_PROXY_PASSWORD", "env-password");

        let profile = ProxyProfile {
            name: "references".into(),
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: 8080,
            username: "env://WPTSALL_PROXY_USERNAME".into(),
            password: "env://WPTSALL_PROXY_PASSWORD".into(),
            enabled: true,
        };
        let mut profiles = HashMap::new();
        profiles.insert("refs".to_string(), profile.clone());
        let pool = ProxyClientPool::new(&profiles).expect("proxy client should build");
        assert!(pool.proxied_clients.contains_key("refs"));
        assert_eq!(profiles["refs"].username, "env://WPTSALL_PROXY_USERNAME");
        assert_eq!(profiles["refs"].password, "env://WPTSALL_PROXY_PASSWORD");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn proxy_pool_client_sends_component_request_through_http_proxy() {
        let _lock = lock_credential_env();
        let _env_cleanup = EnvCleanup(&[
            "WPTSALL_PROXY_USERNAME",
            "WPTSALL_PROXY_PASSWORD",
            "WPTSALL_PROVIDER_ALLOWLIST",
        ]);
        std::env::set_var("WPTSALL_PROXY_USERNAME", "env-user");
        std::env::set_var("WPTSALL_PROXY_PASSWORD", "env-password");
        std::env::set_var("WPTSALL_PROVIDER_ALLOWLIST", "1.1.1.1");

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind proxy fixture");
        let proxy_addr = listener.local_addr().expect("proxy fixture addr");
        let (request_tx, request_rx) = oneshot::channel::<String>();

        let proxy_handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("proxy accept");
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 4096];
                let read = timeout(Duration::from_secs(5), socket.read(&mut chunk))
                    .await
                    .expect("proxy read timed out")
                    .expect("proxy read failed");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request_is_complete(&request) {
                    break;
                }
            }

            let captured = String::from_utf8_lossy(&request).to_string();
            let _ = request_tx.send(captured);

            let body = br#"{"data":{"translated":"via proxy"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("proxy response write");
        });

        let profile = ProxyProfile {
            name: "local test proxy".into(),
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: proxy_addr.port(),
            username: "env://WPTSALL_PROXY_USERNAME".into(),
            password: "env://WPTSALL_PROXY_PASSWORD".into(),
            enabled: true,
        };
        let mut profiles = HashMap::new();
        profiles.insert("http-proxy".to_string(), profile);
        let pool = ProxyClientPool::new(&profiles).expect("proxy pool should build");
        let runtime = proxy_translation_runtime();

        let translated = crate::component_rt::runner::translate_text_via_component(
            pool.get_client(Some("http-proxy")).unwrap(),
            &runtime,
            "Hello",
            "en",
            "zh",
        )
        .await
        .expect("component request through proxy should succeed");

        assert_eq!(translated, "via proxy");
        let captured = request_rx.await.expect("proxy captured request");
        assert!(
            captured.starts_with("POST http://1.1.1.1/translate HTTP/1.1\r\n"),
            "proxy should receive an absolute-form HTTP request, got: {}",
            captured.lines().next().unwrap_or("")
        );
        let expected_auth = format!(
            "proxy-authorization: basic {}",
            BASE64_STANDARD.encode("env-user:env-password")
        )
        .to_ascii_lowercase();
        let captured_lower = captured.to_ascii_lowercase();
        assert!(
            captured_lower.contains(&expected_auth),
            "proxy request should include resolved credential reference auth header"
        );
        assert!(
            captured.contains("\"text\":\"Hello\""),
            "component JSON body should reach the proxy"
        );

        proxy_handle.await.expect("proxy task should finish");
    }

    // FM-TIMEOUT — BUG-PROXYCLIENTPOOL-NO-TIMEOUT FIXED (NET-01 product
    // lane, batch H 2026-09-22): every pool client now carries the worker
    // HTTP profile (15s total / 10s connect default; env-overridable for
    // this lane). INVERTED pin: a proxy that accepts and never responds
    // must now produce a bounded timeout ERROR instead of wedging the call
    // in flight — with the total timeout shrunk to 1s, the call resolves
    // (Err) well inside the 3s observation window and the worker can move
    // on to the next item.
    #[tokio::test(flavor = "current_thread")]
    async fn proxy_pool_hanging_proxy_fails_bounded() {
        let _lock = lock_credential_env();
        let _env_cleanup = EnvCleanup(&[
            "WPTSALL_PROVIDER_ALLOWLIST",
            "WPTSALL_PROXY_TOTAL_TIMEOUT_SECS",
        ]);
        std::env::set_var("WPTSALL_PROVIDER_ALLOWLIST", "1.1.1.1");
        std::env::set_var("WPTSALL_PROXY_TOTAL_TIMEOUT_SECS", "1");

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind hanging proxy");
        let proxy_addr = listener.local_addr().expect("hanging proxy addr");
        let hold = tokio::spawn(async move {
            // Accept and hold the socket open without ever responding.
            let _socket = listener.accept().await.expect("hanging proxy accept");
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let profile = ProxyProfile {
            name: "hanging".into(),
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: proxy_addr.port(),
            username: String::new(),
            password: String::new(),
            enabled: true,
        };
        let mut profiles = HashMap::new();
        profiles.insert("hanging".to_string(), profile);
        let pool = ProxyClientPool::new(&profiles).expect("proxy pool should build");
        let runtime = proxy_translation_runtime();

        let call = crate::component_rt::runner::translate_text_via_component(
            pool.get_client(Some("hanging")).unwrap(),
            &runtime,
            "Hello",
            "en",
            "zh",
        );
        let outcome = timeout(Duration::from_secs(3), call).await;
        hold.abort();
        let bounded = outcome.unwrap_or_else(|_| {
            panic!(
                "FM-TIMEOUT regression: the hanging-proxy call is STILL unbounded — \
                 the pool client lost its timeout (NET-01, batch H)"
            )
        });
        assert!(
            bounded.is_err(),
            "the bounded call must surface a timeout ERROR, not a success, got: {:?}",
            bounded.as_ref().ok()
        );
    }

    fn request_is_complete(request: &[u8]) -> bool {
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let header_text = String::from_utf8_lossy(&request[..header_end + 4]);
        let content_length = header_text.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        });
        match content_length {
            Some(length) => request.len() >= header_end + 4 + length,
            None => true,
        }
    }

    fn proxy_translation_runtime() -> ComponentRuntime {
        let template = ComponentTemplate {
            id: "proxy-e2e".to_string(),
            name: "Proxy E2E".to_string(),
            version: "1.0.0".to_string(),
            kind: "text_translation".to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                http_limits: None,
                method: "POST".to_string(),
                url: "http://1.1.1.1/translate".to_string(),
                headers: None,
                body: Some(json!({
                    "text": "{{input.text}}",
                    "source": "{{input.source_lang}}",
                    "target": "{{input.target_lang}}"
                })),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: Some("data.translated".to_string()),
                error_path: Some("error.message".to_string()),
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: None,
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: Vec::new(),
            translation_modes: Vec::new(),
        };

        ComponentRuntime {
            template,
            auth_values: HashMap::new(),
            supported_business_lines: Vec::new(),
            language_map: HashMap::new(),
            supported_content_formats: Vec::new(),
            supported_formats: Vec::new(),
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
            proxy_profile_id: None,
        }
    }

    // -------------------------------------------------------------------
    // N-2 redirect policy (07 audit, 12 批 A3): provider-plane clients
    // follow same-origin hops only; cross-origin hops are refused before
    // any bytes (auth headers never travel cross-host). The server-plane
    // client never follows anything.
    // -------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread")]
    async fn provider_client_refuses_cross_origin_redirect() {
        let pool = ProxyClientPool::new(&HashMap::new()).expect("build pool");

        let origin = TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
        let origin_addr = origin.local_addr().unwrap();
        let cross = TcpListener::bind("127.0.0.1:0").await.expect("bind cross");
        let cross_addr = cross.local_addr().unwrap();
        let cross_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cross_hits_clone = cross_hits.clone();
        let cross_srv = tokio::spawn(async move {
            // If the policy fails, reqwest would connect here; we only
            // count, never answer, so a failure also hangs the client
            // instead of silently succeeding.
            if let Ok((mut socket, _)) = cross.accept().await {
                cross_hits_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut tmp = [0u8; 512];
                let _ = socket.read(&mut tmp).await;
            }
        });
        let origin_srv = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.expect("origin accept");
            let mut tmp = [0u8; 1024];
            let _ = socket.read(&mut tmp).await;
            let body = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{cross_addr}/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(body.as_bytes()).await.expect("write 302");
        });

        let response = pool
            .get_client(None)
            .unwrap()
            .get(format!("http://{origin_addr}/start"))
            .send()
            .await
            .expect("origin request should complete");
        assert!(
            response.status().is_redirection(),
            "cross-origin redirect must surface as a 30x, got {}",
            response.status()
        );
        // Give the (must-not-happen) cross connection a moment, then verify
        // the cross host never saw a request.
        tokio::time::sleep(Duration::from_millis(150)).await;
        cross_srv.abort();
        assert_eq!(
            cross_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the cross-origin host must never receive a connection"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_client_follows_same_origin_redirect() {
        let pool = ProxyClientPool::new(&HashMap::new()).expect("build pool");

        let origin = TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
        let origin_addr = origin.local_addr().unwrap();
        let origin_srv = tokio::spawn(async move {
            while let Ok((mut socket, _)) = origin.accept().await {
                let mut tmp = [0u8; 1024];
                let n = socket.read(&mut tmp).await.unwrap_or(0);
                let request_path = String::from_utf8_lossy(&tmp[..n.max(1)]).to_string();
                let body = if request_path.contains("/first") {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{origin_addr}/second\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 19\r\nConnection: close\r\n\r\n{\"data\":\"followed\"}"
                        .to_string()
                };
                socket
                    .write_all(body.as_bytes())
                    .await
                    .expect("write response");
            }
        });

        let response = pool
            .get_client(None)
            .unwrap()
            .get(format!("http://{origin_addr}/first"))
            .send()
            .await
            .expect("same-origin chain should complete");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = response.json().await.expect("json body");
        assert_eq!(body["data"], json!("followed"));
        // The while-accept server never exits by design; detach it.
        origin_srv.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn server_client_never_follows_any_redirect() {
        let pool = ProxyClientPool::new(&HashMap::new()).expect("build pool");

        let origin = TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
        let origin_addr = origin.local_addr().unwrap();
        let origin_srv = tokio::spawn(async move {
            let (mut socket, _) = origin.accept().await.expect("origin accept");
            let mut tmp = [0u8; 1024];
            let _ = socket.read(&mut tmp).await;
            let body = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{origin_addr}/second\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(body.as_bytes()).await.expect("write 302");
        });

        let response = pool
            .get_server_client()
            .get(format!("http://{origin_addr}/first"))
            .send()
            .await
            .expect("request should complete");
        assert!(
            response.status().is_redirection(),
            "server-plane client must not follow even same-origin redirects, got {}",
            response.status()
        );
        origin_srv.await.expect("origin server task");
    }
}