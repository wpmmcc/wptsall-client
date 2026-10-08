//! Owned OAuth transport contracts. No saved bindings or external endpoints.
use super::*;
use crate::component_rt::proxy::ProxyClientPool;
use crate::types::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct OwnedTransportEndpoint {
    base: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for OwnedTransportEndpoint {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl OwnedTransportEndpoint {
    async fn start(redirect: Option<String>, same_origin: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = requests.clone();
        let worker = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                let token_request = request.lines().next().unwrap().contains("/token");
                observed.lock().unwrap().push(request);
                let location = if token_request && same_origin {
                    Some("/redirected".into())
                } else if token_request {
                    redirect.clone()
                } else {
                    None
                };
                let response = match location {
                    Some(location) => format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                    None => {
                        let body = r#"{"access_token":"owned-transport-token","expires_in":3600,"text":"owned translated","asset":{"url":"https://owned.invalid/translated.png"}}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                };
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        Self {
            base,
            requests,
            worker,
        }
    }

    fn config(&self) -> OAuthConfig {
        serde_json::from_value(serde_json::json!({
            "vendor_id":"owned","label":"Owned transport",
            "grant_type":"client_credentials","token_url":format!("{}/token",self.base),
            "client_id":"owned-client","client_secret":"owned-mock-transport-secret"
        }))
        .unwrap()
    }
}

fn runtime(
    endpoint: &OwnedTransportEndpoint,
    client: OAuthHttpClient,
    db: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    kind: &str,
    proxy_id: Option<&str>,
) -> ComponentRuntime {
    ComponentRuntime {
        template: serde_json::from_value(serde_json::json!({
            "id":"owned-transport","name":"Owned transport","version":"1","type":kind,
            "request":{"method":"POST","url":format!("{}/translate",endpoint.base),
                "body_type":"json","body":{"text":"{{input.text}}"},
                "headers":{"Authorization":"Bearer {{auth.access_token}}"}},
            "response":{"translated_text_path":"text","translated_ref_path":"asset.url"}
        }))
        .unwrap(),
        auth_values: HashMap::new(),
        supported_business_lines: vec![],
        language_map: HashMap::new(),
        supported_content_formats: vec!["plain_text".into()],
        supported_formats: vec![],
        key_pool: None,
        oauth_pool: Some(Arc::new(OAuthPool::new(
            vec![OAuthPoolEntry {
                config_id: "owned".into(),
                token_field: "access_token".into(),
                max_concurrent: 1,
                max_input_chars: 0,
                max_file_size_mb: 0.0,
                weight: 1,
                active_count: Arc::new(AtomicUsize::new(0)),
            }],
            KeySelectionStrategy::RoundRobin,
        ))),
        oauth_manager: Some(Arc::new(
            OAuthTokenManager::from_snapshot(
                HashMap::from([("owned".into(), endpoint.config())]),
                client,
            )
            .with_recovery_db(db),
        )),
        proxy_profile_id: proxy_id.map(str::to_owned),
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
    }
}

async fn run_component(client: &reqwest::Client, runtime: &ComponentRuntime) -> bool {
    if runtime.template.kind == "text" {
        crate::component_rt::runner::translate_text_via_component(
            client,
            runtime,
            "owned input",
            "en",
            "zh",
        )
        .await
        .is_ok()
    } else {
        crate::component_rt::runner::translate_non_text_via_component(
            client,
            runtime,
            "owned input",
            None,
            "",
            "image",
            "owned",
            "en",
            "zh",
        )
        .await
        .is_ok()
    }
}

#[tokio::test]
async fn oauth_transport_contract_injected_client_never_replays_any_grant_at_redirect() {
    let other = OwnedTransportEndpoint::start(None, false).await;
    let origin =
        OwnedTransportEndpoint::start(Some(format!("{}/redirected", other.base)), false).await;
    let manager =
        OAuthTokenManager::from_snapshot(HashMap::new(), OAuthHttpClient::direct().unwrap());
    let config = origin.config();
    let mut results = Vec::new();
    results.push(
        manager
            .fetch_client_credentials_token(&config)
            .await
            .is_err(),
    );
    results.push(manager.fetch_jwt_bearer_token(&config).await.is_err());
    results.push(
        manager
            .fetch_refresh_token(&config, "owned-refresh")
            .await
            .is_err(),
    );
    assert_eq!(origin.requests.lock().unwrap().len(), 3);
    assert_eq!(
        other.requests.lock().unwrap().len(),
        0,
        "an injected client must not send token forms to a redirected origin"
    );
    assert!(results.into_iter().all(|failed| failed));
}

#[tokio::test]
async fn oauth_transport_contract_provider_client_cannot_enable_same_origin_token_redirect() {
    let _key = crate::db::owned_mock_bindings_key();
    let endpoint = OwnedTransportEndpoint::start(None, true).await;
    let pool = ProxyClientPool::new(&HashMap::new()).unwrap();
    for kind in ["text", "image"] {
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(":memory:").unwrap(),
        ));
        let runtime = runtime(
            &endpoint,
            OAuthHttpClient::direct().unwrap(),
            db,
            kind,
            None,
        );
        assert!(
            !run_component(pool.get_client(None).unwrap(), &runtime).await,
            "provider same-origin redirect policy must never replace the token transport"
        );
    }
    let requests = endpoint.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "only the original token requests may be sent"
    );
    assert!(requests
        .iter()
        .all(|request| request.lines().next().unwrap().contains("/token")));
}

#[tokio::test]
async fn oauth_transport_contract_selected_proxy_keeps_token_redirect_disabled() {
    let _key = crate::db::owned_mock_bindings_key();
    let proxy = OwnedTransportEndpoint::start(None, true).await;
    let address = url::Url::parse(&proxy.base).unwrap();
    let profiles = HashMap::from([(
        "owned-selected".into(),
        ProxyProfile {
            name: "Owned proxy".into(),
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: address.port().unwrap(),
            username: String::new(),
            password: String::new(),
            enabled: true,
        },
    )]);
    let pool = ProxyClientPool::from_frozen(&profiles).unwrap();
    let endpoint = OwnedTransportEndpoint::start(None, false).await;
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let runtime = runtime(
        &endpoint,
        pool.get_oauth_client(Some("owned-selected"))
            .unwrap()
            .clone(),
        db,
        "text",
        Some("owned-selected"),
    );
    assert!(!run_component(pool.get_client(Some("owned-selected")).unwrap(), &runtime).await);
    assert!(
        endpoint.requests.lock().unwrap().is_empty(),
        "selected proxy must not be bypassed"
    );
    let requests = proxy.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the proxy must observe exactly one original token request"
    );
    assert!(requests[0].lines().next().unwrap().contains("/token"));
    assert!(requests[0].contains("owned-mock-transport-secret"));
}

#[tokio::test]
async fn oauth_transport_contract_missing_selected_proxy_refuses_before_token_issue() {
    let _key = crate::db::owned_mock_bindings_key();
    let endpoint = OwnedTransportEndpoint::start(None, false).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let runtime = runtime(
        &endpoint,
        OAuthHttpClient::direct().unwrap(),
        db,
        "text",
        Some("missing-selected"),
    );
    assert!(!run_component(&client, &runtime).await);
    assert!(
        endpoint.requests.lock().unwrap().is_empty(),
        "a mismatched proxy transport cannot issue a token or fall back to direct"
    );
}

#[tokio::test]
async fn oauth_transport_contract_manual_registry_does_not_inherit_an_arbitrary_http_client() {
    let _key = crate::db::owned_mock_bindings_key();
    let other = OwnedTransportEndpoint::start(None, false).await;
    let endpoint =
        OwnedTransportEndpoint::start(Some(format!("{}/redirected", other.base)), false).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    let runtime = runtime(
        &endpoint,
        OAuthHttpClient::direct().unwrap(),
        db.clone(),
        "text",
        None,
    );
    let registry = ComponentRuntimeRegistry {
        runtimes: HashMap::from([("owned-transport".into(), runtime)]),
        ordered_ids: vec!["owned-transport".into()],
    };
    let plan = crate::db::review_attempts::plan::ManualPlan {
        format: "manual-plan-v1".into(),
        wp_base: "http://127.0.0.1/owned-wp".into(),
        content: serde_json::json!({}),
        relation: serde_json::from_value(serde_json::json!({
            "id":1,"source_lang":"en","target_lang":"zh",
            "target_site_type":"virtual","sync_mode":"translate"
        }))
        .unwrap(),
        rules: vec![],
        runtimes: crate::db::review_attempts::plan::ManualPlan::freeze_runtimes(&registry)
            .await
            .unwrap(),
        ordered_ids: registry.ordered_ids,
        rule_bindings: Default::default(),
        task_type_bindings: Default::default(),
        proxy_profiles: HashMap::new(),
    };
    let frozen = plan.registry(&client, db).unwrap();
    assert!(!run_component(&client, &frozen.runtimes["owned-transport"]).await);
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert!(
        other.requests.lock().unwrap().is_empty(),
        "frozen OAuth cannot inherit a redirecting WP/provider client"
    );
}

#[test]
fn oauth_transport_contract_metadata_floor_applies_before_durable_intent() {
    for endpoint in [
        "http://169.254.169.254/token",
        "http://[::ffff:169.254.169.254]/token",
        "http://[fd00:ec2::254]/token",
        "http://[fe80::1]/token",
        "http://metadata.google.internal/token",
        "http://metadata/token",
    ] {
        let config: OAuthConfig = serde_json::from_value(serde_json::json!({
            "vendor_id":"owned","label":"Owned metadata rejection",
            "grant_type":"client_credentials","token_url":endpoint,"client_id":"owned"
        }))
        .unwrap();
        assert!(
            OAuthTokenManager::validate_token_request(&config).is_err(),
            "cloud metadata must be rejected without sending any request: {endpoint}"
        );
    }
}

#[tokio::test]
async fn oauth_transport_contract_relay_translator_uses_selected_proxy_for_both_requests() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let _web_ui = crate::db::TestEnvVarGuard::set("WPTSALL_WEB_UI", "false");
    let endpoint = OwnedTransportEndpoint::start(None, false).await;
    let proxy = OwnedTransportEndpoint::start(None, false).await;
    let proxy_url = url::Url::parse(&proxy.base).unwrap();
    let template = serde_json::json!({
        "id":"owned-relay","name":"Owned relay","version":"1","type":"text",
        "request":{"method":"POST","url":format!("{}/translate",endpoint.base),
            "body_type":"json","body":{"text":"{{input.text}}"}},
        "response":{"translated_text_path":"text"}
    });
    let local: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "components":{"owned-relay":{
            "vendor_id":"owned","name":"Owned relay","kind":"text","enabled":true,
            "template_id":"owned-relay","template_json":template,"active_version":"v1","created_at":"0",
            "versions":{"v1":{"version":"1","created_at":"0","auth_type":"oauth",
                "proxy_profile_id":"owned-relay-proxy"}}
        }}
    })).unwrap();
    crate::bindings::save_components_local(&crate::config::components_local_file(), &local)
        .unwrap();
    let bindings: ComponentBindingsDoc = serde_json::from_value(serde_json::json!({
        "components":{"owned-relay":{"oauth_ids":["owned"]}}
    }))
    .unwrap();
    crate::bindings::save_component_bindings(&crate::config::component_bindings_file(), &bindings)
        .unwrap();
    crate::bindings::save_vendor_oauth(
        &crate::config::vendor_oauth_file(),
        &VendorOAuthDoc {
            version: 2,
            configs: HashMap::from([("owned".into(), endpoint.config())]),
        },
    )
    .unwrap();
    crate::bindings::save_proxy_profiles(
        &crate::config::proxy_profiles_file(),
        &ProxyProfilesDoc {
            version: 2,
            profiles: HashMap::from([(
                "owned-relay-proxy".into(),
                ProxyProfile {
                    name: "Owned relay proxy".into(),
                    protocol: "http".into(),
                    host: "127.0.0.1".into(),
                    port: proxy_url.port().unwrap(),
                    username: String::new(),
                    password: String::new(),
                    enabled: true,
                },
            )]),
        },
    )
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let translator = crate::sync_engine::build_translator(
        &client,
        "owned-relay",
        root.path().join("owned.log").to_str().unwrap(),
    )
    .await
    .unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let mut runtime = (*translator.runtime).clone();
    runtime.oauth_manager = runtime
        .oauth_manager
        .as_ref()
        .map(|manager| Arc::new((**manager).clone().with_recovery_db(db.clone())));
    let result = crate::component_rt::runner::translate_text_via_component(
        &translator.vendor_client,
        &runtime,
        "owned relay input",
        "en",
        "zh",
    )
    .await;
    assert!(result.is_ok(), "owned relay execution failed: {result:?}");
    assert!(
        endpoint.requests.lock().unwrap().is_empty(),
        "relay must not bypass the configured proxy for token or provider"
    );
    let requests = proxy.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].lines().next().unwrap().contains("/token"));
    assert!(requests[1].lines().next().unwrap().contains("/translate"));
}
