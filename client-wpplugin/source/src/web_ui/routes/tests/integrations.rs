use super::*;
use crate::web_ui::routes::integrations::oauth_flow;
use std::collections::HashMap;

#[path = "../../../../../../tests/modules/client-wpplugin/unit/oauth_transport_routes.rs"]
mod transport;

fn temp_path(prefix: &str, extension: &str) -> String {
    format!("/tmp/{}-{}.{}", prefix, uuid::Uuid::new_v4(), extension)
}

fn make_test_oauth_config(token_url: String) -> OAuthConfig {
    OAuthConfig {
        vendor_id: "google".to_string(),
        label: "Google OAuth".to_string(),
        grant_type: "authorization_code".to_string(),
        auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
        token_url,
        client_id: "client-123".to_string(),
        client_secret: "secret-xyz".to_string(),
        scopes: "translate profile".to_string(),
        extra_params: HashMap::new(),
        auth_extra_params: HashMap::new(),
        cached_token: None,
        cached_token_expires_at: 0,
        refresh_token: None,
        max_concurrent: 5,
        weight: 1,
        max_input_chars: 0,
        max_file_size_mb: 0.0,
        token_field: "access_token".to_string(),
    }
}

async fn seed_vendor_oauth_callback_state(
    state: &Arc<Mutex<WebUiState>>,
    vendor_oauth_path: &str,
    config_id: &str,
    oauth_state: &str,
    config: OAuthConfig,
) {
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert(config_id.to_string(), config.clone());
    crate::bindings::save_vendor_oauth(vendor_oauth_path, &doc).unwrap();
    let verifier = "owned-verifier".repeat(4);
    oauth_flow::seed(state, oauth_state, config_id, &config, &verifier)
        .await
        .unwrap();
    let mut guard = state.lock().await;
    guard.vendor_oauth_pending.insert(
        oauth_state.to_string(),
        VendorOAuthPendingEntry {
            config_id: config_id.to_string(),
            code_verifier: verifier,
            expires_at: unix_ts() as i64 + 600,
        },
    );
}

#[tokio::test]
async fn vendor_key_create_persists_and_duplicate_id_conflicts() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_keys_path = temp_path("test-vendor-keys", "json");
    let _vendor_keys_guard = EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", vendor_keys_path.clone());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let body = serde_json::to_vec(&json!({
        "id": "openai-main",
        "vendor_id": "openai",
        "label": "OpenAI Main",
        "auth_values": { "api_key": "sk-live-test" },
        "max_concurrent": 8,
        "requests_per_second": 4.5,
        "enabled": true
    }))
    .unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_vendor_key_create(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(payload["data"]["id"], json!("openai-main"));

    let saved_doc = crate::bindings::load_vendor_keys(&vendor_keys_path).unwrap();
    let saved_key = saved_doc
        .keys
        .get("openai-main")
        .expect("saved vendor key entry");
    assert_eq!(saved_key.vendor_id, "openai");
    assert_eq!(
        saved_key.auth_values.get("api_key"),
        Some(&"sk-live-test".to_string())
    );

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_vendor_key_create(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 409 Conflict"));
    assert_eq!(payload["error"]["code"], json!("DUPLICATE_ID"));

    let _ = std::fs::remove_file(&vendor_keys_path);
}

#[tokio::test]
async fn oauth_config_create_persists_and_authorize_returns_pkce_url() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let _web_ui_port_guard = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "8999".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let create_body = serde_json::to_vec(&json!({
        "id": "google-oauth",
        "vendor_id": "google",
        "label": "Google OAuth",
        "grant_type": "authorization_code",
        "auth_url": "https://accounts.google.com/o/oauth2/v2/auth",
        "token_url": "https://oauth2.googleapis.com/token",
        "client_id": "client-123",
        "client_secret": "secret-xyz",
        "scopes": "translate profile",
        "auth_extra_params": { "access_type": "offline" }
    }))
    .unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_oauth_config_create(&mut socket, &state, &create_body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(payload["data"]["id"], json!("google-oauth"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_oauth_authorize(&mut socket, &state, "google-oauth")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    let authorize_url = payload["data"]["authorize_url"]
        .as_str()
        .expect("authorize url string");
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(
        authorize_url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
        "unexpected authorize_url: {}",
        authorize_url
    );
    assert!(authorize_url.contains("client_id=client-123"));
    assert!(authorize_url.contains("response_type=code"));
    assert!(authorize_url
        .contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8999%2Foauth%2Fvendor%2Fcallback"));
    assert!(authorize_url.contains("scope=translate%20profile"));
    assert!(authorize_url.contains("access_type=offline"));
    assert!(authorize_url.contains("state="));
    assert!(authorize_url.contains("code_challenge="));
    assert_eq!(
        payload["data"]["callback_url"],
        json!("http://127.0.0.1:8999/oauth/vendor/callback")
    );

    let guard = state.lock().await;
    assert_eq!(guard.vendor_oauth_pending.len(), 1);
    let pending = guard
        .vendor_oauth_pending
        .values()
        .next()
        .expect("pending oauth entry");
    assert_eq!(pending.config_id, "google-oauth");
    assert!(!pending.code_verifier.is_empty());
    drop(guard);

    let saved_doc = crate::bindings::load_vendor_oauth(&vendor_oauth_path).unwrap();
    assert!(saved_doc.configs.contains_key("google-oauth"));

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn oauth_authorize_uses_bind_port_when_port_env_missing() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth-bind-port", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let _web_ui_bind_guard = EnvVarGuard::set("WPTSALL_WEB_UI_BIND", "127.0.0.1:9011".to_string());
    let _web_ui_port_guard = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", String::new());

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert(
        "google-oauth".to_string(),
        make_test_oauth_config("https://oauth2.googleapis.com/token".to_string()),
    );
    crate::bindings::save_vendor_oauth(&vendor_oauth_path, &doc).unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_oauth_authorize(&mut socket, &state, "google-oauth")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    let authorize_url = payload["data"]["authorize_url"]
        .as_str()
        .expect("authorize url string");
    assert!(
        authorize_url
            .contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9011%2Foauth%2Fvendor%2Fcallback"),
        "unexpected authorize_url: {}",
        authorize_url
    );
    assert_eq!(
        payload["data"]["callback_url"],
        json!("http://127.0.0.1:9011/oauth/vendor/callback")
    );

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn oauth_authorize_port_env_overrides_bind_port() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth-port-override", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let _web_ui_bind_guard = EnvVarGuard::set("WPTSALL_WEB_UI_BIND", "127.0.0.1:9011".to_string());
    let _web_ui_port_guard = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "9012".to_string());

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert(
        "google-oauth".to_string(),
        make_test_oauth_config("https://oauth2.googleapis.com/token".to_string()),
    );
    crate::bindings::save_vendor_oauth(&vendor_oauth_path, &doc).unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_oauth_authorize(&mut socket, &state, "google-oauth")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    let authorize_url = payload["data"]["authorize_url"]
        .as_str()
        .expect("authorize url string");
    assert!(
        authorize_url
            .contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A9012%2Foauth%2Fvendor%2Fcallback"),
        "unexpected authorize_url: {}",
        authorize_url
    );
    assert_eq!(
        payload["data"]["callback_url"],
        json!("http://127.0.0.1:9012/oauth/vendor/callback")
    );

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn oauth_authorize_rejects_missing_auth_url_for_authorization_code() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert(
        "broken-auth-code".to_string(),
        OAuthConfig {
            vendor_id: "google".to_string(),
            label: "Broken Auth Code".to_string(),
            grant_type: "authorization_code".to_string(),
            auth_url: String::new(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: None,
            cached_token_expires_at: 0,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".to_string(),
        },
    );
    crate::bindings::save_vendor_oauth(&vendor_oauth_path, &doc).unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_oauth_authorize(&mut socket, &state, "broken-auth-code")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(payload["error"]["code"], json!("MISSING_AUTH_URL"));

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn proxy_create_rejects_missing_host() {
    let _env_guard = components_env_lock().lock().unwrap();
    let proxy_profiles_path = temp_path("test-proxy-profiles", "json");
    let _proxy_profiles_guard =
        EnvVarGuard::set("WPTSALL_PROXY_PROFILES_FILE", proxy_profiles_path.clone());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_proxy_create(
        &mut socket,
        &state,
        br#"{"id":"proxy-a","protocol":"http","host":"   "}"#,
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(payload["error"]["code"], json!("INVALID_HOST"));

    let _ = std::fs::remove_file(&proxy_profiles_path);
}

#[tokio::test]
async fn proxy_test_returns_structured_failure_for_unreachable_profile() {
    let _env_guard = components_env_lock().lock().unwrap();
    let proxy_profiles_path = temp_path("test-proxy-profiles", "json");
    let _proxy_profiles_guard =
        EnvVarGuard::set("WPTSALL_PROXY_PROFILES_FILE", proxy_profiles_path.clone());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let mut doc = ProxyProfilesDoc::default();
    doc.profiles.insert(
        "proxy-dead".to_string(),
        ProxyProfile {
            name: "Dead Proxy".to_string(),
            protocol: "http".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1,
            username: String::new(),
            password: String::new(),
            enabled: true,
        },
    );
    crate::bindings::save_proxy_profiles(&proxy_profiles_path, &doc).unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_proxy_test(&mut socket, &state, "proxy-dead")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "proxy test should surface structured failure payload: {}",
        response
    );
    assert_eq!(payload["success"], json!(false));
    assert_eq!(payload["error"]["code"], json!("PROXY_TEST_FAILED"));
    assert_eq!(payload["data"]["reachable"], json!(false));
    assert_eq!(payload["data"]["proxy_url"], json!("http://127.0.0.1:1"));

    let _ = std::fs::remove_file(&proxy_profiles_path);
}

#[tokio::test]
async fn vendor_oauth_callback_surfaces_token_exchange_http_failure() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let _web_ui_port_guard = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "9001".to_string());

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let request_text = String::from_utf8_lossy(&request[..read]);
        let first_line = request_text.lines().next().unwrap_or_default();
        assert!(
            first_line.starts_with("POST /token"),
            "expected vendor oauth token exchange request, got: {}",
            first_line
        );
        let body = r#"{"error":"invalid_grant","error_description":"authorization code expired"}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    seed_vendor_oauth_callback_state(
        &state,
        &vendor_oauth_path,
        "google-oauth",
        "state-http-fail",
        make_test_oauth_config(format!("http://{}/token", upstream_addr)),
    )
    .await;

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_vendor_oauth_callback(&mut socket, &state, "code=test-code&state=state-http-fail")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(response.contains("Token 交换失败 (HTTP 400)"));

    let guard = state.lock().await;
    assert!(
        !guard.vendor_oauth_pending.contains_key("state-http-fail"),
        "pending state should be consumed on callback"
    );
    drop(guard);

    let saved_doc = crate::bindings::load_vendor_oauth(&vendor_oauth_path).unwrap();
    assert!(
        saved_doc
            .configs
            .get("google-oauth")
            .expect("oauth config")
            .cached_token
            .is_none(),
        "failed token exchange must not persist cached_token"
    );

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn vendor_oauth_callback_returns_500_for_token_exchange_network_error() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let _web_ui_port_guard = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "9002".to_string());

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    seed_vendor_oauth_callback_state(
        &state,
        &vendor_oauth_path,
        "google-oauth",
        "state-network-fail",
        make_test_oauth_config("http://127.0.0.1:1/token".to_string()),
    )
    .await;

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_vendor_oauth_callback(
        &mut socket,
        &state,
        "code=test-code&state=state-network-fail",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 500 Internal Server Error"));
    assert!(response.contains("Token 交换网络请求失败"));

    let guard = state.lock().await;
    assert!(
        !guard
            .vendor_oauth_pending
            .contains_key("state-network-fail"),
        "pending state should still be consumed on network failure"
    );
    drop(guard);

    let saved_doc = crate::bindings::load_vendor_oauth(&vendor_oauth_path).unwrap();
    assert!(saved_doc
        .configs
        .get("google-oauth")
        .expect("oauth config")
        .cached_token
        .is_none());

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

#[tokio::test]
async fn vendor_oauth_callback_rejects_success_response_without_access_token() {
    let _env_guard = components_env_lock().lock().unwrap();
    let vendor_oauth_path = temp_path("test-vendor-oauth", "json");
    let _vendor_oauth_guard =
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", vendor_oauth_path.clone());
    let _web_ui_port_guard = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "9003".to_string());

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        // Read the request before responding (same as the passing sibling
        // tests): dropping a socket with UNREAD received data sends a TCP
        // RST, which on Windows can reset the connection before reqwest
        // reads the response — surfacing as a transport error (500) instead
        // of the 400 this test asserts.
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.unwrap();
        let request_text = String::from_utf8_lossy(&request[..read]);
        assert!(
            request_text
                .lines()
                .next()
                .unwrap_or_default()
                .starts_with("POST /token"),
            "expected vendor oauth token exchange request"
        );
        let body = r#"{"expires_in":3600,"refresh_token":"rt-123"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    seed_vendor_oauth_callback_state(
        &state,
        &vendor_oauth_path,
        "google-oauth",
        "state-missing-token",
        make_test_oauth_config(format!("http://{}/token", upstream_addr)),
    )
    .await;

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_vendor_oauth_callback(
        &mut socket,
        &state,
        "code=test-code&state=state-missing-token",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(response.contains("响应中缺少 access_token"));

    let saved_doc = crate::bindings::load_vendor_oauth(&vendor_oauth_path).unwrap();
    let saved_config = saved_doc.configs.get("google-oauth").expect("oauth config");
    assert!(saved_config.cached_token.is_none());
    assert!(saved_config.refresh_token.is_none());

    let _ = std::fs::remove_file(&vendor_oauth_path);
}

struct OwnedIntegrationTokenServer {
    base: String,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    forms: Arc<std::sync::Mutex<Vec<HashMap<String, String>>>>,
    task: tokio::task::JoinHandle<()>,
}

async fn owned_integration_mutation(
    state: &Arc<Mutex<WebUiState>>,
    kind: &str,
    update: bool,
    body: &Value,
) -> (anyhow::Result<()>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let body = serde_json::to_vec(body).unwrap();
    let result = match (kind, update) {
        ("keys", false) => handle_vendor_key_create(&mut socket, state, &body).await,
        ("keys", true) => handle_vendor_key_update(&mut socket, state, &body, "owned").await,
        ("oauth", false) => handle_oauth_config_create(&mut socket, state, &body).await,
        ("oauth", true) => handle_oauth_config_update(&mut socket, state, &body, "owned").await,
        ("proxy", false) => handle_proxy_create(&mut socket, state, &body).await,
        ("proxy", true) => handle_proxy_update(&mut socket, state, &body, "owned").await,
        _ => unreachable!(),
    };
    drop(socket);
    (result, reader.await.unwrap())
}

#[tokio::test]
async fn integration_durable_duplicate_oauth_create_does_not_replace_existing_profile() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let state = build_test_web_ui_state("", None);
    let (first, response) = integration_config_fixture(&state, "oauth", true).await;
    assert!(first.is_ok() && response.starts_with("HTTP/1.1 200"));
    let original = std::fs::read(&path).unwrap();
    let (second, response) = integration_config_fixture(&state, "oauth", true).await;
    assert!(
        second.is_err() || response.starts_with("HTTP/1.1 409"),
        "creating the same profile again cannot replace credentials"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn integration_durable_invalid_shapes_and_limits_cannot_change_configuration() {
    let _env = components_env_lock().lock().unwrap();
    for (kind, variable, cases) in [
        (
            "keys",
            "WPTSALL_VENDOR_KEYS_FILE",
            vec![
                json!({"max_concurrent":0}),
                json!({"max_concurrent":u64::MAX}),
                json!({"weight":u64::MAX}),
                json!({"requests_per_second":-1}),
                json!({"max_input_chars":u64::MAX}),
                json!({"max_file_size_mb":-1}),
                json!({"auth_values":{"api_key":false}}),
                json!({"enabled":"false"}),
            ],
        ),
        (
            "oauth",
            "WPTSALL_VENDOR_OAUTH_FILE",
            vec![
                json!({"max_concurrent":0}),
                json!({"max_concurrent":u64::MAX}),
                json!({"weight":u64::MAX}),
                json!({"max_input_chars":u64::MAX}),
                json!({"max_file_size_mb":-1}),
                json!({"extra_params":{"audience":false}}),
                json!({"auth_extra_params":[]}),
                json!({"scopes":[]}),
            ],
        ),
        (
            "proxy",
            "WPTSALL_PROXY_PROFILES_FILE",
            vec![
                json!({"port":"wrong"}),
                json!({"port":-1}),
                json!({"enabled":"false"}),
                json!({"password":false}),
            ],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-config.json");
        let _path = EnvVarGuard::set(variable, path.display().to_string());
        let state = build_test_web_ui_state("", None);
        let (created, response) = integration_config_fixture(&state, kind, true).await;
        assert!(
            created.is_ok() && response.starts_with("HTTP/1.1 200"),
            "{response}"
        );
        let original = std::fs::read(&path).unwrap();
        for patch in cases {
            for update in [false, true] {
                let mut body = json!({"id":"new-owned","vendor_id":"owned","client_id":"owned",
                    "token_url":"http://127.0.0.1:1/token","host":"127.0.0.1"});
                body.as_object_mut()
                    .unwrap()
                    .extend(patch.as_object().unwrap().clone());
                let (result, response) =
                    owned_integration_mutation(&state, kind, update, &body).await;
                assert!(result.is_err() || !response.starts_with("HTTP/1.1 200"),
                    "{kind} update={update}: invalid fields must refuse, not truncate or silently ignore: {patch}");
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    original,
                    "invalid input cannot change the file"
                );
            }
        }
    }
}

impl Drop for OwnedIntegrationTokenServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OwnedIntegrationTokenServer {
    async fn start(lose_first_reply: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = requests.clone();
        let forms = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_forms = forms.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0; 2048];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    raw.extend_from_slice(&buffer[..count]);
                    if let Some(end) = raw.windows(4).position(|part| part == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..end]);
                        let length = head
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if raw.len() >= end + 4 + length {
                            assert!(head.starts_with("POST /token "));
                            received_forms.lock().unwrap().push(
                                url::form_urlencoded::parse(&raw[end + 4..end + 4 + length])
                                    .into_owned()
                                    .collect(),
                            );
                            break;
                        }
                    }
                }
                let previous = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if lose_first_reply && previous == 0 {
                    continue;
                }
                let body = r#"{"access_token":"owned-token","expires_in":3600}"#;
                socket.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body,
                ).as_bytes()).await.unwrap();
            }
        });
        Self {
            base,
            requests,
            forms,
            task,
        }
    }
}

async fn authorize_integration_fixture(state: &Arc<Mutex<WebUiState>>, id: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    handle_oauth_authorize(&mut socket, state, id)
        .await
        .unwrap();
    drop(socket);
    reader.await.unwrap()
}

#[tokio::test]
async fn integration_durable_client_credentials_replays_original_issue() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let server = OwnedIntegrationTokenServer::start(false).await;
    let mut config = make_test_oauth_config(format!("{}/token", server.base));
    config.grant_type = "client_credentials".into();
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert("owned".into(), config);
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &doc).unwrap();
    let state = build_test_web_ui_state("", None);
    let first = authorize_integration_fixture(&state, "owned").await;
    let second = authorize_integration_fixture(&state, "owned").await;
    assert!(first.starts_with("HTTP/1.1 200"), "{first}");
    assert!(second.starts_with("HTTP/1.1 200"), "{second}");
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        !first.contains("owned-token"),
        "authorization responses must not expose a short token"
    );
    let guard = state.lock().await;
    let conn = guard.db.lock().await;
    assert!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn integration_durable_client_credentials_lost_reply_is_not_reissued() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let server = OwnedIntegrationTokenServer::start(true).await;
    let mut config = make_test_oauth_config(format!("{}/token", server.base));
    config.grant_type = "client_credentials".into();
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert("owned".into(), config);
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &doc).unwrap();
    let state = build_test_web_ui_state("", None);
    let first = authorize_integration_fixture(&state, "owned").await;
    let second = authorize_integration_fixture(&state, "owned").await;
    assert!(!first.starts_with("HTTP/1.1 200"), "{first}");
    assert!(!second.starts_with("HTTP/1.1 200"), "{second}");
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

async fn integration_config_fixture(
    state: &Arc<Mutex<WebUiState>>,
    kind: &str,
    create: bool,
) -> (anyhow::Result<()>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let result = match (kind, create) {
        ("keys", false) => handle_vendor_keys_list(&mut socket,state,"").await,
        ("oauth", false) => handle_oauth_configs_list(&mut socket,state,"").await,
        ("proxy", false) => handle_proxy_list(&mut socket,state).await,
        ("keys", true) => handle_vendor_key_create(&mut socket,state,
            br#"{"id":"owned","vendor_id":"owned","auth_values":{"api_key":"owned-mock"}}"#).await,
        ("oauth", true) => handle_oauth_config_create(&mut socket,state,
            br#"{"id":"owned","vendor_id":"owned","client_id":"owned","token_url":"http://127.0.0.1:1/token"}"#).await,
        ("proxy", true) => handle_proxy_create(&mut socket,state,
            br#"{"id":"owned","host":"127.0.0.1","port":12345}"#).await,
        _ => unreachable!(),
    };
    drop(socket);
    (result, reader.await.unwrap())
}

#[tokio::test]
async fn integration_durable_corrupt_configuration_is_not_an_empty_success() {
    let _env = components_env_lock().lock().unwrap();
    for (kind, variable) in [
        ("keys", "WPTSALL_VENDOR_KEYS_FILE"),
        ("oauth", "WPTSALL_VENDOR_OAUTH_FILE"),
        ("proxy", "WPTSALL_PROXY_PROFILES_FILE"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-damaged.json");
        let _path = EnvVarGuard::set(variable, path.display().to_string());
        std::fs::write(&path, b"{owned-damaged").unwrap();
        let state = build_test_web_ui_state("", None);
        let (result, response) = integration_config_fixture(&state, kind, false).await;
        assert!(
            result.is_err() || !response.starts_with("HTTP/1.1 200"),
            "{kind}: {response}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"{owned-damaged");
    }
}

#[tokio::test]
async fn integration_durable_ignored_database_save_does_not_change_file_or_claim_success() {
    let _env = components_env_lock().lock().unwrap();
    for (kind, variable, key) in [
        ("keys", "WPTSALL_VENDOR_KEYS_FILE", "vendor_keys_doc"),
        ("oauth", "WPTSALL_VENDOR_OAUTH_FILE", "vendor_oauth_doc"),
        ("proxy", "WPTSALL_PROXY_PROFILES_FILE", "proxy_profiles_doc"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("owned-config.json");
        let _path = EnvVarGuard::set(variable, path.display().to_string());
        let path_str = path.to_str().unwrap();
        match kind {
            "keys" => {
                crate::bindings::save_vendor_keys(path_str, &VendorKeysDoc::default()).unwrap()
            }
            "oauth" => {
                crate::bindings::save_vendor_oauth(path_str, &VendorOAuthDoc::default()).unwrap()
            }
            _ => crate::bindings::save_proxy_profiles(path_str, &ProxyProfilesDoc::default())
                .unwrap(),
        };
        let original = std::fs::read(&path).unwrap();
        let state = build_test_web_ui_state("", None);
        state
            .lock()
            .await
            .db
            .lock()
            .await
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_doc BEFORE INSERT ON system_config
             WHEN NEW.key='{key}' BEGIN SELECT RAISE(IGNORE); END;",
            ))
            .unwrap();
        let (result, response) = integration_config_fixture(&state, kind, true).await;
        assert!(
            result.is_err() || !response.starts_with("HTTP/1.1 200"),
            "{kind}: {response}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "{kind}: uncommitted credentials must not change the file"
        );
    }
}

async fn owned_code_flow_fixture(
    state: &Arc<Mutex<WebUiState>>,
    server: &OwnedIntegrationTokenServer,
    path: &std::path::Path,
) -> String {
    let mut doc = VendorOAuthDoc::default();
    doc.configs.insert(
        "owned".into(),
        make_test_oauth_config(format!("{}/token", server.base)),
    );
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &doc).unwrap();
    let response = authorize_integration_fixture(state, "owned").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let body = parse_http_json_body(&response);
    url::Url::parse(body["data"]["authorize_url"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned()
}

async fn callback_integration_fixture(state: &Arc<Mutex<WebUiState>>, oauth_state: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    let result = handle_vendor_oauth_callback(
        &mut socket,
        state,
        &format!("code=owned-code&state={oauth_state}"),
    )
    .await;
    drop(socket);
    let response = reader.await.unwrap();
    assert!(result.is_ok(), "callback returned an unhandled error");
    response
}

#[tokio::test]
async fn integration_durable_authorization_code_survives_runtime_recreation_with_original_redirect()
{
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let _port = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "8999");
    let server = OwnedIntegrationTokenServer::start(false).await;
    let first = build_test_web_ui_state("", None);
    let oauth_state = owned_code_flow_fixture(&first, &server, &path).await;
    let db = first.lock().await.db.clone();
    drop(first);
    let _changed_port = EnvVarGuard::set("WPTSALL_WEB_UI_PORT", "9009");
    let restarted = build_test_web_ui_state("", None);
    restarted.lock().await.db = db;
    let response = callback_integration_fixture(&restarted, &oauth_state).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        server.forms.lock().unwrap()[0]["redirect_uri"],
        "http://127.0.0.1:8999/oauth/vendor/callback"
    );
    let replay = callback_integration_fixture(&restarted, &oauth_state).await;
    assert!(replay.starts_with("HTTP/1.1 200"), "{replay}");
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn integration_durable_authorization_code_replays_ready_after_projection_refusal() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let server = OwnedIntegrationTokenServer::start(false).await;
    let state = build_test_web_ui_state("", None);
    let oauth_state = owned_code_flow_fixture(&state, &server, &path).await;
    state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch(
            "CREATE TRIGGER refuse_tokens BEFORE INSERT ON system_config
         WHEN NEW.key='vendor_oauth_doc' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let failed = callback_integration_fixture(&state, &oauth_state).await;
    assert!(!failed.starts_with("HTTP/1.1 200"), "{failed}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch("DROP TRIGGER refuse_tokens;")
        .unwrap();
    state.lock().await.vendor_oauth_pending.clear();
    let recovered = callback_integration_fixture(&state, &oauth_state).await;
    assert!(recovered.starts_with("HTTP/1.1 200"), "{recovered}");
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn integration_durable_authorization_code_unknown_exchange_fences_new_authorization() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let server = OwnedIntegrationTokenServer::start(true).await;
    let state = build_test_web_ui_state("", None);
    let oauth_state = owned_code_flow_fixture(&state, &server, &path).await;
    let failed = callback_integration_fixture(&state, &oauth_state).await;
    assert!(!failed.starts_with("HTTP/1.1 200"), "{failed}");
    let new_flow = authorize_integration_fixture(&state, "owned").await;
    assert!(
        !new_flow.starts_with("HTTP/1.1 200"),
        "unresolved token issue must not be replaced: {new_flow}"
    );
    let replay = callback_integration_fixture(&state, &oauth_state).await;
    assert!(!replay.starts_with("HTTP/1.1 200"), "{replay}");
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn integration_durable_oauth_identity_change_invalidates_old_token_not_profile_only_changes()
{
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let _path = EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", path.display().to_string());
    let mut config = make_test_oauth_config("http://127.0.0.1:1/token".into());
    config.cached_token = Some("owned-old-token".into());
    config.refresh_token = Some("owned-old-refresh".into());
    config.cached_token_expires_at = unix_ts() as i64 + 3600;
    let doc = VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([("owned".into(), config)]),
    };
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &doc).unwrap();
    let state = build_test_web_ui_state("", None);
    for (body, retained) in [
        (br#"{"label":"owned new label"}"#.as_slice(), true),
        (br#"{"client_id":"owned-new-client"}"#.as_slice(), false),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reader = tokio::spawn(async move {
            let mut socket = TcpStream::connect(address).await.unwrap();
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let (mut socket, _) = listener.accept().await.unwrap();
        handle_oauth_config_update(&mut socket, &state, body, "owned")
            .await
            .unwrap();
        drop(socket);
        assert!(reader.await.unwrap().starts_with("HTTP/1.1 200"));
        let saved = crate::bindings::load_vendor_oauth(path.to_str().unwrap()).unwrap();
        let saved = &saved.configs["owned"];
        assert_eq!(
            saved.cached_token.is_some(),
            retained,
            "token ownership follows credentials, not the profile label"
        );
        assert_eq!(saved.refresh_token.is_some(), retained);
        if !retained {
            assert_eq!(saved.cached_token_expires_at, 0);
        }
    }
}

mod config_store;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/physical_config_capacity.rs"]
mod physical_config_capacity;
#[cfg(target_os = "linux")]
mod process;
