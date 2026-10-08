// catalog: WEBUI-MOD-db-async-jobs-rs
// oracle: L2
use super::*;
use crate::db::async_jobs::{
    begin_provider_intent, close_job, commit_provider_submit_context, mark_job_failed,
    save_provider_job, save_provider_result, touch_polling_job, upsert_polling_job,
};
use serde_json::{json, Value};

fn env(connection: Connection) -> AsyncJobEnv {
    AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(connection)),
        domain: "https://owned.invalid/wp-json/wptsall/v2/owned-private-route/client".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 42,
        field_name: "post_title".into(),
        chunk_index: 0,
        lane: "text",
        source_snapshot: Some(json!({"revision": "owned-private-revision"})),
        resume_binding: Some("owned-private-binding".into()),
    }
}

async fn polling(execution: &ProviderExecution) {
    let ctx = HashMap::from([("computed.job_id".into(), "owned-job".into())]);
    begin_provider_intent(execution, "owned", &ctx, "en", "zh")
        .await
        .unwrap();
    save_provider_job(execution, "owned", "owned-job", &ctx, "en", "zh")
        .await
        .unwrap();
    upsert_polling_job(execution, "owned", "owned-job", &ctx, "en", "zh")
        .await
        .unwrap();
}

async fn state(env: &AsyncJobEnv) -> Value {
    let conn = env.db.lock().await;
    let op: String = conn
        .query_row(
            "SELECT value FROM system_config WHERE key LIKE 'provider-operation-v1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let row: (String, String, i64, String) = conn
        .query_row(
            "SELECT status,ctx_json,attempts,error FROM async_jobs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    json!([op, row])
}

#[tokio::test]
async fn poller_claim_record_changes_fence_every_stale_mutation() {
    let _key = crate::db::owned_mock_bindings_key();
    let env = env(crate::db::open_db(":memory:").unwrap());
    let execution = ProviderExecution::acquire(env.clone()).await.unwrap();
    polling(&execution).await;
    let before = state(&env).await;
    let mut changed: SavedClaim =
        serde_json::from_str(&crate::db::system::decrypt_config_value(&execution.raw).unwrap())
            .unwrap();
    changed.owner = uuid::Uuid::new_v4().to_string();
    let raw =
        crate::db::system::encrypt_config_value(&serde_json::to_string(&changed).unwrap()).unwrap();
    env.db
        .lock()
        .await
        .execute(
            "UPDATE system_config SET value=?1 WHERE key=?2",
            params![raw, execution.key],
        )
        .unwrap();
    let ctx = HashMap::from([("computed.job_id".into(), "owned-job".into())]);
    let outcomes = [
        touch_polling_job(&execution, 2).await,
        mark_job_failed(&execution, "late failure").await,
        close_job(&execution).await,
        save_provider_result(&execution, json!("late output")).await,
        save_provider_job(&execution, "owned", "owned-job", &ctx, "en", "zh").await,
        upsert_polling_job(&execution, "owned", "owned-job", &ctx, "en", "zh").await,
        commit_provider_submit_context(&execution, &ctx).await,
        begin_provider_intent(&execution, "owned", &ctx, "en", "zh").await,
    ];
    for result in outcomes {
        assert!(format!("{:#}", result.unwrap_err()).contains("owner changed"));
        assert_eq!(
            state(&env).await,
            before,
            "stale owner changed durable provider state"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn poller_claim_sixteen_contenders_have_one_live_owner() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.sqlite");
    let env = env(crate::db::open_db(path.to_str().unwrap()).unwrap());
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let (env, barrier) = (env.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            ProviderExecution::acquire(env).await.ok()
        }));
    }
    let mut owners = Vec::new();
    for task in tasks {
        if let Some(owner) = task.await.unwrap() {
            owners.push(owner);
        }
    }
    assert_eq!(owners.len(), 1);
    assert!(ProviderExecution::acquire(env.clone()).await.is_err());
    drop(owners);
    let replacement = ProviderExecution::acquire(env).await.unwrap();
    replacement
        .assert_owner(&*replacement.db.lock().await)
        .unwrap();
}

#[tokio::test]
async fn poller_claim_scope_uses_full_site_unit_but_not_revision_or_runtime() {
    let _key = crate::db::owned_mock_bindings_key();
    let env = env(crate::db::open_db(":memory:").unwrap());
    let owner = ProviderExecution::acquire(env.clone()).await.unwrap();
    let mut revised = env.clone();
    revised.source_snapshot = Some(json!({"revision": "other"}));
    revised.resume_binding = Some("other runtime".into());
    assert!(ProviderExecution::acquire(revised).await.is_err());
    let mut units = vec![env.with_chunk(1), env.with_sub_path("$.title")];
    for dimension in [
        "site",
        "relation",
        "object_type",
        "object_id",
        "field",
        "lane",
    ] {
        let mut other = env.clone();
        match dimension {
            "site" => other.domain =
                "https://owned.invalid/other-install/wp-json/wptsall/v2/owned-private-route/client"
                    .into(),
            "relation" => other.relation_id += 1,
            "object_type" => other.object_type = "taxonomy".into(),
            "object_id" => other.object_id += 1,
            "field" => other.field_name = "post_content".into(),
            "lane" => other.lane = "non_text",
            _ => unreachable!(),
        }
        units.push(other);
    }
    let mut others = Vec::new();
    for unit in units {
        others.push(ProviderExecution::acquire(unit).await.unwrap());
    }
    owner.assert_owner(&*env.db.lock().await).unwrap();
    assert_eq!(others.len(), 8);
}

#[tokio::test]
async fn poller_claim_storage_abort_and_ignore_never_create_an_owner() {
    let _key = crate::db::owned_mock_bindings_key();
    for existing in [false, true] {
        for outcome in ["ABORT,'owned claim storage fault'", "IGNORE"] {
            let env = env(crate::db::open_db(":memory:").unwrap());
            let key = unit_key(&AsyncUnit::from_env(&env)).unwrap();
            if existing {
                drop(ProviderExecution::acquire(env.clone()).await.unwrap());
            }
            let prior =
                crate::db::system::get_system_config_checked(&*env.db.lock().await, &key).unwrap();
            let sql = format!(
                "CREATE TRIGGER deny_claim BEFORE {} ON system_config
                 WHEN NEW.key LIKE 'provider-execution-claim-v1:%'
                 BEGIN SELECT RAISE({outcome}); END;",
                if existing { "UPDATE" } else { "INSERT" }
            );
            env.db.lock().await.execute_batch(&sql).unwrap();
            assert!(ProviderExecution::acquire(env.clone()).await.is_err());
            let conn = env.db.lock().await;
            assert!(conn.is_autocommit());
            assert_eq!(
                crate::db::system::get_system_config_checked(&conn, &key).unwrap(),
                prior
            );
            conn.execute_batch("DROP TRIGGER deny_claim").unwrap();
            drop(conn);
            ProviderExecution::acquire(env).await.unwrap();
        }
    }
}

#[tokio::test]
async fn poller_claim_bad_ciphertext_and_scope_are_retained_not_replaced() {
    let _key = crate::db::owned_mock_bindings_key();
    for damage in [
        "plaintext",
        "ciphertext",
        "scope",
        "owner",
        "format",
        "unknown_field",
    ] {
        let env = env(crate::db::open_db(":memory:").unwrap());
        let owner = ProviderExecution::acquire(env.clone()).await.unwrap();
        let mut saved: Value =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&owner.raw).unwrap())
                .unwrap();
        let key = owner.key.clone();
        drop(owner);
        let raw = match damage {
            "plaintext" => "{}".to_string(),
            "ciphertext" => "V1BUQw-broken".to_string(),
            _ => {
                match damage {
                    "scope" => saved["unit"]["object_id"] = json!(99),
                    "owner" => saved["owner"] = json!("not-a-uuid"),
                    "format" => saved["format"] = json!("future-format"),
                    "unknown_field" => saved["age"] = json!(0),
                    _ => unreachable!(),
                }
                crate::db::system::encrypt_config_value(&saved.to_string()).unwrap()
            }
        };
        env.db
            .lock()
            .await
            .execute(
                "UPDATE system_config SET value=?1 WHERE key=?2",
                params![raw, key],
            )
            .unwrap();
        assert!(
            ProviderExecution::acquire(env.clone()).await.is_err(),
            "{damage}"
        );
        assert_eq!(
            crate::db::system::get_system_config_checked(&*env.db.lock().await, &key).unwrap(),
            Some(raw)
        );
    }
}

#[tokio::test]
async fn poller_claim_ciphertext_retains_private_scope_and_wrong_key_refuses() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.sqlite");
    let env = env(crate::db::open_db(path.to_str().unwrap()).unwrap());
    let owner = ProviderExecution::acquire(env.clone()).await.unwrap();
    assert!(owner.raw.starts_with("V1BUQw"));
    let before = owner.raw.clone();
    drop(owner);
    {
        let _wrong = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-wrong-claim-key",
        );
        assert!(ProviderExecution::acquire(env.clone()).await.is_err());
    }
    let owner = ProviderExecution::acquire(env.clone()).await.unwrap();
    assert_ne!(owner.raw, before, "new owner needs a new durable fence");
    for entry in std::fs::read_dir(root.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        for marker in [
            "owned-private-route",
            "owned-private-revision",
            "owned-private-binding",
        ] {
            assert!(!bytes
                .windows(marker.len())
                .any(|window| window == marker.as_bytes()));
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn poller_claim_aliases_and_descendants_keep_native_liveness() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.sqlite");
    let lease = crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let env = env(crate::db::open_db(path.to_str().unwrap()).unwrap());
    let owner = ProviderExecution::acquire(env.clone()).await.unwrap();
    drop(lease);
    assert!(crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).is_err());
    let alias = root.path().join("alias.sqlite");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let mut other = env.clone();
    other.db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(alias.to_str().unwrap()).unwrap(),
    ));
    assert!(ProviderExecution::acquire(other.clone()).await.is_err());
    drop(owner);
    ProviderExecution::acquire(other).await.unwrap();
    crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let locks = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".poller.lock")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        locks.len(),
        1,
        "canonical aliases must not split native locks"
    );
    assert_eq!(
        locks[0].metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
}
