//! Owned actual media recovery stager; no remote provider or user files.
use super::*;

#[test]
fn encrypted_spool_recovery_copy_never_persists_plaintext() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let store = RecoveryStore::at(root.path().join("store")).unwrap();
    let bytes = b"owned-media-spool-private-content";
    let source = root.path().join("legacy-owned.bin");
    std::fs::write(&source, bytes).unwrap();
    let path = copy_asset(&store, &source).unwrap();
    let stored = std::fs::read(&path).unwrap();
    assert!(
        !stored.windows(bytes.len()).any(|window| window == bytes),
        "encrypted checkpoint must not leave its asset plaintext"
    );
    assert_eq!(
        digest_file(std::path::Path::new(&path)).unwrap(),
        digest_file(&source).unwrap()
    );
    assert_eq!(std::fs::read(source).unwrap(), bytes);
}

async fn retained_case(completed: bool, wrong_key: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let source = root.path().join("original.bin");
    std::fs::write(&source, b"owned retained original").unwrap();
    let (sha256, size) = digest_file(&source).unwrap();
    let store = RecoveryStore::configured().unwrap();
    let asset = copy_asset(&store, &source).unwrap();
    let original = std::fs::read(&asset).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let scope = "owned-encrypted-spool-scope";
    let row = Checkpoint {
        format: "media-session-v1".into(),
        key: checkpoint_key(
            &base,
            "owned-device",
            77,
            22,
            33,
            Some(scope),
            "owned.bin",
            "application/octet-stream",
            &sha256,
        )
        .unwrap(),
        operation_id: uuid::Uuid::new_v4().to_string(),
        wp_base: base.clone(),
        device_id: "owned-device".into(),
        source_id: 77,
        task_id: 22,
        relation_id: 33,
        scope: Some(scope.into()),
        filename: "owned.bin".into(),
        content_type: "application/octet-stream".into(),
        sha256,
        size,
        chunk_size: 5 * 1024 * 1024,
        asset: asset.clone(),
        state: if completed {
            "result_ready"
        } else {
            "prepared"
        }
        .into(),
        upload_id: None,
        attachment_id: completed.then_some(321),
    };
    save(&store, &row).unwrap();
    let receipt = root.path().join("media-recovery").join(format!(
        "{}.receipt",
        crate::sync_engine::hmac::sha256_hex(row.key.as_bytes())
    ));
    let receipt_before = std::fs::read(&receipt).unwrap();
    let mut damaged = original.clone();
    if !wrong_key {
        *damaged.last_mut().unwrap() ^= 0x80;
        std::fs::write(&asset, &damaged).unwrap();
    }
    let _wrong = wrong_key.then(|| {
        crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-wrong-spool-key",
        )
    });
    let config = ChunkedUploadConfig {
        wp_base: base,
        token: "owned-token".into(),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        route_secret: Some("owned".into()),
        chunk_size: 0,
    };
    let result = resume_scope(
        &reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap(),
        &config,
        scope,
    )
    .await;
    if completed {
        assert_eq!(result.unwrap().unwrap().attachment_id, 321);
    } else {
        assert!(result.is_err());
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "damaged or completed retained media must not cause HTTP egress"
    );
    assert_eq!(std::fs::read(receipt).unwrap(), receipt_before);
    assert_eq!(std::fs::read(asset).unwrap(), damaged);
    assert_eq!(std::fs::read(source).unwrap(), b"owned retained original");
}

#[tokio::test]
async fn encrypted_spool_media_corrupt_asset_refuses_before_network_and_keeps_checkpoint() {
    retained_case(false, false).await;
}

#[tokio::test]
async fn encrypted_spool_media_wrong_key_refuses_before_network_and_keeps_checkpoint() {
    retained_case(false, true).await;
}

#[tokio::test]
async fn encrypted_spool_media_completed_receipt_wins_without_asset_or_network_replay() {
    retained_case(true, false).await;
}
