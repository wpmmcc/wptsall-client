use anyhow::Context;
use reqwest::Client;
use std::collections::HashMap;

use crate::types::ProxyProfile;

#[allow(dead_code)]
pub(crate) struct ProxyClientPool {
    server_client: Client,
    direct_client: Client,
    proxied_clients: HashMap<String, Client>,
}

#[allow(dead_code)]
impl ProxyClientPool {
    pub(crate) fn new(profiles: &HashMap<String, ProxyProfile>) -> anyhow::Result<Self> {
        let server_client = Client::builder()
            .no_proxy()
            .build()
            .with_context(|| "build server http client failed")?;
        let direct_client = Client::builder()
            .no_proxy()
            .build()
            .with_context(|| "build direct http client failed")?;

        let mut proxied_clients = HashMap::new();
        for (profile_id, profile) in profiles {
            if !profile.enabled {
                continue;
            }
            validate_proxy_profile(profile)
                .with_context(|| format!("invalid proxy profile '{}'", profile_id))?;
            let proxy_url = build_proxy_url(profile);
            let proxy = match profile.protocol.as_str() {
                "http" => reqwest::Proxy::http(&proxy_url),
                "https" => reqwest::Proxy::https(&proxy_url),
                "socks5" | "socks5h" => reqwest::Proxy::all(&proxy_url),
                _ => reqwest::Proxy::all(&proxy_url),
            }
            .with_context(|| format!("build proxy for profile '{}' failed", profile_id))?;

            let username =
                crate::component_rt::runner::resolve_credential_reference(&profile.username)
                    .with_context(|| {
                        format!("resolve proxy username for '{}' failed", profile_id)
                    })?;
            let password =
                crate::component_rt::runner::resolve_credential_reference(&profile.password)
                    .with_context(|| {
                        format!("resolve proxy password for '{}' failed", profile_id)
                    })?;
            let proxy = if !username.is_empty() {
                proxy.basic_auth(&username, &password)
            } else {
                proxy
            };

            let client = Client::builder()
                .proxy(proxy)
                .build()
                .with_context(|| format!("build proxied client for '{}' failed", profile_id))?;
            proxied_clients.insert(profile_id.clone(), client);
        }

        Ok(Self {
            server_client,
            direct_client,
            proxied_clients,
        })
    }

    pub(crate) fn get_client(&self, profile_id: Option<&str>) -> &Client {
        match profile_id {
            Some(id) => self.proxied_clients.get(id).unwrap_or(&self.direct_client),
            None => &self.direct_client,
        }
    }

    pub(crate) fn get_server_client(&self) -> &Client {
        &self.server_client
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
    use std::sync::{Mutex, OnceLock};
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
    fn get_client_returns_direct_for_unknown_profile() {
        let pool = ProxyClientPool::new(&HashMap::new()).unwrap();
        let _client = pool.get_client(Some("nonexistent"));
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
            pool.get_client(Some("http-proxy")),
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

    // FM-TIMEOUT known_red pin — BUG-PROXYCLIENTPOOL-NO-TIMEOUT (plan §7
    // TEST-NETWORK-RESILIENCE-001; gap doc NET-01). ProxyClientPool builders
    // configure no request/connect timeout, so a proxy that accepts and
    // never responds wedges the component call indefinitely. Pin: after 3s
    // the call is still in flight. When the product lane adds bounded
    // timeouts this pin FAILS — flip it to assert bounded completion and
    // move FM-TIMEOUT to covered.
    #[tokio::test(flavor = "current_thread")]
    async fn proxy_pool_hanging_proxy_pin_known_red_no_timeout() {
        let _lock = lock_credential_env();
        let _env_cleanup = EnvCleanup(&["WPTSALL_PROVIDER_ALLOWLIST"]);
        std::env::set_var("WPTSALL_PROVIDER_ALLOWLIST", "1.1.1.1");

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
            pool.get_client(Some("hanging")),
            &runtime,
            "Hello",
            "en",
            "zh",
        );
        let outcome = timeout(Duration::from_secs(3), call).await;
        hold.abort();
        assert!(
            outcome.is_err(),
            "KNOWN-RED pin (BUG-PROXYCLIENTPOOL-NO-TIMEOUT / FM-TIMEOUT): the hanging-proxy call is still in flight after 3s; if this fails the product lane added a bounded timeout — flip the pin to assert bounded completion"
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
}
