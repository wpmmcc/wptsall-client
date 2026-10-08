//! Owned integration entry points use the token transport contract too.
use super::*;

struct OwnedRouteTokenEndpoint {
    base: String,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for OwnedRouteTokenEndpoint {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl OwnedRouteTokenEndpoint {
    async fn start(body: String, redirect: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = requests.clone();
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
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = if let Some(location) = &redirect {
                    format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())
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
}

#[tokio::test]
async fn oauth_transport_route_client_credentials_does_not_use_injected_redirecting_state_client() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let body = r#"{"access_token":"owned-route-token","expires_in":3600}"#.to_owned();
    let other = OwnedRouteTokenEndpoint::start(body.clone(), None).await;
    let origin = OwnedRouteTokenEndpoint::start(body, Some(format!("{}/token", other.base))).await;
    let mut config = make_test_oauth_config(format!("{}/token", origin.base));
    config.grant_type = "client_credentials".into();
    crate::bindings::save_vendor_oauth(
        path.to_str().unwrap(),
        &VendorOAuthDoc {
            version: 2,
            configs: HashMap::from([("owned".into(), config)]),
        },
    )
    .unwrap();
    let state = build_test_web_ui_state("", None);
    let response = authorize_integration_fixture(&state, "owned").await;
    assert_eq!(origin.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        other.requests.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "integration token forms cannot inherit the state client's redirects"
    );
    assert!(!response.starts_with("HTTP/1.1 200"));
    let replay = authorize_integration_fixture(&state, "owned").await;
    assert!(!replay.starts_with("HTTP/1.1 200"));
    assert_eq!(origin.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oauth_transport_route_authorization_code_rejects_invalid_lifetime_rotation_and_header() {
    let _env = components_env_lock().lock().unwrap();
    for body in [
        r#"{"access_token":"owned-token","expires_in":"3600"}"#,
        r#"{"access_token":"owned-token","expires_in":null}"#,
        r#"{"access_token":"owned-token","expires_in":3600,"refresh_token":false}"#,
        r#"{"access_token":"owned-token","expires_in":3600,"refresh_token":""}"#,
        r#"{"access_token":"owned\nbad","expires_in":3600}"#,
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-oauth.json");
        let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
        let endpoint = OwnedRouteTokenEndpoint::start(body.into(), None).await;
        let state = build_test_web_ui_state("", None);
        seed_vendor_oauth_callback_state(
            &state,
            path.to_str().unwrap(),
            "owned",
            "owned-state",
            make_test_oauth_config(format!("{}/token", endpoint.base)),
        )
        .await;
        let original = std::fs::read(&path).unwrap();
        let response = callback_integration_fixture(&state, "owned-state").await;
        assert!(
            !response.starts_with("HTTP/1.1 200"),
            "invalid token receipt must remain unresolved: {body}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let retry = callback_integration_fixture(&state, "owned-state").await;
        assert!(!retry.starts_with("HTTP/1.1 200"));
        assert_eq!(
            endpoint.requests.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}

#[tokio::test]
async fn oauth_transport_route_authorization_start_rejects_metadata_without_creating_pending() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let config = make_test_oauth_config("http://169.254.169.254/token".into());
    crate::bindings::save_vendor_oauth(
        path.to_str().unwrap(),
        &VendorOAuthDoc {
            version: 2,
            configs: HashMap::from([("owned".into(), config)]),
        },
    )
    .unwrap();
    let state = build_test_web_ui_state("", None);
    let response = authorize_integration_fixture(&state, "owned").await;
    assert!(
        !response.starts_with("HTTP/1.1 200"),
        "start is read/plan-only: metadata must be refused without making a token request"
    );
    assert!(state.lock().await.vendor_oauth_pending.is_empty());
    let db = state.lock().await.db.clone();
    let count: i64 = db
        .lock()
        .await
        .query_row(
            "SELECT COUNT(*) FROM system_config WHERE key LIKE 'oauth-authorization-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn oauth_transport_route_authorization_scope_and_identity_cannot_be_shadowed() {
    let _env = components_env_lock().lock().unwrap();
    for (auth_extra, token_extra) in [
        (("ScOpE", "owned-other"), None),
        (("CLIENT_ID", "owned-other"), None),
        (("prompt", "consent"), Some(("CLIENT_ID", "owned-other"))),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-oauth.json");
        let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
        let mut config = make_test_oauth_config("http://127.0.0.1:1/token".into());
        config
            .auth_extra_params
            .insert(auth_extra.0.into(), auth_extra.1.into());
        if let Some((key, value)) = token_extra {
            config.extra_params.insert(key.into(), value.into());
        }
        crate::bindings::save_vendor_oauth(
            path.to_str().unwrap(),
            &VendorOAuthDoc {
                version: 2,
                configs: HashMap::from([("owned".into(), config)]),
            },
        )
        .unwrap();
        let state = build_test_web_ui_state("", None);
        let response = authorize_integration_fixture(&state, "owned").await;
        assert!(!response.starts_with("HTTP/1.1 200"));
        assert!(state.lock().await.vendor_oauth_pending.is_empty());
    }
}
