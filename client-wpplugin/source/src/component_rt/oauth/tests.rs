use super::*;
use tempfile::tempdir;

#[tokio::test]
async fn cached_token_returned_when_valid() {
    let mut configs = HashMap::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    configs.insert(
        "test".to_string(),
        OAuthConfig {
            vendor_id: "vendor-1".into(),
            label: "Test".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "csecret".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("cached-token-abc".into()),
            cached_token_expires_at: now + 3600,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".into(),
        },
    );

    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth.json".into(),
    );
    let token = manager.get_token("test").await.unwrap();
    assert_eq!(token, "cached-token-abc");
}

#[tokio::test]
async fn missing_config_returns_error() {
    let manager = OAuthTokenManager::new(
        HashMap::new(),
        reqwest::Client::new(),
        "/tmp/test-oauth.json".into(),
    );
    let result = manager.get_token("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn persist_doc_syncs_vendor_oauth_to_db_when_db_path_configured() {
    let dir = tempdir().expect("tempdir");
    let oauth_path = dir.path().join("vendor-oauth.json");
    let db_path = dir.path().join("wptsall.db");

    let mut configs = HashMap::new();
    configs.insert(
        "cfg-1".to_string(),
        OAuthConfig {
            vendor_id: "vendor-1".into(),
            label: "Config 1".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "sec".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("cached-token-db".into()),
            cached_token_expires_at: 1234567890,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".into(),
        },
    );

    let mut manager = OAuthTokenManager::new(
        configs.clone(),
        reqwest::Client::new(),
        oauth_path.to_string_lossy().to_string(),
    );
    manager.db_path = Some(db_path.to_string_lossy().to_string());

    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: configs.clone(),
    };
    manager.persist_doc(&doc).await;

    let conn = crate::db::open_db(db_path.to_string_lossy().as_ref()).expect("open db");
    let loaded = crate::db::vendor::load_vendor_oauth_doc(&conn);
    assert!(loaded.configs.contains_key("cfg-1"));
    assert_eq!(
        loaded
            .configs
            .get("cfg-1")
            .and_then(|v| v.cached_token.clone())
            .as_deref(),
        Some("cached-token-db")
    );
}

#[tokio::test]
async fn authorization_code_without_refresh_token_returns_error() {
    // authorization_code is now supported; without a stored refresh_token it should
    // return an error asking the user to re-authorize via the Web UI.
    let mut configs = HashMap::new();
    configs.insert(
        "test".to_string(),
        OAuthConfig {
            vendor_id: "vendor-1".into(),
            label: "Test".into(),
            grant_type: "authorization_code".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: String::new(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: None,
            cached_token_expires_at: 0,
            refresh_token: None, // no refresh token stored
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".into(),
        },
    );

    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth.json".into(),
    );
    let result = manager.get_token("test").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("no refresh_token") || err.contains("re-authorize"),
        "expected re-authorize error, got: {}",
        err
    );
}

#[tokio::test]
async fn unsupported_grant_type_returns_error() {
    let mut configs = HashMap::new();
    configs.insert(
        "test".to_string(),
        OAuthConfig {
            vendor_id: "vendor-1".into(),
            label: "Test".into(),
            grant_type: "magic_beans".into(), // truly unsupported
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: String::new(),
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
            token_field: "access_token".into(),
        },
    );

    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth.json".into(),
    );
    let result = manager.get_token("test").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unsupported oauth grant_type"),
        "error: {}",
        err
    );
}

#[tokio::test]
async fn oauth_pool_max_file_size_mb_propagated_to_guard() {
    // Uses a cached token to avoid network requests.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut configs = HashMap::new();
    configs.insert(
        "oc-1".to_string(),
        OAuthConfig {
            vendor_id: "vendor-1".into(),
            label: "Test OAuth".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("cached-token-xyz".into()),
            cached_token_expires_at: now + 3600,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 15.5,
            token_field: "access_token".into(),
        },
    );
    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth-pool.json".into(),
    );
    let pool = OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "oc-1".into(),
            token_field: "access_token".into(),
            max_concurrent: 5,
            max_input_chars: 0,
            max_file_size_mb: 15.5,
            weight: 1,
            active_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }],
        crate::types::KeySelectionStrategy::RoundRobin,
    );
    let guard = pool.select(&manager, 0, 0.0).await.unwrap();
    assert_eq!(guard.max_file_size_mb, 15.5);
    assert_eq!(guard.access_token, "cached-token-xyz");
}

#[tokio::test]
async fn oauth_pool_zero_max_file_size_mb_means_unlimited() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut configs = HashMap::new();
    configs.insert(
        "oc-2".to_string(),
        OAuthConfig {
            vendor_id: "vendor-2".into(),
            label: "No Limit".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("tok".into()),
            cached_token_expires_at: now + 3600,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".into(),
        },
    );
    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth-pool2.json".into(),
    );
    let pool = OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "oc-2".into(),
            token_field: "access_token".into(),
            max_concurrent: 5,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            weight: 1,
            active_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }],
        crate::types::KeySelectionStrategy::RoundRobin,
    );
    let guard = pool.select(&manager, 0, 0.0).await.unwrap();
    assert_eq!(guard.max_file_size_mb, 0.0, "0.0 should mean unlimited");
}

#[tokio::test]
async fn oauth_pool_file_size_mb_filter() {
    // Pool with two entries: "small" (max_file_size_mb=1.0) and "big" (unlimited=0.0).
    // Selecting with file_size_mb=5.0 should skip "small" and return "big".
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut configs = HashMap::new();
    configs.insert(
        "small".to_string(),
        OAuthConfig {
            vendor_id: "v".into(),
            label: "Small".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("token-small".into()),
            cached_token_expires_at: now + 3600,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 1.0,
            token_field: "access_token".into(),
        },
    );
    configs.insert(
        "big".to_string(),
        OAuthConfig {
            vendor_id: "v".into(),
            label: "Big".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("token-big".into()),
            cached_token_expires_at: now + 3600,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".into(),
        },
    );
    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth-filter.json".into(),
    );
    let pool = OAuthPool::new(
        vec![
            OAuthPoolEntry {
                config_id: "small".into(),
                token_field: "access_token".into(),
                max_concurrent: 5,
                max_input_chars: 0,
                max_file_size_mb: 1.0,
                weight: 1,
                active_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
            OAuthPoolEntry {
                config_id: "big".into(),
                token_field: "access_token".into(),
                max_concurrent: 5,
                max_input_chars: 0,
                max_file_size_mb: 0.0,
                weight: 1,
                active_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
        ],
        crate::types::KeySelectionStrategy::RoundRobin,
    );
    // file_size_mb=5.0 should skip "small" and select "big"
    let guard = pool.select(&manager, 0, 5.0).await.unwrap();
    assert_eq!(guard.access_token, "token-big");
}

#[tokio::test]
async fn oauth_pool_no_eligible_file_size_fails_fast() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut configs = HashMap::new();
    configs.insert(
        "only".to_string(),
        OAuthConfig {
            vendor_id: "v".into(),
            label: "Only".into(),
            grant_type: "client_credentials".into(),
            auth_url: String::new(),
            token_url: "http://localhost/token".into(),
            client_id: "cid".into(),
            client_secret: "cs".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("tok".into()),
            cached_token_expires_at: now + 3600,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 1.0,
            token_field: "access_token".into(),
        },
    );
    let manager = OAuthTokenManager::new(
        configs,
        reqwest::Client::new(),
        "/tmp/test-oauth-noel.json".into(),
    );
    let pool = OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "only".into(),
            token_field: "access_token".into(),
            max_concurrent: 5,
            max_input_chars: 0,
            max_file_size_mb: 1.0,
            weight: 1,
            active_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }],
        crate::types::KeySelectionStrategy::RoundRobin,
    );
    // file_size_mb=5.0 exceeds max_file_size_mb=1.0 — should fail immediately
    let result = pool.select(&manager, 0, 5.0).await;
    assert!(result.is_err(), "should fail when no eligible oauth entry");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("no eligible oauth entry"),
        "expected 'no eligible oauth entry' error, got: {}",
        err
    );
}
