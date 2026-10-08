//! All OAuth grant paths share the same bounded, non-disclosing token contract.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct OwnedTokenEndpoint {
    base: String,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for OwnedTokenEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OwnedTokenEndpoint {
    async fn new(status: &'static str, body: String, chunked: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/token", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 8192];
                let n = socket.read(&mut request).await.unwrap();
                assert!(String::from_utf8_lossy(&request[..n]).starts_with("POST "));
                count.fetch_add(1, Ordering::SeqCst);
                let response = if chunked {
                    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",body.len())
                } else {
                    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())
                };
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        Self {
            base,
            requests,
            task,
        }
    }

    fn config(&self) -> OAuthConfig {
        serde_json::from_value(serde_json::json!({
            "vendor_id":"owned","label":"Owned token",
            "grant_type":"client_credentials","token_url":self.base,
            "client_id":"owned-client","client_secret":"owned-mock-client-secret",
        }))
        .unwrap()
    }

    fn manager(&self) -> OAuthTokenManager {
        OAuthTokenManager::from_snapshot(HashMap::new(), OAuthHttpClient::direct().unwrap())
    }
}

async fn token_result(
    endpoint: &OwnedTokenEndpoint,
    config: &OAuthConfig,
    grant: &str,
) -> anyhow::Result<String> {
    let manager = endpoint.manager();
    match grant {
        "client_credentials" => manager
            .fetch_client_credentials_token(config)
            .await
            .map(|v| v.0),
        "jwt_bearer" => manager.fetch_jwt_bearer_token(config).await.map(|v| v.0),
        _ => manager
            .fetch_refresh_token(config, "owned-original-refresh")
            .await
            .map(|v| v.0),
    }
}

#[tokio::test]
async fn oauth_response_contract_all_grants_bound_announced_and_chunked_bodies() {
    for chunked in [false, true] {
        let body = serde_json::json!({
            "access_token":"owned-token","expires_in":3600,
            "padding":"x".repeat(1024*1024),
        })
        .to_string();
        let endpoint = OwnedTokenEndpoint::new("200 OK", body, chunked).await;
        for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
            assert!(
                token_result(&endpoint, &endpoint.config(), grant)
                    .await
                    .is_err(),
                "{grant} must reject an OAuth response larger than 1 MiB"
            );
        }
    }
}

#[tokio::test]
async fn oauth_response_contract_all_grants_hide_upstream_credential_echoes() {
    let body = serde_json::json!({
        "error":"invalid_grant",
        "error_description":"owned-mock-client-secret owned-original-refresh",
    })
    .to_string();
    let endpoint = OwnedTokenEndpoint::new("401 Unauthorized", body, false).await;
    for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
        let error = format!(
            "{:#}",
            token_result(&endpoint, &endpoint.config(), grant)
                .await
                .unwrap_err()
        );
        assert!(
            !error.contains("owned-mock-client-secret")
                && !error.contains("owned-original-refresh"),
            "OAuth errors must not retain upstream credential echoes"
        );
        assert!(
            error.contains("401"),
            "preserve the non-sensitive HTTP status"
        );
    }
}

#[tokio::test]
async fn oauth_response_contract_all_grants_reject_empty_access_token() {
    let endpoint = OwnedTokenEndpoint::new(
        "200 OK",
        r#"{"access_token":"","expires_in":3600}"#.into(),
        false,
    )
    .await;
    for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
        assert!(
            token_result(&endpoint, &endpoint.config(), grant)
                .await
                .is_err(),
            "{grant} must not save an empty Ready token"
        );
    }
}

#[tokio::test]
async fn oauth_response_contract_reserved_identity_parameters_refuse_before_egress() {
    let endpoint = OwnedTokenEndpoint::new(
        "200 OK",
        r#"{"access_token":"owned-token","expires_in":3600}"#.into(),
        false,
    )
    .await;
    for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
        let mut config = endpoint.config();
        config
            .extra_params
            .insert("client_id".into(), "different-client".into());
        assert!(
            token_result(&endpoint, &config, grant).await.is_err(),
            "extra parameters cannot replace the frozen OAuth credential identity"
        );
    }
    assert_eq!(endpoint.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oauth_response_contract_endpoint_userinfo_refuses_before_egress() {
    let endpoint = OwnedTokenEndpoint::new(
        "200 OK",
        r#"{"access_token":"owned-token","expires_in":3600}"#.into(),
        false,
    )
    .await;
    let mut config = endpoint.config();
    config.token_url = config
        .token_url
        .replace("http://", "http://owned-user:owned-password@");
    for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
        assert!(
            token_result(&endpoint, &config, grant).await.is_err(),
            "credentialed OAuth endpoint URLs must fail closed"
        );
    }
    assert_eq!(endpoint.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oauth_response_contract_loader_does_not_forward_token_form_on_redirect() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let oauth_path = root.path().join("oauth.json");
    let _guards = [
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string()),
        crate::db::TestEnvVarGuard::set(
            "WPTSALL_VENDOR_KEYS_FILE",
            root.path().join("keys.json").display().to_string(),
        ),
        crate::db::TestEnvVarGuard::set(
            "WPTSALL_PROXY_PROFILES_FILE",
            root.path().join("proxies.json").display().to_string(),
        ),
        crate::db::TestEnvVarGuard::set(
            "WPTSALL_VENDOR_OAUTH_FILE",
            oauth_path.display().to_string(),
        ),
    ];
    let other = OwnedTokenEndpoint::new(
        "200 OK",
        r#"{"access_token":"owned-token","expires_in":3600}"#.into(),
        false,
    )
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_url = format!("http://{}/token", listener.local_addr().unwrap());
    let redirect_to = other.base.clone();
    let origin = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        socket.read(&mut request).await.unwrap();
        let response = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect_to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let mut config = other.config();
    config.token_url = token_url;
    crate::bindings::save_vendor_oauth(
        oauth_path.to_str().unwrap(),
        &crate::types::VendorOAuthDoc {
            version: 2,
            configs: HashMap::from([("owned".into(), config.clone())]),
        },
    )
    .unwrap();
    let resources = crate::component_rt::loader::load_local_data_resources(
        root.path().to_str().unwrap(),
        root.path().join("owned.log").to_str().unwrap(),
    )
    .unwrap();
    let result = resources
        .oauth_manager
        .fetch_client_credentials_token(&config)
        .await;
    origin.await.unwrap();
    assert_eq!(
        other.requests.load(Ordering::SeqCst),
        0,
        "a token form must never reach the redirected origin"
    );
    assert!(result.is_err());
}

#[tokio::test]
async fn oauth_response_contract_invalid_lifetimes_and_rotation_are_not_ready() {
    for field in [
        r#""expires_in":0"#,
        r#""expires_in":-1"#,
        r#""expires_in":"3600""#,
        r#""expires_in":null"#,
        r#""expires_in":3600,"refresh_token":"""#,
        r#""expires_in":3600,"refresh_token":false"#,
    ] {
        let endpoint = OwnedTokenEndpoint::new(
            "200 OK",
            format!(r#"{{"access_token":"owned-token",{field}}}"#),
            false,
        )
        .await;
        for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
            assert!(token_result(&endpoint, &endpoint.config(), grant)
                .await
                .is_err());
        }
    }
}

#[tokio::test]
async fn oauth_response_contract_transport_failure_does_not_echo_token_url() {
    let endpoint = OwnedTokenEndpoint::new("200 OK", "{}".into(), false).await;
    let mut config = endpoint.config();
    config.token_url = "http://127.0.0.1:0/token?secret=owned-url-secret".into();
    for grant in ["client_credentials", "jwt_bearer", "refresh_token"] {
        let error = format!(
            "{:#}",
            token_result(&endpoint, &config, grant).await.unwrap_err()
        );
        assert!(!error.contains("owned-url-secret") && !error.contains("http://"));
    }
}
