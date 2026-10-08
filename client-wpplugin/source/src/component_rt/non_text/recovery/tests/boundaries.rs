use super::*;

#[cfg(unix)]
#[test]
fn media_recovery_private_files_refuse_aliases_and_public_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("owned");
    let store = RecoveryStore::at(directory.clone()).unwrap();
    let outside = root.path().join("outside");
    std::fs::write(&outside, b"owned outside bytes").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    let receipt = directory.join(format!(
        "{}.receipt",
        crate::sync_engine::hmac::sha256_hex(b"owned")
    ));
    symlink(&outside, &receipt).unwrap();
    assert!(store.load::<Value>("owned").is_err());
    assert!(store
        .update("owned", |_: Option<Value>| Ok((json!({}), ())))
        .is_err());
    assert!(store.list::<Value>().is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"owned outside bytes");
    std::fs::remove_file(&receipt).unwrap();
    for extension in ["active", "lock"] {
        let path = receipt.with_extension(extension);
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        symlink(&outside, &path).unwrap();
        if extension == "active" {
            assert!(store.guard("owned").is_err());
        } else {
            assert!(store
                .update("owned", |_: Option<Value>| Ok((json!({}), ())))
                .is_err());
        }
        std::fs::remove_file(path).unwrap();
    }
    std::fs::hard_link(&outside, &receipt).unwrap();
    assert!(store.load::<Value>("owned").is_err());
    std::fs::remove_file(&receipt).unwrap();
    store
        .update("owned", |_: Option<Value>| {
            Ok((json!({"private":"owned"}), ()))
        })
        .unwrap();
    std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load::<Value>("owned").is_err());
    assert!(store
        .update("owned", |_: Option<Value>| Ok((json!({}), ())))
        .is_err());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(RecoveryStore::at(directory).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"owned outside bytes");
}

#[test]
fn media_recovery_refuses_oversized_empty_and_nonregular_sources() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned");
    std::fs::File::create(&path).unwrap();
    assert!(digest_file(&path).is_err());
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(512 * 1024 * 1024 + 1)
        .unwrap();
    assert!(digest_file(&path).is_err());
    assert!(digest_file(root.path()).is_err());
}

#[test]
fn media_recovery_staging_rechecks_source_bounds_before_copy() {
    let root = tempfile::tempdir().unwrap();
    let store = RecoveryStore::at(root.path().join("store")).unwrap();
    let source = root.path().join("changed-after-hash");
    std::fs::File::create(&source).unwrap();
    assert!(
        copy_asset(&store, &source).is_err(),
        "an emptied source must not be staged"
    );
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(512 * 1024 * 1024 + 1)
        .unwrap();
    assert!(
        copy_asset(&store, &source).is_err(),
        "a grown source must not be staged"
    );
    assert_eq!(
        std::fs::read_dir(store.assets().unwrap()).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn media_recovery_transplanted_encrypted_checkpoint_is_not_another_unit_result() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let asset = root.path().join("original");
    std::fs::write(&asset, b"owned").unwrap();
    let (sha, size) = digest_file(&asset).unwrap();
    let base = "http://127.0.0.1:9/wp-json/wptsall/v2/owned/client";
    let key_for = |source| {
        checkpoint_key(
            base,
            "owned-device",
            source,
            22,
            33,
            Some("owned-transplant"),
            "owned.bin",
            "application/octet-stream",
            &sha,
        )
        .unwrap()
    };
    let key = key_for(77);
    let other_key = key_for(78);
    let other = Checkpoint {
        format: "media-session-v1".into(),
        key: other_key,
        operation_id: uuid::Uuid::new_v4().to_string(),
        wp_base: base.into(),
        device_id: "owned-device".into(),
        source_id: 78,
        task_id: 22,
        relation_id: 33,
        scope: Some("owned-transplant".into()),
        filename: "owned.bin".into(),
        content_type: "application/octet-stream".into(),
        sha256: sha,
        size,
        chunk_size: 5 * 1024 * 1024,
        asset: "owned-retained".into(),
        state: "result_ready".into(),
        upload_id: None,
        attachment_id: Some(321),
    };
    let store = RecoveryStore::configured().unwrap();
    store
        .update(&key, |_: Option<Checkpoint>| Ok((other, ())))
        .unwrap();
    let before = std::fs::read(root_receipt(&key)).unwrap();
    let config = ChunkedUploadConfig {
        wp_base: base.into(),
        token: "owned-recovery-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let result = upload(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &config,
        asset.to_str().unwrap(),
        "owned.bin",
        "application/octet-stream",
        77,
        22,
        33,
        Some("owned-transplant"),
    )
    .await;
    assert!(result.is_err());
    assert!(
        operations().is_err(),
        "a wrong physical receipt key must not appear as valid evidence"
    );
    assert_eq!(std::fs::read(root_receipt(&key)).unwrap(), before);
}

#[tokio::test]
async fn media_recovery_reconciliation_checks_scope_before_post() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let key = checkpoint_key(
        &base,
        "owned-device",
        77,
        22,
        33,
        Some("owned-boundary"),
        "owned.bin",
        "application/octet-stream",
        &"0".repeat(64),
    )
    .unwrap();
    let row = Checkpoint {
        format: "media-session-v1".into(),
        key: key.clone(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        wp_base: base.clone(),
        device_id: "owned-device".into(),
        source_id: 77,
        task_id: 22,
        relation_id: 33,
        scope: Some("owned-boundary".into()),
        filename: "owned.bin".into(),
        content_type: "application/octet-stream".into(),
        sha256: "0".repeat(64),
        size: 1,
        chunk_size: 5 * 1024 * 1024,
        asset: "owned-retained".into(),
        state: "complete_unknown".into(),
        upload_id: None,
        attachment_id: None,
    };
    save(&RecoveryStore::configured().unwrap(), &row).unwrap();
    let operation = row.operation_id.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (headers, _) = read_request(&mut socket).await;
        assert!(headers.starts_with("GET "));
        let body = serde_json::to_vec(&json!({"success":true,"data":{
            "state":"completion_unknown","operation_id":operation,"content_sha256":"0".repeat(64),
            "source_id":78,"task_id":22,"relation_id":33,
        }}))
        .unwrap();
        let sig =
            crate::web_ui::test_support::sign_wp_plaintext_response("owned-recovery-token", &body);
        let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-WPTSALL-Response-Signature: {sig}\r\nConnection: close\r\n\r\n",body.len());
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "no reconciliation POST may precede scope verification"
        );
    });
    let config = ChunkedUploadConfig {
        wp_base: base,
        token: "owned-recovery-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    assert!(reconcile(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &config,
        &row.operation_id,
        None
    )
    .await
    .is_err());
    assert_eq!(operations().unwrap()[0].state, "complete_unknown");
    server.await.unwrap();
}
