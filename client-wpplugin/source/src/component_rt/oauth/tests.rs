use super::*;
use tempfile::tempdir;

#[tokio::test]
async fn physical_sqlite_oauth_readonly_probe_cannot_create_unadmitted_shm() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-oauth-readonly-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let path = root.join("legacy.sqlite");
    // An owned legacy database intentionally predates capacity bookings.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE system_config(key TEXT PRIMARY KEY,value TEXT NOT NULL);
         INSERT INTO system_config VALUES ('owned-reader-receipt','retained');",
    )
    .unwrap();
    let mut persist = 1i32;
    assert_eq!(
        unsafe {
            rusqlite::ffi::sqlite3_file_control(
                conn.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
                (&mut persist as *mut i32).cast(),
            )
        },
        rusqlite::ffi::SQLITE_OK
    );
    drop(conn);
    let shm = root.join("legacy.sqlite-shm");
    // Only the disposable SHM created above is removed; DB/WAL evidence stays.
    std::fs::remove_file(&shm).unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let endpoint = RemainingTokenEndpoint::start(false).await;
    let config = endpoint.config("authorization_code");
    let mut manager = OAuthTokenManager::from_snapshot(
        HashMap::from([("owned".into(), config.clone())]),
        OAuthHttpClient::direct().unwrap(),
    );
    manager.db_path = Some(path.display().to_string());
    let result = super::recovery::TokenExecution::has_checkpoint(&manager, "owned", &config).await;
    let after = shm.metadata().ok().map(|metadata| metadata.len());
    assert!(
        result.is_err() && after.is_none(),
        "read-only SQL must not bypass physical SHM admission: \
         root={} result={result:?} shm_bytes={after:?}",
        root.display()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(endpoint.requests.lock().unwrap().is_empty());
}

#[path = "../../../../../tests/modules/client-wpplugin/unit/oauth_token_response.rs"]
mod token_response;

#[path = "../../../../../tests/modules/client-wpplugin/unit/physical_oauth_capacity.rs"]
mod physical_capacity;

#[path = "../../../../../tests/modules/client-wpplugin/unit/oauth_transport.rs"]
mod transport;

#[cfg(target_os = "linux")]
mod process;

#[test]
fn configuration_authority_weighted_oauth_pool_does_not_overflow_valid_weights() {
    let pool = OAuthPool::new(
        ["owned-a", "owned-b"]
            .into_iter()
            .map(|id| OAuthPoolEntry {
                config_id: id.into(),
                token_field: "access_token".into(),
                max_concurrent: 1,
                max_input_chars: 0,
                max_file_size_mb: 0.0,
                weight: u32::MAX,
                active_count: Arc::new(AtomicUsize::new(0)),
            })
            .collect(),
        KeySelectionStrategy::Weighted,
    );
    for _ in 0..8 {
        let selected = pool
            .try_acquire_weighted(1, 1.0)
            .expect("valid weighted OAuth profile must be selectable");
        assert!(selected.config_id_tmp == "owned-a" || selected.config_id_tmp == "owned-b");
        drop(OAuthGuard {
            access_token: String::new(),
            token_field: selected.token_field_tmp,
            max_file_size_mb: selected.max_file_size_mb_tmp,
            active_count: selected.active_count,
            notify: selected.notify,
        });
    }
}

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
        OAuthHttpClient::direct().unwrap(),
        "/tmp/test-oauth.json".into(),
    );
    let token = manager.get_token("test").await.unwrap();
    assert_eq!(token, "cached-token-abc");
}

#[tokio::test]
async fn remaining_manual_oauth_snapshot_drops_live_refresh_sentinel_and_preserves_limits() {
    let config: OAuthConfig = serde_json::from_value(serde_json::json!({
        "vendor_id":"owned","label":"Owned","grant_type":"client_credentials","auth_url":"",
        "token_url":"http://127.0.0.1:9","client_id":"owned","client_secret":"owned-mock-secret",
        "scopes":"","cached_token":"","cached_token_expires_at":0
    }))
    .unwrap();
    let manager = OAuthTokenManager::new(
        HashMap::from([("owned".into(), config)]),
        OAuthHttpClient::direct().unwrap(),
        String::new(),
    );
    let snapshot = manager.snapshot().await;
    assert!(
        snapshot["owned"].cached_token.is_none(),
        "a live refresh sentinel is not a durable refresh owner"
    );
    let restored = OAuthTokenManager::from_snapshot(snapshot, OAuthHttpClient::direct().unwrap());
    assert!(restored.db_path.is_none());
    assert!(
        restored.config_path.is_empty(),
        "frozen credentials cannot overwrite current operator configuration"
    );
    let pool = OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "owned".into(),
            token_field: "access_token".into(),
            max_concurrent: 2,
            max_input_chars: 8,
            max_file_size_mb: 5.0,
            weight: 3,
            active_count: Arc::new(AtomicUsize::new(0)),
        }],
        KeySelectionStrategy::Weighted,
    );
    let restored = OAuthPool::from_snapshot(pool.snapshot());
    assert!(restored.has_eligible(8, 5.0));
    assert!(!restored.has_eligible(9, 5.0));
    assert!(!restored.has_eligible(8, 6.0));
}

#[tokio::test]
async fn remaining_manual_restored_oauth_pools_share_concurrency_and_release_wakeup() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let config: OAuthConfig = serde_json::from_value(serde_json::json!({
        "vendor_id":"owned","label":"Owned","grant_type":"client_credentials","token_url":"http://127.0.0.1:9",
        "client_id":"owned","cached_token":"owned-cached-token","cached_token_expires_at":now+3600
    })).unwrap();
    let manager = Arc::new(OAuthTokenManager::from_snapshot(
        HashMap::from([("owned-shared-oauth".into(), config)]),
        OAuthHttpClient::direct().unwrap(),
    ));
    let pool = OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "owned-shared-oauth".into(),
            token_field: "access_token".into(),
            max_concurrent: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            weight: 1,
            active_count: Arc::new(AtomicUsize::new(0)),
        }],
        KeySelectionStrategy::RoundRobin,
    );
    let first = OAuthPool::from_snapshot(pool.snapshot());
    let second = OAuthPool::from_snapshot(pool.snapshot());
    let owner = first.select(&manager, 1, 0.0).await.unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            second.select(&manager, 1, 0.0)
        )
        .await
        .is_err(),
        "restored OAuth scopes cannot each mint a fresh concurrency budget"
    );
    let waiting = tokio::spawn(async move { second.select(&manager, 1, 0.0).await });
    tokio::task::yield_now().await;
    drop(owner);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), waiting)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn remaining_oauth_failed_token_lookup_releases_the_reserved_slot() {
    let pool = OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "owned-missing-oauth".into(),
            token_field: "access_token".into(),
            max_concurrent: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            weight: 1,
            active_count: Arc::new(AtomicUsize::new(0)),
        }],
        KeySelectionStrategy::RoundRobin,
    );
    let manager =
        OAuthTokenManager::from_snapshot(HashMap::new(), OAuthHttpClient::direct().unwrap());
    assert!(pool.select(&manager, 1, 0.0).await.is_err());
    assert_eq!(
        pool.entries[0].active_count.load(Ordering::SeqCst),
        0,
        "a refused credential lookup cannot permanently consume the OAuth budget"
    );
}

struct RemainingTokenEndpoint {
    url: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    worker: tokio::task::JoinHandle<()>,
    release: Arc<tokio::sync::Notify>,
}

impl Drop for RemainingTokenEndpoint {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl RemainingTokenEndpoint {
    async fn start(delay: bool) -> Self {
        Self::with_ttl(delay, 5).await
    }

    async fn with_ttl(delay: bool, ttl: i64) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let saved = requests.clone();
        let release = Arc::new(tokio::sync::Notify::new());
        let unblock = release.clone();
        let worker = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let body = loop {
                    let count = socket.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break String::from_utf8_lossy(&bytes[end + 4..end + 4 + length])
                                .into_owned();
                        }
                    }
                };
                let number = {
                    let mut requests = saved.lock().unwrap();
                    requests.push(body);
                    requests.len()
                };
                if delay {
                    unblock.notified().await;
                }
                let body = format!(
                    r#"{{"access_token":"owned-token-{number}","expires_in":{ttl},"refresh_token":"owned-rotation-{number}"}}"#
                );
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        Self {
            url,
            requests,
            worker,
            release,
        }
    }

    fn config(&self, grant: &str) -> OAuthConfig {
        serde_json::from_value(serde_json::json!({
            "vendor_id":"owned","label":"Owned token","grant_type":grant,
            "token_url":self.url,"client_id":"owned-client","client_secret":"owned-mock-secret",
            "refresh_token":if grant == "authorization_code" { Some("owned-original-refresh") } else { None },
        })).unwrap()
    }
}

#[tokio::test]
async fn remaining_oauth_missing_refresh_does_not_leave_a_permanent_sentinel() {
    let mut config: OAuthConfig = serde_json::from_value(serde_json::json!({
        "vendor_id":"owned","label":"Owned","grant_type":"authorization_code",
        "token_url":"http://127.0.0.1:9","client_id":"owned-client",
    }))
    .unwrap();
    config.refresh_token = None;
    let manager = OAuthTokenManager::from_snapshot(
        HashMap::from([("owned".into(), config)]),
        OAuthHttpClient::direct().unwrap(),
    );
    assert!(manager.get_token("owned").await.is_err());
    let second = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        manager.get_token("owned"),
    )
    .await;
    assert!(
        second.is_ok(),
        "a failed refresh cannot leave all future callers sleeping on a sentinel"
    );
    assert!(second.unwrap().is_err());
}

#[tokio::test]
async fn remaining_oauth_cancelled_refresh_retains_unknown_without_blocking_or_reissuing() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let endpoint = RemainingTokenEndpoint::start(true).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let manager = Arc::new(OAuthTokenManager::new(
        configs.clone(),
        OAuthHttpClient::direct().unwrap(),
        root.path().join("oauth.json").display().to_string(),
    ));
    let running = manager.clone();
    let work = tokio::spawn(async move { running.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    work.abort();
    let _ = work.await;
    let second = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        manager.get_token("owned"),
    )
    .await;
    assert!(
        second.is_ok(),
        "cancellation cannot leave a permanent refresh sentinel"
    );
    assert!(
        second.unwrap().is_err(),
        "an uncertain rotating refresh must stay parked"
    );
    let restarted = OAuthTokenManager::new(
        configs,
        OAuthHttpClient::direct().unwrap(),
        root.path().join("oauth.json").display().to_string(),
    );
    assert!(tokio::time::timeout(
        std::time::Duration::from_millis(300),
        restarted.get_token("owned")
    )
    .await
    .unwrap()
    .is_err());
    assert_eq!(
        endpoint.requests.lock().unwrap().len(),
        1,
        "unknown rotating refresh must not be reissued after restart"
    );
}

#[tokio::test]
async fn remaining_oauth_storage_refusal_stops_before_token_issue() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let blocker = root.path().join("owned-blocker");
    std::fs::write(&blocker, "owned retained bytes").unwrap();
    let endpoint = RemainingTokenEndpoint::start(false).await;
    let manager = OAuthTokenManager::new(
        HashMap::from([("owned".into(), endpoint.config("authorization_code"))]),
        OAuthHttpClient::direct().unwrap(),
        blocker.join("oauth.json").display().to_string(),
    );
    assert!(
        manager.get_token("owned").await.is_err(),
        "token issue cannot succeed without durable rotation storage"
    );
    assert!(
        endpoint.requests.lock().unwrap().is_empty(),
        "storage refusal must precede token egress"
    );
    assert_eq!(std::fs::read(&blocker).unwrap(), b"owned retained bytes");
}

#[tokio::test]
async fn remaining_oauth_frozen_restart_uses_rotation_without_overwriting_operator_config() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("owned.sqlite").display().to_string();
    let conn = crate::db::open_db(&path).unwrap();
    let endpoint = RemainingTokenEndpoint::start(false).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    crate::db::vendor::save_vendor_oauth_doc(
        &conn,
        &crate::types::VendorOAuthDoc {
            version: 1,
            configs: configs.clone(),
        },
    )
    .unwrap();
    let original = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc").unwrap();
    let client = OAuthHttpClient::direct().unwrap();
    let mut first = OAuthTokenManager::from_snapshot(configs.clone(), client.clone());
    first.db_path = Some(path.clone());
    assert_eq!(first.get_token("owned").await.unwrap(), "owned-token-1");
    let mut second = OAuthTokenManager::from_snapshot(configs, client);
    second.db_path = Some(path);
    assert_eq!(second.get_token("owned").await.unwrap(), "owned-token-2");
    let requests = endpoint.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        form_field(&requests[1], "refresh_token").as_deref(),
        Some("owned-rotation-1"),
        "a frozen restart must use the durable rotation, not the original consumed refresh token"
    );
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc").unwrap(),
        original,
        "restored frozen credentials must not overwrite current operator configuration"
    );
}

#[tokio::test]
async fn remaining_oauth_ready_checkpoint_is_replayed_across_concurrent_frozen_managers() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(&root.path().join("owned.sqlite").display().to_string()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let first =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    let second = OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
        .with_recovery_db(db);
    let (a, b) = tokio::join!(first.get_token("owned"), second.get_token("owned"));
    assert_eq!(a.unwrap(), "owned-token-1");
    assert_eq!(b.unwrap(), "owned-token-1");
    assert_eq!(
        endpoint.requests.lock().unwrap().len(),
        1,
        "same credential scope must not mint parallel refresh owners"
    );
}

#[tokio::test]
async fn remaining_oauth_post_reply_checkpoint_refusal_retains_intent_and_never_reissues() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("owned.sqlite").display().to_string();
    let conn = crate::db::open_db(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER owned_refuse_rotation BEFORE UPDATE ON system_config
        WHEN OLD.key LIKE 'oauth-token-checkpoint-v1:%' BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    let endpoint = RemainingTokenEndpoint::start(false).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let mut manager =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap());
    manager.db_path = Some(path.clone());
    assert!(manager.get_token("owned").await.is_err());
    conn.execute_batch("DROP TRIGGER owned_refuse_rotation")
        .unwrap();
    let mut restarted =
        OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap());
    restarted.db_path = Some(path);
    assert!(restarted.get_token("owned").await.is_err());
    assert_eq!(
        endpoint.requests.lock().unwrap().len(),
        1,
        "a received rotation without durable receipt remains unknown"
    );
}

#[tokio::test]
async fn remaining_oauth_wrong_key_or_corrupt_checkpoint_refuses_without_new_egress() {
    let root = tempdir().unwrap();
    let path = root.path().join("owned.sqlite").display().to_string();
    let conn = crate::db::open_db(&path).unwrap();
    let endpoint = RemainingTokenEndpoint::start(false).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let client = OAuthHttpClient::direct().unwrap();
    {
        let _key = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-oauth-key-before",
        );
        let mut manager = OAuthTokenManager::from_snapshot(configs.clone(), client.clone());
        manager.db_path = Some(path.clone());
        assert_eq!(manager.get_token("owned").await.unwrap(), "owned-token-1");
    }
    let original: String = conn
        .query_row(
            "SELECT value FROM system_config WHERE key LIKE 'oauth-token-checkpoint-v1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    {
        let _key = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-oauth-key-after",
        );
        let mut manager = OAuthTokenManager::from_snapshot(configs.clone(), client.clone());
        manager.db_path = Some(path.clone());
        assert!(manager.get_token("owned").await.is_err());
    }
    assert_eq!(
        conn.query_row(
            "SELECT value FROM system_config WHERE key LIKE 'oauth-token-checkpoint-v1:%'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        original
    );
    conn.execute("UPDATE system_config SET value='V1BUQw-damaged' WHERE key LIKE 'oauth-token-checkpoint-v1:%'",[]).unwrap();
    let _key = crate::db::TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-oauth-key-before",
    );
    let mut manager = OAuthTokenManager::from_snapshot(configs, client);
    manager.db_path = Some(path);
    assert!(manager.get_token("owned").await.is_err());
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn remaining_oauth_rotation_cannot_overwrite_operator_changes_during_refresh() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("oauth.json").display().to_string();
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    crate::bindings::save_vendor_oauth(
        &path,
        &crate::types::VendorOAuthDoc {
            version: 1,
            configs: configs.clone(),
        },
    )
    .unwrap();
    let manager = Arc::new(OAuthTokenManager::new(
        configs,
        OAuthHttpClient::direct().unwrap(),
        path.clone(),
    ));
    let running = manager.clone();
    let work = tokio::spawn(async move { running.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut changed = crate::bindings::load_vendor_oauth(&path).unwrap();
    changed.configs.get_mut("owned").unwrap().client_secret = "owned-operator-replacement".into();
    crate::bindings::save_vendor_oauth(&path, &changed).unwrap();
    let original = std::fs::read(&path).unwrap();
    endpoint.release.notify_one();
    assert!(
        work.await.unwrap().is_err(),
        "a stale runtime cannot project rotated tokens over changed credentials"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "operator-owned bytes must remain unchanged"
    );
}

#[tokio::test]
async fn remaining_oauth_rotation_preserves_unrelated_operator_profiles() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("oauth.json").display().to_string();
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    crate::bindings::save_vendor_oauth(
        &path,
        &crate::types::VendorOAuthDoc {
            version: 1,
            configs: configs.clone(),
        },
    )
    .unwrap();
    let manager = Arc::new(OAuthTokenManager::new(
        configs,
        OAuthHttpClient::direct().unwrap(),
        path.clone(),
    ));
    let running = manager.clone();
    let work = tokio::spawn(async move { running.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut changed = crate::bindings::load_vendor_oauth(&path).unwrap();
    let mut added = endpoint.config("client_credentials");
    added.label = "Owned added during refresh".into();
    changed.configs.insert("unrelated-owned".into(), added);
    crate::bindings::save_vendor_oauth(&path, &changed).unwrap();
    endpoint.release.notify_one();
    assert_eq!(work.await.unwrap().unwrap(), "owned-token-1");
    assert!(
        crate::bindings::load_vendor_oauth(&path)
            .unwrap()
            .configs
            .contains_key("unrelated-owned"),
        "a token projection must merge only its owned profile, not replace the entire document"
    );
}

#[tokio::test]
async fn remaining_oauth_rotation_cannot_resurrect_a_deleted_operator_profile() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("oauth.json").display().to_string();
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    crate::bindings::save_vendor_oauth(
        &path,
        &crate::types::VendorOAuthDoc {
            version: 1,
            configs: configs.clone(),
        },
    )
    .unwrap();
    let manager = Arc::new(OAuthTokenManager::new(
        configs,
        OAuthHttpClient::direct().unwrap(),
        path.clone(),
    ));
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    crate::bindings::save_vendor_oauth(&path, &crate::types::VendorOAuthDoc::default()).unwrap();
    let deleted = std::fs::read(&path).unwrap();
    endpoint.release.notify_one();
    assert!(
        work.await.unwrap().is_err(),
        "a completed rotation cannot restore operator-deleted credentials"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        deleted,
        "deleted profile bytes must remain unchanged"
    );
}

#[tokio::test]
async fn remaining_oauth_valid_cached_token_cannot_bypass_deleted_operator_profile() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("oauth.json").display().to_string();
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let mut config = endpoint.config("authorization_code");
    config.cached_token = Some("owned-cached-before-delete".into());
    config.cached_token_expires_at = crate::logging::unix_ts() as i64 + 3600;
    crate::bindings::save_vendor_oauth(&path, &crate::types::VendorOAuthDoc::default()).unwrap();
    let deleted = std::fs::read(&path).unwrap();
    let manager = OAuthTokenManager::new(
        HashMap::from([("owned".into(), config)]),
        OAuthHttpClient::direct().unwrap(),
        path.clone(),
    );
    assert!(
        manager.get_token("owned").await.is_err(),
        "cached credentials do not restore a deleted profile"
    );
    assert_eq!(std::fs::read(&path).unwrap(), deleted);
    assert!(endpoint.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn remaining_oauth_partial_db_file_projection_replays_saved_rotation_without_new_issue() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let path = root.path().join("oauth.json").display().to_string();
    let db_path = root.path().join("owned.sqlite").display().to_string();
    let db = crate::db::open_db(&db_path).unwrap();
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: configs.clone(),
    };
    crate::bindings::save_vendor_oauth(&path, &doc).unwrap();
    crate::db::vendor::save_vendor_oauth_doc(&db, &doc).unwrap();
    let mut manager = OAuthTokenManager::new(
        configs.clone(),
        OAuthHttpClient::direct().unwrap(),
        path.clone(),
    );
    manager.db_path = Some(db_path.clone());
    let manager = Arc::new(manager);
    let running = manager.clone();
    let work = tokio::spawn(async move { running.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("oauth.json.mutation.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    endpoint.release.notify_one();
    assert!(
        work.await.unwrap().is_err(),
        "a blocked file projection must not report durable success"
    );
    drop(lock);
    assert_eq!(
        crate::bindings::load_vendor_oauth(&path).unwrap().configs["owned"]
            .refresh_token
            .as_deref(),
        Some("owned-original-refresh")
    );
    assert_eq!(
        crate::db::vendor::load_vendor_oauth_doc(&db).configs["owned"]
            .refresh_token
            .as_deref(),
        Some("owned-rotation-1")
    );
    let mut restarted =
        OAuthTokenManager::new(configs, OAuthHttpClient::direct().unwrap(), path.clone());
    restarted.db_path = Some(db_path);
    assert_eq!(
        restarted.get_token("owned").await.unwrap(),
        "owned-token-1",
        "a partial mirror projection must replay its saved token, not issue another rotation"
    );
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert_eq!(
        crate::bindings::load_vendor_oauth(&path).unwrap().configs["owned"]
            .refresh_token
            .as_deref(),
        Some("owned-rotation-1")
    );
}

#[tokio::test]
async fn remaining_oauth_cached_token_cannot_bypass_durable_unknown_rotation() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let config = endpoint.config("authorization_code");
    let manager = Arc::new(
        OAuthTokenManager::from_snapshot(
            HashMap::from([("owned".into(), config.clone())]),
            OAuthHttpClient::direct().unwrap(),
        )
        .with_recovery_db(db.clone()),
    );
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    work.abort();
    let _ = work.await;
    let mut cached = config;
    cached.cached_token = Some("owned-stale-access".into());
    cached.cached_token_expires_at = crate::logging::unix_ts() as i64 + 3600;
    let restored = OAuthTokenManager::from_snapshot(
        HashMap::from([("owned".into(), cached)]),
        OAuthHttpClient::direct().unwrap(),
    )
    .with_recovery_db(db);
    assert!(
        restored.get_token("owned").await.is_err(),
        "a valid-looking memory token cannot skip durable unknown rotation authority"
    );
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn remaining_oauth_provider_runner_uses_the_selected_proxy_for_token_issue() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempdir().unwrap();
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let profiles = HashMap::from([(
        "owned-profile".into(),
        crate::types::ProxyProfile {
            name: "Owned token proxy".into(),
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            username: String::new(),
            password: String::new(),
            enabled: true,
        },
    )]);
    let proxy_pool = crate::component_rt::proxy::ProxyClientPool::new(&profiles).unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let saved = observed.clone();
    let worker = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut bytes = [0; 8192];
            let n = socket.read(&mut bytes).await.unwrap();
            saved.lock().unwrap().push(
                String::from_utf8_lossy(&bytes[..n])
                    .lines()
                    .next()
                    .unwrap_or("")
                    .into(),
            );
            let body = r#"{"error":"owned_proxy_refusal"}"#;
            let response = format!("HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(&root.path().join("owned.sqlite").display().to_string()).unwrap(),
    ));
    let runtime = crate::types::ComponentRuntime {
        template:serde_json::from_value(serde_json::json!({
            "id":"owned-proxy-oauth","name":"Owned","version":"1","type":"text",
            "request":{"method":"POST","url":"http://owned-provider.invalid/translate","body_type":"json","body":{"text":"{{input.text}}"}},
            "response":{"translated_text_path":"text"}
        })).unwrap(),
        auth_values:HashMap::new(),language_map:HashMap::new(),supported_content_formats:vec!["plain_text".into()],
        supported_formats:vec![],supported_business_lines:vec![],key_pool:None,
        oauth_pool:Some(Arc::new(OAuthPool::new(vec![OAuthPoolEntry {
            config_id:"owned".into(),token_field:"access_token".into(),max_concurrent:1,
            max_input_chars:0,max_file_size_mb:0.0,weight:1,active_count:Arc::new(AtomicUsize::new(0)),
        }],KeySelectionStrategy::RoundRobin))),
        oauth_manager:Some(Arc::new(OAuthTokenManager::from_snapshot(
            HashMap::from([("owned".into(),endpoint.config("client_credentials"))]),
            proxy_pool.get_oauth_client(Some("owned-profile")).unwrap().clone()).with_recovery_db(db))),
        proxy_profile_id:Some("owned-profile".into()),runtime_max_concurrent_requests:0,
        runtime_min_interval_ms:0,runtime_concurrency_sem:None,runtime_last_request_at:None,
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(proxy).unwrap())
        .build()
        .unwrap();
    assert!(crate::component_rt::runner::translate_text_via_component(
        &client,
        &runtime,
        "owned source",
        "en",
        "zh"
    )
    .await
    .is_err());
    worker.abort();
    assert!(
        endpoint.requests.lock().unwrap().is_empty(),
        "OAuth token request must not bypass the selected provider proxy"
    );
    assert!(
        observed
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("/token")),
        "selected proxy must observe the token issue"
    );
}

#[tokio::test]
async fn missing_config_returns_error() {
    let manager = OAuthTokenManager::new(
        HashMap::new(),
        OAuthHttpClient::direct().unwrap(),
        "/tmp/test-oauth.json".into(),
    );
    let result = manager.get_token("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn persist_doc_syncs_vendor_oauth_to_db_when_db_path_configured() {
    let _key = crate::db::owned_mock_bindings_key();
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
        OAuthHttpClient::direct().unwrap(),
        oauth_path.to_string_lossy().to_string(),
    );
    manager.db_path = Some(db_path.to_string_lossy().to_string());

    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: configs.clone(),
    };
    manager.persist_doc(&doc).await.unwrap();

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
        OAuthHttpClient::direct().unwrap(),
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
        OAuthHttpClient::direct().unwrap(),
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
        OAuthHttpClient::direct().unwrap(),
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
        OAuthHttpClient::direct().unwrap(),
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
        OAuthHttpClient::direct().unwrap(),
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
        OAuthHttpClient::direct().unwrap(),
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

/// Minimal urlencoded-form field extractor for the token server below.
fn form_field(body: &str, field: &str) -> Option<String> {
    for pair in body.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == field {
            return Some(v.replace("%2F", "/"));
        }
    }
    None
}

/// S3 (doc 16) dedicated case: short-lived tokens (MOCK_TOKEN_TTL_SECONDS=5
/// class) must be refreshed transparently on use, never served stale past
/// their TTL, and the rotated refresh_token must be persisted and used for
/// the next refresh cycle.
#[tokio::test]
async fn short_ttl_token_refreshed_dynamically_with_rotation_persisted() {
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _key = crate::db::TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-oauth-rotation-test-key",
    );
    let dir = tempdir().expect("tempdir");
    let oauth_path = dir.path().join("vendor-oauth.json");
    let oauth_path_str = oauth_path.to_string_lossy().to_string();

    // Local token endpoint mirroring the mock-api OAuth shape: every refresh
    // grant returns a fresh access_token with expires_in=5 and a rotated
    // refresh_token.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<std::sync::Mutex<Vec<(String, String)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let req_log = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let req_log = Arc::clone(&req_log);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut raw = String::new();
                let n = sock.read(&mut buf).await.unwrap_or(0);
                raw.push_str(&String::from_utf8_lossy(&buf[..n]));
                let content_length = raw
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            v.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                while raw
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.len())
                    .unwrap_or(0)
                    < content_length
                {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                let body = raw
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                let grant = form_field(&body, "grant_type").unwrap_or_default();
                let refresh = form_field(&body, "refresh_token").unwrap_or_default();
                let count = {
                    let mut log = req_log.lock().unwrap();
                    log.push((grant, refresh));
                    log.len()
                };
                let resp_body = format!(
                    "{{\"access_token\":\"ttl5-token-{c}\",\"token_type\":\"Bearer\",\"expires_in\":5,\"refresh_token\":\"rot-{c}\"}}",
                    c = count
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut configs = HashMap::new();
    configs.insert(
        "s3-ttl".to_string(),
        OAuthConfig {
            vendor_id: "mock-oauth".into(),
            label: "S3 TTL".into(),
            grant_type: "authorization_code".into(),
            auth_url: String::new(),
            token_url: format!("http://127.0.0.1:{}/token", port),
            client_id: "mock-oauth-client".into(),
            client_secret: "mock-oauth-secret".into(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: Some("stale-token".into()),
            cached_token_expires_at: now - 10, // already expired
            refresh_token: Some("mock-refresh-s3".into()),
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".into(),
        },
    );
    let manager = OAuthTokenManager::new(
        configs,
        OAuthHttpClient::direct().unwrap(),
        oauth_path_str.clone(),
    );

    // First call: expired cache → refresh_token grant with the STORED token.
    let t1 = manager.get_token("s3-ttl").await.unwrap();
    assert_eq!(t1, "ttl5-token-1");

    // Second call: expires_in=5 sits inside the 60s early-refresh margin, so
    // the short-TTL token must be refreshed again instead of served stale —
    // this is the dynamic refresh behavior a TTL=5 mock endpoint requires.
    let t2 = manager.get_token("s3-ttl").await.unwrap();
    assert_eq!(t2, "ttl5-token-2");

    let log = requests.lock().unwrap().clone();
    assert_eq!(
        log.len(),
        2,
        "both calls must refresh (short TTL): {:?}",
        log
    );
    assert_eq!(
        log[0],
        ("refresh_token".to_string(), "mock-refresh-s3".to_string()),
        "first refresh must use the stored refresh_token"
    );
    assert_eq!(
        log[1],
        ("refresh_token".to_string(), "rot-1".to_string()),
        "second refresh must use the ROTATED refresh_token persisted by the first"
    );

    // Persisted doc: newest access token cached and latest rotation stored.
    let stored = std::fs::read(&oauth_path_str).unwrap();
    assert!(
        stored.starts_with(b"WPTC"),
        "OAuth credentials must be encrypted"
    );
    let doc = crate::bindings::load_vendor_oauth(&oauth_path_str).unwrap();
    let entry = doc.configs.get("s3-ttl").expect("persisted config");
    assert_eq!(entry.cached_token.as_deref(), Some("ttl5-token-2"));
    assert_eq!(entry.refresh_token.as_deref(), Some("rot-2"));
}
