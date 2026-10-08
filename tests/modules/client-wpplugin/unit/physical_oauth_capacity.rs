use super::*;

fn saved_checkpoint(conn: &rusqlite::Connection) -> (String, String, serde_json::Value) {
    let (key, raw): (String, String) = conn
        .query_row(
            "SELECT key,value FROM system_config WHERE key LIKE 'oauth-token-checkpoint-v1:%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let saved =
        serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
    (key, raw, saved)
}

fn force_wal_growth(conn: &rusqlite::Connection, root: &std::path::Path) {
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .unwrap();
    assert_eq!(busy, 0);
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
}

fn lower_policy(root: &std::path::Path) {
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
}

#[tokio::test]
async fn physical_sqlite_oauth_known_reply_finishes_only_original_intent_after_quota_drop() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-oauth-quota-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let path = root.join("owned.sqlite");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(path.to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let manager =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (key, original, owner) = {
        let conn = db.lock().await;
        let (key, raw): (String, String) = conn
            .query_row(
                "SELECT key,value FROM system_config WHERE key LIKE 'oauth-token-checkpoint-v1:%'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(raw.starts_with("V1BUQw"));
        let saved: serde_json::Value =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
        assert_eq!(saved["state"], "intent");
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0, "owned fixture must force new physical WAL growth");
        (key, raw, saved["owner"].clone())
    };
    let wal = root.join("owned.sqlite-wal");
    assert_eq!(wal.metadata().unwrap().len(), 0);
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    assert!(booked > 64 * 1024);
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    endpoint.release.notify_one();
    let issued = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&issued, Ok(token) if token == "owned-token-1"),
        "known original OAuth reply must commit with its prior DB booking: \
         root={} result={issued:?}",
        root.display()
    );
    assert!(wal.metadata().unwrap().len() > 0);
    let raw = crate::db::system::get_system_config_checked(&*db.lock().await, &key)
        .unwrap()
        .unwrap();
    assert_ne!(raw, original);
    let saved: serde_json::Value =
        serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
    assert_eq!(saved["state"], "ready");
    assert_eq!(saved["owner"], owner);
    let remaining = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    assert!(remaining > 0 && remaining < booked);
    let restored = OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
        .with_recovery_db(db.clone());
    assert_eq!(restored.get_token("owned").await.unwrap(), "owned-token-1");
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key)
            .unwrap()
            .unwrap(),
        raw
    );
}

#[tokio::test]
async fn physical_sqlite_oauth_expired_ready_cannot_finance_new_refresh_intent() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("owned.sqlite");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(path.to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let manager =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    assert_eq!(manager.get_token("owned").await.unwrap(), "owned-token-1");
    let (key, raw) = {
        let conn = db.lock().await;
        let (key, _, mut saved) = saved_checkpoint(&conn);
        assert_eq!(saved["state"], "ready");
        saved["expires_at"] = serde_json::json!(0);
        let raw = crate::db::system::encrypt_config_value(&saved.to_string()).unwrap();
        crate::db::system::set_system_config(&conn, &key, &raw).unwrap();
        force_wal_growth(&conn, root.path());
        (key, raw)
    };
    let credit = crate::storage_capacity::database_recovery_credit(&path, false).unwrap();
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    lower_policy(root.path());
    let restored = OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
        .with_recovery_db(db.clone());
    assert!(
        crate::storage_capacity::with_result_credit(credit, restored.get_token("owned"))
            .await
            .is_err(),
        "even ambient DB continuation credit cannot authorize a new token issue"
    );
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key)
            .unwrap()
            .unwrap(),
        raw
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[tokio::test]
async fn physical_sqlite_oauth_busy_root_preserves_unknown_original_reply() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let manager =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (key, raw, saved) = {
        let conn = db.lock().await;
        force_wal_growth(&conn, root.path());
        saved_checkpoint(&conn)
    };
    assert_eq!(saved["state"], "intent");
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    let held = crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap();
    lower_policy(root.path());
    endpoint.release.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    drop(held);
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key)
            .unwrap()
            .unwrap(),
        raw
    );
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
    let restored = OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
        .with_recovery_db(db);
    assert!(restored.get_token("owned").await.is_err());
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn physical_sqlite_oauth_stale_reply_cannot_spend_booking_or_replace_new_owner() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let manager = OAuthTokenManager::from_snapshot(
        HashMap::from([("owned".into(), endpoint.config("authorization_code"))]),
        OAuthHttpClient::direct().unwrap(),
    )
    .with_recovery_db(db.clone());
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (key, raw) = {
        let conn = db.lock().await;
        let (key, _, mut saved) = saved_checkpoint(&conn);
        saved["owner"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
        let raw = crate::db::system::encrypt_config_value(&saved.to_string()).unwrap();
        crate::db::system::set_system_config(&conn, &key, &raw).unwrap();
        force_wal_growth(&conn, root.path());
        (key, raw)
    };
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    lower_policy(root.path());
    endpoint.release.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key)
            .unwrap()
            .unwrap(),
        raw
    );
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[tokio::test]
async fn physical_sqlite_oauth_original_reply_projects_operator_token_after_quota_drop() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-oauth-projection-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: configs.clone(),
    };
    crate::db::system::set_encrypted_config(
        &*db.lock().await,
        "vendor_oauth_doc",
        &serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let manager = OAuthTokenManager::new(
        configs.clone(),
        OAuthHttpClient::direct().unwrap(),
        String::new(),
    )
    .with_recovery_db(db.clone());
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let original_owner = {
        let conn = db.lock().await;
        force_wal_growth(&conn, &root);
        let (_, _, intent) = saved_checkpoint(&conn);
        assert_eq!(intent["state"], "intent");
        intent["owner"].clone()
    };
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    assert!(booked > 64 * 1024);
    lower_policy(&root);
    endpoint.release.notify_one();
    let issued = tokio::time::timeout(std::time::Duration::from_secs(5), work)
        .await
        .unwrap()
        .unwrap();
    {
        let conn = db.lock().await;
        let (_, _, ready) = saved_checkpoint(&conn);
        assert_eq!(
            ready["state"], "ready",
            "original reply must already be durable"
        );
        assert_eq!(ready["owner"], original_owner);
        if issued.is_err() {
            let raw = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap();
            assert_eq!(
                crate::db::system::decrypt_config_value(&raw).unwrap(),
                serde_json::to_string(&doc).unwrap(),
                "failed projection must retain the original operator document"
            );
        }
    }
    assert!(
        matches!(&issued, Ok(token) if token == "owned-token-1"),
        "known original reply must project only its operator token: root={} result={issued:?}",
        root.display()
    );
    let read_projection = || async {
        let conn = db.lock().await;
        let (_, _, ready) = saved_checkpoint(&conn);
        assert_eq!(ready["state"], "ready");
        assert_eq!(ready["owner"], original_owner);
        let raw = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap();
        let doc: crate::types::VendorOAuthDoc =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
        assert_eq!(
            doc.configs["owned"].cached_token.as_deref(),
            Some("owned-token-1")
        );
        assert_eq!(
            doc.configs["owned"].refresh_token.as_deref(),
            Some("owned-rotation-1")
        );
    };
    read_projection().await;
    assert!(root.join("owned.sqlite-wal").metadata().unwrap().len() > 0);
    let remaining = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    assert!(remaining > 0 && remaining < booked);
    let restored =
        OAuthTokenManager::new(configs, OAuthHttpClient::direct().unwrap(), String::new())
            .with_recovery_db(db.clone());
    assert_eq!(restored.get_token("owned").await.unwrap(), "owned-token-1");
    read_projection().await;
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn physical_sqlite_oauth_cancelled_issue_keeps_intent_and_never_reissues_at_quota() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let manager =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    let work = tokio::spawn(async move { manager.get_token("owned").await });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while endpoint.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (key, raw, intent) = {
        let conn = db.lock().await;
        force_wal_growth(&conn, root.path());
        saved_checkpoint(&conn)
    };
    assert_eq!(intent["state"], "intent");
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    lower_policy(root.path());
    work.abort();
    assert!(work.await.unwrap_err().is_cancelled());
    endpoint.release.notify_one();
    let restored = OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
        .with_recovery_db(db.clone());
    assert!(restored.get_token("owned").await.is_err());
    assert_eq!(
        crate::db::system::get_system_config_checked(&*db.lock().await, &key)
            .unwrap()
            .unwrap(),
        raw
    );
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[tokio::test]
async fn physical_sqlite_oauth_changed_ready_cannot_finance_projection() {
    let _key = crate::db::owned_mock_bindings_key();
    for changed in ["owner", "binding", "ciphertext"] {
        let root = tempfile::tempdir().unwrap();
        let _root =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        ));
        let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
        let config = endpoint.config("authorization_code");
        let doc = crate::types::VendorOAuthDoc {
            version: 1,
            configs: HashMap::from([("owned".into(), config.clone())]),
        };
        crate::db::system::set_encrypted_config(
            &*db.lock().await,
            "vendor_oauth_doc",
            &serde_json::to_string(&doc).unwrap(),
        )
        .unwrap();
        let manager = OAuthTokenManager::from_snapshot(
            doc.configs.clone(),
            OAuthHttpClient::direct().unwrap(),
        )
        .with_recovery_db(db.clone());
        assert_eq!(manager.get_token("owned").await.unwrap(), "owned-token-1");
        let execution = super::super::recovery::TokenExecution::acquire(&manager, "owned", &config)
            .await
            .unwrap();
        let mut saved = config.clone();
        execution.restore(&mut saved);
        let (key, raw, operator) = {
            let conn = db.lock().await;
            let (key, _, mut ready) = saved_checkpoint(&conn);
            assert_eq!(ready["state"], "ready");
            ready[changed] = serde_json::json!(uuid::Uuid::new_v4().to_string());
            let raw = if changed == "ciphertext" {
                "V1BUQw-owned-damaged-checkpoint".into()
            } else {
                crate::db::system::encrypt_config_value(&ready.to_string()).unwrap()
            };
            crate::db::system::set_system_config(&conn, &key, &raw).unwrap();
            let operator = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap();
            force_wal_growth(&conn, root.path());
            (key, raw, operator)
        };
        let booked = crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes;
        lower_policy(root.path());
        let mut conn = db.lock().await;
        assert!(
            OAuthTokenManager::project_token_db(
                &mut conn,
                "owned",
                &config,
                &saved,
                false,
                Some(&execution),
            )
            .is_err(),
            "{changed}: stale held execution must not borrow or replace authority"
        );
        assert_eq!(
            crate::db::system::get_system_config_checked(&conn, &key)
                .unwrap()
                .unwrap(),
            raw
        );
        assert_eq!(
            crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap(),
            operator
        );
        assert_eq!(
            crate::storage_capacity::inventory_at(root.path())
                .unwrap()
                .root_booked_bytes,
            booked
        );
        assert_eq!(
            root.path()
                .join("owned.sqlite-wal")
                .metadata()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn physical_sqlite_oauth_db_credit_cannot_finance_operator_file_mirror() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("owned.sqlite");
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(path.to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: configs.clone(),
    };
    let file = root.path().join("owned-oauth.json");
    crate::bindings::save_vendor_oauth(file.to_str().unwrap(), &doc).unwrap();
    let original_file = std::fs::read(&file).unwrap();
    crate::db::system::set_encrypted_config(
        &*db.lock().await,
        "vendor_oauth_doc",
        &serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let frozen =
        OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    assert_eq!(frozen.get_token("owned").await.unwrap(), "owned-token-1");
    let original_ready = {
        let conn = db.lock().await;
        force_wal_growth(&conn, root.path());
        saved_checkpoint(&conn).1
    };
    let credit = crate::storage_capacity::database_recovery_credit(&path, false).unwrap();
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    lower_policy(root.path());
    let manager = OAuthTokenManager::new(
        configs,
        OAuthHttpClient::direct().unwrap(),
        file.to_str().unwrap().into(),
    )
    .with_recovery_db(db.clone());
    assert!(
        crate::storage_capacity::with_result_credit(credit, manager.get_token("owned"))
            .await
            .is_err(),
        "a DB-only booking must not finance a file snapshot, even through ambient scope"
    );
    assert_eq!(std::fs::read(&file).unwrap(), original_file);
    {
        let conn = db.lock().await;
        assert_eq!(saved_checkpoint(&conn).1, original_ready);
        let raw = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap();
        let projected: crate::types::VendorOAuthDoc =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap();
        assert_eq!(
            projected.configs["owned"].cached_token.as_deref(),
            Some("owned-token-1")
        );
    }
    assert!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes
            < booked,
        "only the checked DB projection may spend its own prior booking"
    );
    assert!(manager.get_token("owned").await.is_err());
    assert_eq!(std::fs::read(&file).unwrap(), original_file);
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn physical_sqlite_oauth_original_client_and_jwt_replies_use_only_prior_db_credit() {
    let _key = crate::db::owned_mock_bindings_key();
    for grant in ["client_credentials", "jwt_bearer"] {
        let root = tempfile::tempdir().unwrap();
        let _root =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        ));
        let endpoint = RemainingTokenEndpoint::with_ttl(true, 3600).await;
        let configs = HashMap::from([("owned".into(), endpoint.config(grant))]);
        let manager =
            OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
                .with_recovery_db(db.clone());
        let work = tokio::spawn(async move { manager.get_token("owned").await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while endpoint.requests.lock().unwrap().is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let owner = {
            let conn = db.lock().await;
            let (_, _, intent) = saved_checkpoint(&conn);
            assert_eq!(intent["state"], "intent");
            force_wal_growth(&conn, root.path());
            intent["owner"].clone()
        };
        let booked = crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes;
        assert!(booked > 64 * 1024);
        lower_policy(root.path());
        endpoint.release.notify_one();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), work)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            "owned-token-1"
        );
        let conn = db.lock().await;
        let (_, raw, ready) = saved_checkpoint(&conn);
        assert_eq!(ready["state"], "ready");
        assert_eq!(ready["owner"], owner);
        drop(conn);
        assert!(
            root.path()
                .join("owned.sqlite-wal")
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        let remaining = crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes;
        assert!(remaining > 0 && remaining < booked);
        let restored =
            OAuthTokenManager::from_snapshot(configs, OAuthHttpClient::direct().unwrap())
                .with_recovery_db(db.clone());
        assert_eq!(restored.get_token("owned").await.unwrap(), "owned-token-1");
        assert_eq!(saved_checkpoint(&*db.lock().await).1, raw);
        assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn physical_sqlite_oauth_ready_projection_cannot_overwrite_changed_operator_scope() {
    let _key = crate::db::owned_mock_bindings_key();
    for change in ["credentials", "deleted", "pending-save"] {
        let root = tempfile::tempdir().unwrap();
        let _root =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        ));
        let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
        let config = endpoint.config("authorization_code");
        let mut doc = crate::types::VendorOAuthDoc {
            version: 1,
            configs: HashMap::from([("owned".into(), config.clone())]),
        };
        let configs = doc.configs.clone();
        let frozen =
            OAuthTokenManager::from_snapshot(configs.clone(), OAuthHttpClient::direct().unwrap())
                .with_recovery_db(db.clone());
        assert_eq!(frozen.get_token("owned").await.unwrap(), "owned-token-1");
        match change {
            "credentials" => doc.configs.get_mut("owned").unwrap().client_id = "new-owner".into(),
            "deleted" => doc.configs.clear(),
            "pending-save" => {
                crate::db::system::set_encrypted_config(
                    &*db.lock().await,
                    "integration-save-v1:vendor_oauth_doc",
                    r#"{"owned":"pending-operator-save"}"#,
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let (operator, ready) = {
            let conn = db.lock().await;
            crate::db::system::set_encrypted_config(
                &conn,
                "vendor_oauth_doc",
                &serde_json::to_string(&doc).unwrap(),
            )
            .unwrap();
            force_wal_growth(&conn, root.path());
            (
                crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                    .unwrap()
                    .unwrap(),
                saved_checkpoint(&conn).1,
            )
        };
        let booked = crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes;
        lower_policy(root.path());
        let manager =
            OAuthTokenManager::new(configs, OAuthHttpClient::direct().unwrap(), String::new())
                .with_recovery_db(db.clone());
        assert!(manager.get_token("owned").await.is_err(), "{change}");
        let conn = db.lock().await;
        assert_eq!(saved_checkpoint(&conn).1, ready);
        assert_eq!(
            crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap(),
            operator
        );
        assert_eq!(
            crate::storage_capacity::inventory_at(root.path())
                .unwrap()
                .root_booked_bytes,
            booked
        );
        assert_eq!(
            root.path()
                .join("owned.sqlite-wal")
                .metadata()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn physical_sqlite_oauth_pending_save_after_plan_fences_held_ready_projection() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-oauth-pending-save-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let config = endpoint.config("authorization_code");
    let mut doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([("owned".into(), config.clone())]),
    };
    crate::db::system::set_encrypted_config(
        &*db.lock().await,
        "vendor_oauth_doc",
        &serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let manager =
        OAuthTokenManager::from_snapshot(doc.configs.clone(), OAuthHttpClient::direct().unwrap())
            .with_recovery_db(db.clone());
    assert_eq!(manager.get_token("owned").await.unwrap(), "owned-token-1");
    let execution = super::super::recovery::TokenExecution::acquire(&manager, "owned", &config)
        .await
        .unwrap();
    let mut saved = config.clone();
    execution.restore(&mut saved);
    let (operator, journal, ready) = {
        let conn = db.lock().await;
        assert!(
            !OAuthTokenManager::projection_db_plan(&conn, "owned", &config, Some(&saved)).unwrap()
        );
        let before_digest =
            crate::db::system::private_json_digest(&serde_json::to_value(&doc).unwrap()).unwrap();
        doc.configs.get_mut("owned").unwrap().label = "Operator revision".into();
        let operator =
            crate::db::system::encrypt_config_value(&serde_json::to_string(&doc).unwrap()).unwrap();
        let prepared = serde_json::json!({
            "format":"integration-save-v1",
            "path":root.join("owned-oauth.json").to_str().unwrap(),
            "before_digest":before_digest,
            "after":doc,
            "db_value":operator,
        });
        let journal = crate::db::system::encrypt_config_value(&prepared.to_string()).unwrap();
        crate::db::system::set_system_config(&conn, "vendor_oauth_doc", &operator).unwrap();
        crate::db::system::set_system_config(
            &conn,
            "integration-save-v1:vendor_oauth_doc",
            &journal,
        )
        .unwrap();
        force_wal_growth(&conn, &root);
        (operator, journal, saved_checkpoint(&conn).1)
    };
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    lower_policy(&root);
    let mut conn = db.lock().await;
    let result = OAuthTokenManager::project_token_db(
        &mut conn,
        "owned",
        &config,
        &saved,
        false,
        Some(&execution),
    );
    assert!(
        result.is_err(),
        "pending configuration committed after planning must fence even held Ready: root={}",
        root.display()
    );
    assert_eq!(saved_checkpoint(&conn).1, ready);
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap(),
        operator
    );
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "integration-save-v1:vendor_oauth_doc")
            .unwrap()
            .unwrap(),
        journal
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(&root)
            .unwrap()
            .root_booked_bytes,
        booked
    );
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

async fn projected_replay_preserves_physical_authority(with_file: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::Builder::new()
        .prefix("sqlite-oauth-projected-replay-")
        .tempdir()
        .unwrap()
        .keep();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
    let configs = HashMap::from([("owned".into(), endpoint.config("authorization_code"))]);
    let doc = crate::types::VendorOAuthDoc {
        version: 1,
        configs: configs.clone(),
    };
    crate::db::system::set_encrypted_config(
        &*db.lock().await,
        "vendor_oauth_doc",
        &serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let file = root.join("owned-oauth.json");
    let configured = if with_file {
        crate::bindings::save_vendor_oauth(file.to_str().unwrap(), &doc).unwrap();
        file.to_str().unwrap().to_string()
    } else {
        String::new()
    };
    let first = OAuthTokenManager::new(
        configs.clone(),
        OAuthHttpClient::direct().unwrap(),
        configured.clone(),
    )
    .with_recovery_db(db.clone());
    assert_eq!(first.get_token("owned").await.unwrap(), "owned-token-1");
    let (ready, operator) = {
        let conn = db.lock().await;
        let ready = saved_checkpoint(&conn).1;
        let operator = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap();
        let projected: crate::types::VendorOAuthDoc =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&operator).unwrap())
                .unwrap();
        assert_eq!(
            projected.configs["owned"].cached_token.as_deref(),
            Some("owned-token-1")
        );
        force_wal_growth(&conn, &root);
        (ready, operator)
    };
    let original_file = with_file.then(|| std::fs::read(&file).unwrap());
    let booked = crate::storage_capacity::inventory_at(&root)
        .unwrap()
        .root_booked_bytes;
    lower_policy(&root);
    let restored = OAuthTokenManager::new(configs, OAuthHttpClient::direct().unwrap(), configured)
        .with_recovery_db(db.clone());
    let replay = restored.get_token("owned").await;
    assert!(
        matches!(&replay, Ok(token) if token == "owned-token-1"),
        "already projected Ready must replay without a new write: root={} result={replay:?}",
        root.display()
    );
    let conn = db.lock().await;
    assert_eq!(saved_checkpoint(&conn).1, ready);
    assert_eq!(
        crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
            .unwrap()
            .unwrap(),
        operator,
        "no-op projection must preserve its confirmed physical ciphertext"
    );
    assert_eq!(root.join("owned.sqlite-wal").metadata().unwrap().len(), 0);
    assert_eq!(
        crate::storage_capacity::inventory_at(&root)
            .unwrap()
            .root_booked_bytes,
        booked
    );
    if let Some(original) = original_file {
        assert_eq!(std::fs::read(&file).unwrap(), original);
    }
    assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn physical_sqlite_oauth_projected_db_replay_is_read_only_after_quota_drop() {
    projected_replay_preserves_physical_authority(false).await;
}

#[tokio::test]
async fn physical_sqlite_oauth_projected_mirror_replay_is_read_only_after_quota_drop() {
    projected_replay_preserves_physical_authority(true).await;
}

#[tokio::test]
async fn physical_sqlite_oauth_noop_projection_still_checks_original_authority() {
    let _key = crate::db::owned_mock_bindings_key();
    for changed in ["pending", "credentials", "deleted", "ready_owner"] {
        let root = tempfile::tempdir().unwrap();
        let _root =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        ));
        let endpoint = RemainingTokenEndpoint::with_ttl(false, 3600).await;
        let config = endpoint.config("authorization_code");
        let configs = HashMap::from([("owned".into(), config.clone())]);
        let doc = crate::types::VendorOAuthDoc {
            version: 1,
            configs: configs.clone(),
        };
        crate::db::system::set_encrypted_config(
            &*db.lock().await,
            "vendor_oauth_doc",
            &serde_json::to_string(&doc).unwrap(),
        )
        .unwrap();
        let manager =
            OAuthTokenManager::new(configs, OAuthHttpClient::direct().unwrap(), String::new())
                .with_recovery_db(db.clone());
        assert_eq!(manager.get_token("owned").await.unwrap(), "owned-token-1");
        let execution = super::super::recovery::TokenExecution::acquire(&manager, "owned", &config)
            .await
            .unwrap();
        let mut saved = config.clone();
        execution.restore(&mut saved);
        let (ready, operator, pending) = {
            let conn = db.lock().await;
            let (_, _, checkpoint) = saved_checkpoint(&conn);
            assert_eq!(checkpoint["state"], "ready");
            let operator = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap();
            let mut projected: crate::types::VendorOAuthDoc =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&operator).unwrap())
                    .unwrap();
            assert_eq!(projected.configs["owned"].cached_token, saved.cached_token);
            assert_eq!(
                projected.configs["owned"].cached_token_expires_at,
                saved.cached_token_expires_at
            );
            assert_eq!(
                projected.configs["owned"].refresh_token,
                saved.refresh_token
            );
            match changed {
                "pending" => crate::db::system::set_encrypted_config(
                    &conn,
                    "integration-save-v1:vendor_oauth_doc",
                    r#"{"format":"integration-save-v1","pending":true}"#,
                )
                .unwrap(),
                "credentials" | "deleted" => {
                    if changed == "deleted" {
                        projected.configs.remove("owned");
                    } else {
                        projected.configs.get_mut("owned").unwrap().client_secret =
                            "owned-replaced-credential".into();
                    }
                    crate::db::system::set_encrypted_config(
                        &conn,
                        "vendor_oauth_doc",
                        &serde_json::to_string(&projected).unwrap(),
                    )
                    .unwrap();
                }
                "ready_owner" => {
                    let (key, _, mut checkpoint) = saved_checkpoint(&conn);
                    checkpoint["owner"] = serde_json::json!(uuid::Uuid::new_v4().to_string());
                    crate::db::system::set_encrypted_config(&conn, &key, &checkpoint.to_string())
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let operator = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap();
            let pending = crate::db::system::get_system_config_checked(
                &conn,
                "integration-save-v1:vendor_oauth_doc",
            )
            .unwrap();
            force_wal_growth(&conn, root.path());
            (saved_checkpoint(&conn).1, operator, pending)
        };
        let booked = crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes;
        lower_policy(root.path());
        let mut conn = db.lock().await;
        assert!(
            OAuthTokenManager::project_token_db(
                &mut conn,
                "owned",
                &config,
                &saved,
                false,
                Some(&execution),
            )
            .is_err(),
            "{changed}: a completed logical projection must not bypass authority checks"
        );
        assert_eq!(saved_checkpoint(&conn).1, ready);
        assert_eq!(
            crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")
                .unwrap()
                .unwrap(),
            operator
        );
        assert_eq!(
            crate::db::system::get_system_config_checked(
                &conn,
                "integration-save-v1:vendor_oauth_doc"
            )
            .unwrap(),
            pending
        );
        assert_eq!(
            crate::storage_capacity::inventory_at(root.path())
                .unwrap()
                .root_booked_bytes,
            booked
        );
        assert_eq!(
            root.path()
                .join("owned.sqlite-wal")
                .metadata()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(endpoint.requests.lock().unwrap().len(), 1);
    }
}
