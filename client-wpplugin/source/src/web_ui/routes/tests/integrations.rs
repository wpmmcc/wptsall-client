use super::*;
use std::collections::HashMap;

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
    doc.configs.insert(config_id.to_string(), config);
    crate::bindings::save_vendor_oauth(vendor_oauth_path, &doc).unwrap();
    let mut guard = state.lock().await;
    guard.vendor_oauth_pending.insert(
        oauth_state.to_string(),
        VendorOAuthPendingEntry {
            config_id: config_id.to_string(),
            code_verifier: "verifier-seeded".to_string(),
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
            request_text.lines().next().unwrap_or_default().starts_with("POST /token"),
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
