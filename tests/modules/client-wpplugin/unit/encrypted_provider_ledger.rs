//! Actual SQLite provider authority with owned encrypted and legacy assets.
use super::super::test_writes::{begin_provider_intent, save_provider_result};
use super::*;
use std::sync::Arc;

fn environment(path: &std::path::Path) -> AsyncJobEnv {
    AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(path.to_str().unwrap()).unwrap(),
        )),
        domain: "https://owned.invalid".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 42,
        field_name: "owned".into(),
        chunk_index: 0,
        lane: "non_text",
        source_snapshot: None,
        resume_binding: Some("owned-spool-runtime".into()),
    }
}

#[tokio::test]
async fn physical_capacity_provider_ready_bad_readback_does_not_release_root_booking() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let env = environment(&root.path().join("owned.db"));
    begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
        .await
        .unwrap();
    let booked = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    assert!(booked > 0);
    let booking_names = || -> std::collections::BTreeSet<_> {
        std::fs::read_dir(root.path().join(".wptsall-storage-bookings-v1"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect()
    };
    let original_booking_names = booking_names();
    let original_operation = load(&*env.db.lock().await, &env).unwrap().unwrap().0;
    let directory =
        crate::retained_assets::directory(&root.path().join("provider-assets")).unwrap();
    let path = directory.join("wpa1paid.pdf");
    crate::retained_assets::write_bytes(&path, b"owned-paid-before-ready").unwrap();
    let original = std::fs::read(&path).unwrap();
    env.db
        .lock()
        .await
        .execute_batch(
            "CREATE TRIGGER damage_provider_ready AFTER UPDATE ON system_config
             WHEN NEW.key LIKE 'provider-operation-v1:%'
             BEGIN UPDATE system_config SET value='damaged-owned-ready' WHERE key=NEW.key; END;",
        )
        .unwrap();
    assert!(save_provider_result(
        &env,
        json!({"translated_ref":format!("file://{}",path.display()),"translated_text":""}),
    )
    .await
    .is_err());
    let remaining = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    // Rolled-back native WAL growth still consumes its admitted debit. The
    // unpaid/unknown outcome must retain the same booking, not release it.
    assert!(remaining > 0 && remaining <= booked);
    assert_eq!(booking_names(), original_booking_names);
    assert_eq!(
        load(&*env.db.lock().await, &env).unwrap().unwrap().0,
        original_operation
    );
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn physical_capacity_ready_replay_reconciles_only_its_confirmed_unused_booking() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let env = environment(&root.path().join("owned.db"));
    let recovery_booked = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
        .await
        .unwrap();
    let directory =
        crate::retained_assets::directory(&root.path().join("provider-assets")).unwrap();
    let path = directory.join("wpa1paid.pdf");
    crate::retained_assets::write_bytes(&path, b"owned-confirmed-paid").unwrap();
    let original = std::fs::read(&path).unwrap();
    let result = json!({"translated_ref":format!("file://{}",path.display()),"translated_text":""});
    // Admit actual WAL slack before the busy-root injection. Otherwise the
    // new VFS correctly refuses growth before commit, never reaching release.
    {
        let db = env.db.lock().await;
        unsafe {
            let mut wal: *mut rusqlite::ffi::sqlite3_file = std::ptr::null_mut();
            assert_eq!(
                rusqlite::ffi::sqlite3_file_control(
                    db.handle(),
                    c"main".as_ptr(),
                    rusqlite::ffi::SQLITE_FCNTL_JOURNAL_POINTER,
                    (&mut wal as *mut *mut rusqlite::ffi::sqlite3_file).cast()
                ),
                rusqlite::ffi::SQLITE_OK
            );
            assert!(!wal.is_null());
            let methods = &*(*wal).pMethods;
            let mut size = 0;
            assert_eq!(
                methods.xFileSize.unwrap()(wal, &mut size),
                rusqlite::ffi::SQLITE_OK
            );
            assert_eq!(
                methods.xTruncate.unwrap()(wal, size + 65536),
                rusqlite::ffi::SQLITE_OK
            );
        }
    }
    let lease = crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap();
    assert!(save_provider_result(&env, result.clone()).await.is_err());
    let (raw, operation) = load(&*env.db.lock().await, &env).unwrap().unwrap();
    assert_eq!(operation.state, "result_ready");
    assert!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes
            > 0
    );
    drop(lease);
    match recover_provider_operation(&env).await.unwrap() {
        ProviderRecovery::Ready(saved) => assert_eq!(saved, result),
        _ => panic!("exact confirmed Ready must replay without new provider work"),
    }
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        recovery_booked
    );
    assert_eq!(load(&*env.db.lock().await, &env).unwrap().unwrap().0, raw);
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn physical_capacity_busy_root_refuses_unadmitted_ready_growth_and_retains_original() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let env = environment(&root.path().join("owned.db"));
    begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
        .await
        .unwrap();
    let original = load(&*env.db.lock().await, &env).unwrap().unwrap().0;
    let lease = crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap();
    assert!(save_provider_result(
        &env,
        json!({
            "translated_ref":"","translated_text":"owned paid result awaiting admitted growth"
        })
    )
    .await
    .is_err());
    assert_eq!(
        load(&*env.db.lock().await, &env).unwrap().unwrap().0,
        original
    );
    assert!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes
            > 0
    );
    drop(lease);
}

#[tokio::test]
async fn encrypted_spool_provider_ready_reopens_sqlite_with_same_logical_digest_and_uuid() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let directory =
        crate::retained_assets::directory(&root.path().join("provider-assets")).unwrap();
    for encrypted in [false, true] {
        let path = directory.join(if encrypted {
            "wpa1owned.pdf"
        } else {
            "legacy.pdf"
        });
        let bytes = b"%PDF-1.7\nowned-paid-retained-document";
        if encrypted {
            crate::retained_assets::write_bytes(&path, bytes).unwrap();
        } else {
            std::fs::write(&path, bytes).unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        let db_path = root.path().join(format!("owned-{encrypted}.db"));
        let env = environment(&db_path);
        begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
            .await
            .unwrap();
        let result =
            json!({"translated_ref":format!("file://{}", path.display()),"translated_text":""});
        save_provider_result(&env, result.clone()).await.unwrap();
        let (raw, row) = load(&*env.db.lock().await, &env).unwrap().unwrap();
        assert_eq!(
            row.asset_sha256,
            Some(crate::sync_engine::hmac::sha256_hex(bytes))
        );
        assert_eq!(row.state, "result_ready");
        drop(env);
        let restarted = environment(&db_path);
        match recover_provider_operation(&restarted).await.unwrap() {
            ProviderRecovery::Ready(value) => assert_eq!(value, result),
            _ => panic!("original Ready authority must win after restart"),
        }
        let (after, restarted_row) = load(&*restarted.db.lock().await, &restarted)
            .unwrap()
            .unwrap();
        assert_eq!(raw, after);
        assert_eq!(row.attempt_id, restarted_row.attempt_id);
        assert!(
            begin_provider_intent(&restarted, "owned", &HashMap::new(), "en", "zh")
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "legacy and new paid assets stay untouched"
        );
    }
}

#[tokio::test]
async fn encrypted_spool_provider_corruption_and_wrong_key_keep_receipt_and_block_resubmit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let directory =
        crate::retained_assets::directory(&root.path().join("provider-assets")).unwrap();
    let path = directory.join("wpa1owned.pdf");
    crate::retained_assets::write_bytes(&path, b"owned retained result").unwrap();
    let original = std::fs::read(&path).unwrap();
    let env = environment(&root.path().join("owned.db"));
    begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
        .await
        .unwrap();
    save_provider_result(
        &env,
        json!({"translated_ref":format!("file://{}",path.display()),"translated_text":""}),
    )
    .await
    .unwrap();
    let before = load(&*env.db.lock().await, &env).unwrap().unwrap().0;
    {
        let _wrong = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-wrong-spool-key",
        );
        assert!(recover_provider_operation(&env).await.is_err());
        assert!(
            begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    let mut damaged = original.clone();
    *damaged.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &damaged).unwrap();
    assert!(recover_provider_operation(&env).await.is_err());
    assert!(
        begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
            .await
            .is_err()
    );
    assert_eq!(
        load(&*env.db.lock().await, &env).unwrap().unwrap().0,
        before
    );
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
    std::fs::write(&path, original).unwrap();
    assert!(matches!(
        recover_provider_operation(&env).await.unwrap(),
        ProviderRecovery::Ready(_)
    ));
}

#[cfg(unix)]
#[test]
fn encrypted_spool_provider_ledger_does_not_canonicalize_away_encrypted_alias_checks() {
    use std::os::unix::fs::symlink;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let directory =
        crate::retained_assets::directory(&root.path().join("provider-assets")).unwrap();
    let path = directory.join("wpa1owned.bin");
    crate::retained_assets::write_bytes(&path, b"owned").unwrap();
    let alias = directory.join("wpa1alias.bin");
    symlink(&path, &alias).unwrap();
    assert!(asset_digest(&json!({"translated_ref":format!("file://{}",alias.display())})).is_err());
    std::fs::remove_file(alias).unwrap();
    assert_eq!(
        asset_digest(&json!({"translated_ref":format!("file://{}",path.display())})).unwrap(),
        Some(crate::sync_engine::hmac::sha256_hex(b"owned"))
    );
}

#[tokio::test]
async fn physical_capacity_known_paid_job_can_reclaim_poll_and_close_under_lowered_limit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let env = environment(&root.path().join("owned.db"));
    begin_provider_intent(&env, "owned", &HashMap::new(), "en", "zh")
        .await
        .unwrap();
    let ctx = HashMap::from([("computed.job_id".into(), "owned-known-paid-job".into())]);
    super::super::test_writes::save_provider_job(
        &env,
        "owned",
        "owned-known-paid-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    super::super::test_writes::upsert_polling_job(
        &env,
        "owned",
        "owned-known-paid-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    let original = load(&*env.db.lock().await, &env).unwrap().unwrap().1;
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let execution = ProviderExecution::acquire(env.clone())
        .await
        .expect("original paid poller must use only its prior database credit");
    super::super::touch_polling_job(&execution, 1)
        .await
        .unwrap();
    super::save_provider_result(
        &execution,
        json!({
            "translated_ref":"","translated_text":"owned-known-paid-result"
        }),
    )
    .await
    .unwrap();
    super::super::close_job(&execution).await.unwrap();
    let (raw, after) = load(&*env.db.lock().await, &env).unwrap().unwrap();
    assert_eq!(after.attempt_id, original.attempt_id);
    assert_eq!(after.job_id, original.job_id);
    assert_eq!(after.state, "result_ready");
    drop(execution);
    assert!(matches!(
        recover_provider_operation(&env).await.unwrap(),
        ProviderRecovery::Ready(_)
    ));
    assert_eq!(load(&*env.db.lock().await, &env).unwrap().unwrap().0, raw);
    let mut unadmitted = env.clone();
    unadmitted.object_id += 1;
    assert!(ProviderExecution::acquire(unadmitted).await.is_err());
}
