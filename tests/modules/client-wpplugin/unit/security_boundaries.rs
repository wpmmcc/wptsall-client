//! Private W2 regressions, compiled against the real shared Client core.

use crate::db::async_jobs::test_writes::upsert_polling_job;
use crate::db::async_jobs::{find_polling_job, AsyncJobEnv};
use crate::db::pending_callbacks::{add_pending_callback, PendingCallbackEntry};
use crate::db::TestEnvVarGuard;
use crate::types::TranslationCallbackPayload;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn callback_entry() -> PendingCallbackEntry {
    PendingCallbackEntry {
        api_base_url: "https://wp.example.invalid".into(),
        idempotency_key: "owned-w2-callback".into(),
        payload: TranslationCallbackPayload {
            schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
            attempt_id: "owned-attempt".into(),
            object_snapshot_hash: "owned-snapshot".into(),
            source_revision: "owned-revision".into(),
            policy_version: "owned-policy".into(),
            field_results: Vec::new(),
            relation_id: 7,
            business_line: "post_content".into(),
            object_type: "post_type".into(),
            subtype: "post".into(),
            object_id: 42,
            translated_fields: HashMap::from([("post_title".into(), "Owned translation".into())]),
            translated_meta: HashMap::new(),
            media_mappings: Vec::new(),
            media_field_sources: HashMap::new(),
            client_task_id: "owned-task".into(),
            outbox_id: None,
            worker_id: "owned-worker".into(),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            execution_time_ms: 0,
        },
        route_secret: Some("owned-route-credential-marker".into()),
        created_at: crate::logging::unix_ts(),
        retry_count: 0,
        last_retry_at: 0,
        relation_id: 7,
        object_id: 42,
        object_type: "post_type".into(),
    }
}

fn async_env() -> AsyncJobEnv {
    AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(":memory:").unwrap(),
        )),
        domain: "https://wp.example.invalid".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 42,
        field_name: "post_title".into(),
        chunk_index: 0,
        lane: "text",
        source_snapshot: None,
        resume_binding: None,
    }
}

#[test]
fn encrypted_atomic_snapshot_preserves_aliases_and_survives_failed_install() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-atomic-key");
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("settings.json");
    crate::bindings::save_encrypted_file(&target, "initial snapshot").unwrap();
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias.json");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        crate::bindings::save_encrypted_file(&alias, "updated through alias").unwrap();
        assert!(std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            crate::bindings::decrypt_from_bytes(&std::fs::read(&target).unwrap()).unwrap(),
            "updated through alias"
        );
        let dangling = dir.path().join("dangling.json");
        std::os::unix::fs::symlink(dir.path().join("absent.json"), &dangling).unwrap();
        assert!(crate::bindings::save_encrypted_file(&dangling, "must not replace link").is_err());
        assert!(std::fs::symlink_metadata(dangling)
            .unwrap()
            .file_type()
            .is_symlink());
    }
    let not_a_file = dir.path().join("protected-directory");
    std::fs::create_dir(&not_a_file).unwrap();
    std::fs::write(not_a_file.join("original"), b"owned original").unwrap();
    assert!(
        crate::bindings::save_encrypted_file(&not_a_file, "must not overwrite directory").is_err()
    );
    assert_eq!(
        std::fs::read(not_a_file.join("original")).unwrap(),
        b"owned original"
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
}

#[test]
fn concurrent_encrypted_snapshots_are_never_partial_or_public() {
    let _key = TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-w2-concurrent-key",
    );
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("WPTSALL_DATA_DIR", dir.path().to_str().unwrap());
    let path = dir.path().join("concurrent.json");
    crate::bindings::save_encrypted_file(&path, r#"{"writer":0}"#).unwrap();
    std::thread::scope(|threads| {
        for writer in 1..=8 {
            let path = &path;
            threads.spawn(move || {
                for _ in 0..8 {
                    crate::bindings::save_encrypted_file(
                        path,
                        &format!(r#"{{"writer":{writer}}}"#),
                    )
                    .unwrap();
                    let bytes = std::fs::read(path).unwrap();
                    let plain = crate::bindings::decrypt_from_bytes(&bytes).unwrap();
                    let value: serde_json::Value = serde_json::from_str(&plain).unwrap();
                    assert!(value["writer"].as_u64().unwrap() <= 8);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        assert_eq!(
                            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                            0o600
                        );
                    }
                }
            });
        }
    });
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != ".wptsall-storage-v1.lock")
            .count(),
        1
    );
    let control = dir.path().join(".wptsall-storage-v1.lock");
    assert!(std::fs::symlink_metadata(&control).unwrap().is_file());
    assert_eq!(std::fs::read(&control).unwrap(), b"");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(control).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn sqlite_and_wal_never_duplicate_new_endpoint_or_resume_credentials() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-disk-key");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.db");
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    let mut callback = callback_entry();
    callback.api_base_url =
        "https://wp.example.invalid/wp-json/wptsall/v2/owned-path-secret-marker/client".into();
    add_pending_callback(&conn, &callback).unwrap();
    let found = crate::db::pending_callbacks::find_pending_callback(
        &conn,
        &callback.api_base_url,
        7,
        "post_type",
        42,
    )
    .unwrap()
    .unwrap();
    assert_eq!(found.api_base_url, callback.api_base_url);
    assert_eq!(found.route_secret, callback.route_secret);
    let env = AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(conn)),
        domain: callback.api_base_url,
        ..async_env()
    };
    let ctx = HashMap::from([("auth.api_key".into(), "owned-disk-auth-marker".into())]);
    upsert_polling_job(&env, "owned-component", "owned-job", &ctx, "en", "zh")
        .await
        .unwrap();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        for marker in [
            "owned-path-secret-marker",
            "owned-route-credential-marker",
            "owned-disk-auth-marker",
        ] {
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "new credentials must not leak into DB, WAL or shared-memory files"
            );
        }
    }
}

#[test]
fn callback_wrong_key_and_damaged_envelope_are_errors_with_rows_retained() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-callback-key");
    let conn = crate::db::open_db(":memory:").unwrap();
    let entry = callback_entry();
    add_pending_callback(&conn, &entry).unwrap();
    {
        let _wrong_key =
            TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-wrong-key");
        assert!(crate::db::pending_callbacks::find_pending_callback(
            &conn,
            &entry.api_base_url,
            7,
            "post_type",
            42
        )
        .is_err());
        assert!(crate::db::pending_callbacks::list_all_pending_callbacks(&conn).is_err());
    }
    conn.execute(
        "UPDATE pending_callbacks SET route_secret_enc='V1BUQw!damaged'",
        [],
    )
    .unwrap();
    assert!(crate::db::pending_callbacks::find_pending_callback(
        &conn,
        &entry.api_base_url,
        7,
        "post_type",
        42
    )
    .is_err());
    assert_eq!(
        crate::db::pending_callbacks::count_pending_for_relation(&conn, &entry.api_base_url, 7)
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn hashed_async_rows_reject_plaintext_downgrade_and_wrong_key() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-resume-key");
    let env = async_env();
    upsert_polling_job(
        &env,
        "owned-component",
        "owned-job",
        &HashMap::new(),
        "en",
        "zh",
    )
    .await
    .unwrap();
    {
        let _wrong_key =
            TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-wrong-key");
        assert!(find_polling_job(&env).await.is_err());
    }
    env.db
        .lock()
        .await
        .execute("UPDATE async_jobs SET ctx_json='{}'", [])
        .unwrap();
    assert!(
        find_polling_job(&env).await.is_err(),
        "new-format jobs cannot be downgraded to plaintext"
    );
    assert_eq!(
        env.db
            .lock()
            .await
            .query_row("SELECT COUNT(*) FROM async_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn async_legacy_crud_is_compatible_but_ambiguous_jobs_fail_closed() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-legacy-key");
    let env = async_env();
    let ctx = HashMap::from([("computed.job_id".into(), "owned-legacy-job".into())]);
    upsert_polling_job(
        &env,
        "owned-component",
        "owned-legacy-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    env.db
        .lock()
        .await
        .execute(
            "UPDATE async_jobs SET domain=?1, ctx_json=?2",
            rusqlite::params![env.domain, serde_json::to_string(&ctx).unwrap()],
        )
        .unwrap();
    assert_eq!(find_polling_job(&env).await.unwrap().unwrap().ctx, ctx);
    crate::db::async_jobs::test_writes::touch_polling_job(&env, 3)
        .await
        .unwrap();
    assert_eq!(find_polling_job(&env).await.unwrap().unwrap().attempts, 3);
    crate::db::async_jobs::test_writes::mark_job_failed(
        &env,
        "owned error ?key=owned-hidden-error-marker",
    )
    .await
    .unwrap();
    let error: String = env
        .db
        .lock()
        .await
        .query_row("SELECT error FROM async_jobs", [], |row| row.get(0))
        .unwrap();
    assert!(!error.contains("owned-hidden-error-marker"));
    upsert_polling_job(
        &env,
        "owned-component",
        "owned-replacement-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    let count: i64 = env
        .db
        .lock()
        .await
        .query_row("SELECT COUNT(*) FROM async_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "replacing a failed legacy unit must not leave two identities"
    );
    crate::db::async_jobs::test_writes::close_job(&env)
        .await
        .unwrap();
    assert!(find_polling_job(&env).await.unwrap().is_none());
}

#[tokio::test]
async fn async_legacy_replacement_rolls_back_on_insert_failure_inside_outer_transaction() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-rollback-key");
    for outer in [false, true] {
        let env = async_env();
        let ctx = HashMap::from([("computed.job_id".into(), "owned-original-job".into())]);
        upsert_polling_job(
            &env,
            "owned-component",
            "owned-original-job",
            &ctx,
            "en",
            "zh",
        )
        .await
        .unwrap();
        env.db
            .lock()
            .await
            .execute(
                "UPDATE async_jobs SET domain=?1, ctx_json=?2, status='failed'",
                rusqlite::params![env.domain, serde_json::to_string(&ctx).unwrap()],
            )
            .unwrap();
        env.db
            .lock()
            .await
            .execute_batch(
                "CREATE TRIGGER owned_insert_failure BEFORE INSERT ON async_jobs
             BEGIN SELECT RAISE(ABORT, 'owned insertion failure'); END;",
            )
            .unwrap();
        if outer {
            env.db.lock().await.execute_batch("BEGIN").unwrap();
        }
        assert!(upsert_polling_job(
            &env,
            "owned-component",
            "owned-replacement-job",
            &ctx,
            "en",
            "zh"
        )
        .await
        .is_err());
        let count: i64 = env
            .db
            .lock()
            .await
            .query_row(
                "SELECT COUNT(*) FROM async_jobs WHERE job_id='owned-original-job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "failed legacy replacement lost its original row; outer={outer}"
        );
        if outer {
            assert!(!env.db.lock().await.is_autocommit());
            env.db.lock().await.execute_batch("COMMIT").unwrap();
        }
    }
}

#[tokio::test]
async fn ambiguous_async_jobs_never_choose_or_overwrite_one_paid_job() {
    let _key = TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-w2-ambiguous-key",
    );
    let env = async_env();
    let ctx = HashMap::new();
    upsert_polling_job(&env, "owned-component", "owned-first-job", &ctx, "en", "zh")
        .await
        .unwrap();
    env.db.lock().await.execute(
        "INSERT INTO async_jobs (domain,relation_id,object_type,object_id,field_name,chunk_index,lane,
         component_id,job_id,ctx_json,status)
         VALUES (?1,7,'post_type',42,'post_title',0,'text','owned-component','owned-competing-job','{}','polling')",
        [&env.domain]).unwrap();
    assert!(
        find_polling_job(&env).await.is_err(),
        "two provider jobs for one unit require explicit recovery"
    );
    assert!(upsert_polling_job(
        &env,
        "owned-component",
        "must-not-replace",
        &ctx,
        "en",
        "zh"
    )
    .await
    .is_err());
    assert!(crate::db::async_jobs::test_writes::close_job(&env)
        .await
        .is_err());
    assert_eq!(
        env.db
            .lock()
            .await
            .query_row("SELECT COUNT(*) FROM async_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn automatic_binding_backup_is_encrypted_private_and_never_reused() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-backup-key");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.json");
    let doc = crate::types::RuleComponentBindingsDoc {
        version: 1,
        ..Default::default()
    };
    let original = serde_json::to_string(&doc).unwrap();
    std::fs::write(&path, &original).unwrap();
    let discovery = crate::bindings::BindingDiscoveryIndex::default();
    let (_, first) =
        crate::bindings::migrate_rule_bindings_v1_to_v2(Some(&path), &doc, &discovery).unwrap();
    let stored = std::fs::read(&first.backup_path).unwrap();
    assert!(
        stored.starts_with(b"WPTC"),
        "new backups must not re-publish legacy plaintext"
    );
    assert_eq!(
        crate::bindings::decrypt_from_bytes(&stored).unwrap(),
        original
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&first.backup_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let (_, second) =
        crate::bindings::migrate_rule_bindings_v1_to_v2(Some(&path), &doc, &discovery).unwrap();
    assert_ne!(first.backup_path, second.backup_path);
    assert_eq!(std::fs::read(first.backup_path).unwrap(), stored);
    let migrated = std::fs::read(path).unwrap();
    assert!(migrated.starts_with(b"WPTC"));
    let migrated: serde_json::Value =
        serde_json::from_str(&crate::bindings::decrypt_from_bytes(&migrated).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2);
}

#[tokio::test]
async fn real_non_text_resume_skips_prepare_upload_source_and_submit() {
    let _key = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-runner-key");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let recorded = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 8192];
            let size = socket.read(&mut bytes).await.unwrap();
            let path = String::from_utf8_lossy(&bytes[..size])
                .lines()
                .next()
                .unwrap()
                .to_string();
            recorded.lock().unwrap().push(path);
            let body = r#"{"status":"done","result":{"video_url":"https://owned.example.invalid/result.mp4"}}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let template = serde_json::from_value(json!({
        "id": "owned-component", "name": "Owned resume", "version": "1.0.0", "type": "video_translation",
        "request": {"method": "POST", "url": format!("{base}/submit"), "body": {}, "body_type": "json"},
        "response": {"translated_ref_path": "result.video_url"},
        "prepare": {"request": {"method": "POST", "url": format!("{base}/prepare"), "body": {}, "body_type": "json"}},
        "source_upload": {"method": "POST", "url": format!("{base}/upload"), "success_statuses": [200]},
        "async_poll": {"job_id_path": "job_id",
            "request": {"method": "GET", "url": format!("{base}/poll/{{{{computed.job_id}}}}"), "body_type": "none"},
            "status_path": "status", "pending_values": ["processing"], "done_values": ["done"],
            "failed_values": ["failed"], "interval_seconds": 1, "timeout_seconds": 3,
            "result_ref_path": "result.video_url"}
    })).unwrap();
    let runtime = crate::types::ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".into()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        proxy_profile_id: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
    };
    let mut env = async_env();
    env.lane = "non_text";
    env.field_name = "media_file".into();
    let env = env
        .for_runtime(
            &runtime,
            &json!({
                "source_text": "", "source_payload": null, "source_ref": format!("{base}/source"),
                "task_type": "video", "key": "media_file",
            }),
            "en",
            "zh",
        )
        .unwrap();
    let ctx = HashMap::from([("computed.job_id".into(), "owned-paid-job".into())]);
    upsert_polling_job(&env, "owned-component", "owned-paid-job", &ctx, "en", "zh")
        .await
        .unwrap();
    let outcome = crate::component_rt::runner::translate_non_text_via_component_with_env(
        &reqwest::Client::new(),
        &runtime,
        "",
        None,
        &format!("{base}/source"),
        "video",
        "media_file",
        "en",
        "zh",
        Some(env.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        outcome.translated_ref,
        "https://owned.example.invalid/result.mp4"
    );
    assert_eq!(
        *requests.lock().unwrap(),
        ["GET /poll/owned-paid-job HTTP/1.1"]
    );
    assert!(find_polling_job(&env).await.unwrap().is_none());
    assert!(
        upsert_polling_job(
            &env,
            "owned-component",
            "owned-damaged-job",
            &ctx,
            "en",
            "zh"
        )
        .await
        .is_err(),
        "a completed paid unit must not be reset for a corruption fixture"
    );
    let mut damaged_env = env.clone();
    damaged_env.db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    ));
    upsert_polling_job(
        &damaged_env,
        "owned-component",
        "owned-damaged-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    damaged_env
        .db
        .lock()
        .await
        .execute("UPDATE async_jobs SET ctx_json='{damaged'", [])
        .unwrap();
    let result = crate::component_rt::runner::translate_non_text_via_component_with_env(
        &reqwest::Client::new(),
        &runtime,
        "",
        None,
        &format!("{base}/source"),
        "video",
        "media_file",
        "en",
        "zh",
        Some(damaged_env),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "damaged resume must make zero provider requests"
    );
    server.abort();
}

#[test]
fn missing_key_refuses_save_and_preserves_previous_bytes() {
    let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "");
    let _device = TestEnvVarGuard::set("WPTSALL_DEVICE_ID", "");
    crate::bindings::clear_default_device_id_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("components.json");
    std::fs::write(&path, b"owned original bytes").unwrap();
    let doc = crate::types::ComponentsLocalDoc {
        version: 1,
        components: HashMap::new(),
    };
    let result = crate::bindings::save_components_local(path.to_str().unwrap(), &doc);
    assert!(result.is_err(), "missing encryption key must fail closed");
    assert_eq!(std::fs::read(path).unwrap(), b"owned original bytes");
}

#[test]
fn local_component_file_is_private_and_encrypted() {
    let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-test-key");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("components.json");
    let doc = crate::types::ComponentsLocalDoc {
        version: 1,
        components: HashMap::new(),
    };
    crate::bindings::save_components_local(path.to_str().unwrap(), &doc).unwrap();
    assert!(std::fs::read(&path).unwrap().starts_with(b"WPTC"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn callback_route_credential_is_encrypted_at_rest() {
    let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-test-key");
    let conn = crate::db::open_db(":memory:").unwrap();
    add_pending_callback(&conn, &callback_entry()).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT route_secret_enc FROM pending_callbacks",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        BASE64.decode(raw).unwrap_or_default().starts_with(b"WPTC"),
        "route_secret_enc must contain an authenticated encrypted envelope"
    );
}

#[tokio::test]
async fn async_resume_context_is_encrypted_at_rest() {
    let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-test-key");
    let env = async_env();
    let ctx = HashMap::from([
        (
            "auth.api_key".into(),
            "owned-async-credential-marker".into(),
        ),
        ("computed.job_id".into(), "owned-provider-job".into()),
    ]);
    upsert_polling_job(
        &env,
        "owned-component",
        "owned-provider-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    let raw: String = env
        .db
        .lock()
        .await
        .query_row("SELECT ctx_json FROM async_jobs", [], |row| row.get(0))
        .unwrap();
    assert!(
        BASE64.decode(raw).unwrap_or_default().starts_with(b"WPTC"),
        "full async context must not persist authentication material in plaintext"
    );
    assert_eq!(find_polling_job(&env).await.unwrap().unwrap().ctx, ctx);
}

#[tokio::test]
async fn corrupt_async_resume_is_error_not_fresh_submission() {
    let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "owned-w2-test-key");
    let env = async_env();
    upsert_polling_job(
        &env,
        "owned-component",
        "owned-provider-job",
        &HashMap::new(),
        "en",
        "zh",
    )
    .await
    .unwrap();
    env.db
        .lock()
        .await
        .execute("UPDATE async_jobs SET ctx_json = '{broken'", [])
        .unwrap();
    assert!(
        find_polling_job(&env).await.is_err(),
        "malformed resume data must stop the caller, not become an empty auth context"
    );
}

#[test]
fn log_redaction_covers_query_credentials_and_embedded_json() {
    let (_log_lock, _state) = crate::logging::acquire_log_state(true, "debug");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.log");
    let detail = json!({
        "key": "owned-log-key-marker",
        "nested": [{"signature": "owned-signature-marker", "access_code": "owned-code-marker"}],
        "url": "https://provider.example.invalid/?key=owned-query-marker&signature=owned-query-signature&format=text",
        "error": "response={\"key\":\"owned-embedded-marker\",\"status\":\"failed\"}"
    });
    crate::logging::log_event(path.to_str().unwrap(), "error", "owned.redaction", detail).unwrap();
    crate::logging::flush_log();
    let raw = std::fs::read_to_string(path).unwrap();
    for marker in [
        "owned-log-key-marker",
        "owned-signature-marker",
        "owned-code-marker",
        "owned-query-marker",
        "owned-query-signature",
        "owned-embedded-marker",
    ] {
        assert!(
            !raw.contains(marker),
            "credential-shaped value crossed the logging boundary"
        );
    }
    assert!(
        raw.contains("format=text"),
        "redaction must keep harmless request diagnostics"
    );
}

#[test]
fn log_redaction_handles_escaped_json_encoded_keys_and_both_wp_routes() {
    let credential = "owned-escape-prefix\\\"owned-escape-suffix";
    let nested_json = serde_json::to_string(&json!({"key": credential, "format": "text"})).unwrap();
    let escaped_json = serde_json::to_string(&nested_json).unwrap();
    for input in [
        format!("response={nested_json}"),
        format!("response={escaped_json}"),
        format!("key=\"{credential}\" format=text"),
        format!(
            "partial \"key\": {}",
            serde_json::to_string(credential).unwrap()
        ),
        "https://provider.example.invalid/?%6B%65%79=owned-encoded-marker&format=text".into(),
        "https://wp.example.invalid/wp-json/wptsall/v2/owned-ats-route-marker/client/ping".into(),
        "https://wp.example.invalid/wp-json/wpmmcc/v1/owned-peer-route-marker/sync/ping".into(),
        "https://wp.example.invalid/wp-json/wpmmcc/v1/owned-peer-route-marker/sync".into(),
        "https://wp.example.invalid/wp-json/wpmmcc/v1/owned-peer-route-marker/sync?format=text"
            .into(),
    ] {
        let output = crate::logging::redact_string_for_log(&input);
        for marker in [
            "owned-escape-prefix",
            "owned-escape-suffix",
            "owned-encoded-marker",
            "owned-ats-route-marker",
            "owned-peer-route-marker",
        ] {
            assert!(
                !output.contains(marker),
                "credential escaped the free-form log boundary"
            );
        }
    }
}

#[tokio::test]
async fn wp_authenticated_client_does_not_follow_cross_origin_redirect() {
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = sink.local_addr().unwrap();
    let sink_task = tokio::spawn(async move {
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(700), sink.accept()).await;
        if let Ok(Ok((mut socket, _))) = accepted {
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await;
            true
        } else {
            false
        }
    });
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = origin.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut socket, _) = origin.accept().await.unwrap();
        let mut request = vec![0; 8192];
        socket.read(&mut request).await.unwrap();
        let response = format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target}/sink\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let client = crate::auth::wp_http_client_builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let response = client
        .post(format!("http://{address}/owned"))
        .header("X-WPTSALL-Client-Token", "owned-transport-token")
        .header("X-WPTSALL-Signature", "owned-transport-signature")
        .body("owned callback")
        .send()
        .await
        .unwrap();
    origin_task.await.unwrap();
    let reached_sink = sink_task.await.unwrap();
    assert!(
        !reached_sink,
        "a redirect forwarded an authenticated WP request to another origin"
    );
    assert!(response.status().is_redirection());
}
