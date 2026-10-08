//! Private follow-up probes against the real shared Client store APIs.

use crate::db::async_jobs::test_writes::upsert_polling_job;
use crate::db::async_jobs::{find_polling_job, AsyncJobEnv};
use crate::db::pending_callbacks::{
    add_pending_callback, find_pending_callback, increment_retry_pending_callback,
    list_all_pending_callbacks, remove_pending_callback, PendingCallbackEntry,
};
use crate::db::{open_db, TestEnvVarGuard};
use crate::types::TranslationCallbackPayload;
use rusqlite::{params, Connection};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn owned_entry() -> PendingCallbackEntry {
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "schema_version": crate::config::TASK_CALLBACK_SCHEMA_VERSION,
        "attempt_id": "owned-followup-attempt", "object_snapshot_hash": "owned-followup-snapshot",
        "source_revision": "owned-followup-revision", "policy_version": "owned-followup-policy",
        "relation_id": 7, "business_line": "post_content", "object_type": "post_type",
        "post_type": "post", "object_id": 42,
        "translated_fields": {"post_title": "Owned saved result"}, "translated_meta": {},
        "media_mappings": [], "client_task_id": "owned-followup-task", "worker_id": "owned-followup-worker",
        "source_lang": "en", "target_lang": "zh", "execution_time_ms": 0
    })).unwrap();
    PendingCallbackEntry {
        api_base_url: "https://owned-followup.example.invalid".into(),
        idempotency_key: "owned-followup-callback".into(),
        route_secret: Some("owned-followup-route".into()),
        created_at: crate::logging::unix_ts(),
        retry_count: 0,
        last_retry_at: 0,
        relation_id: 7,
        object_id: 42,
        object_type: "post_type".into(),
        payload,
    }
}

fn ambiguous_callbacks() -> (Connection, PendingCallbackEntry) {
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    conn.execute(
        "INSERT INTO pending_callbacks (api_base_url,idempotency_key,payload_json,route_secret_enc,
         created_at,retry_count,last_retry_at,relation_id,object_id,object_type)
         SELECT ?1,'owned-followup-legacy',payload_json,'owned-followup-legacy-route',
         created_at,retry_count,last_retry_at,relation_id,object_id,object_type FROM pending_callbacks",
        [&entry.api_base_url],
    ).unwrap();
    (conn, entry)
}

#[test]
fn followup_callback_ambiguous_raw_and_hash_never_select_one_result() {
    let _key = crate::db::owned_mock_bindings_key();
    let (conn, entry) = ambiguous_callbacks();
    assert!(find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err());
    assert!(list_all_pending_callbacks(&conn).is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pending_callbacks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn followup_callback_ambiguous_retry_keeps_both_original_rows() {
    let _key = crate::db::owned_mock_bindings_key();
    let (conn, entry) = ambiguous_callbacks();
    assert!(
        increment_retry_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err()
    );
    assert_eq!(
        conn.query_row("SELECT SUM(retry_count) FROM pending_callbacks", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn followup_callback_ambiguous_remove_keeps_both_original_rows() {
    let _key = crate::db::owned_mock_bindings_key();
    let (conn, entry) = ambiguous_callbacks();
    assert!(remove_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pending_callbacks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn followup_callback_new_id_cannot_duplicate_the_same_saved_unit() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let mut entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    entry.idempotency_key = "owned-followup-conflicting-result".into();
    assert!(add_pending_callback(&conn, &entry).is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM pending_callbacks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn followup_callback_idempotency_cannot_move_the_result_to_another_origin() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    let mut moved = entry.clone();
    moved.api_base_url = "https://owned-other-origin.example.invalid".into();
    assert!(add_pending_callback(&conn, &moved).is_err());
    assert!(
        find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42)
            .unwrap()
            .is_some()
    );
}

#[test]
fn followup_callback_scope_columns_must_match_the_saved_payload() {
    let _key = crate::db::owned_mock_bindings_key();
    for legacy in [false, true] {
        for sql in [
            "UPDATE pending_callbacks SET payload_json=json_set(payload_json,'$.object_id',999)",
            "UPDATE pending_callbacks SET retry_count=-1",
            "UPDATE pending_callbacks SET object_type='unregistered_kind'",
        ] {
            let conn = open_db(":memory:").unwrap();
            let entry = owned_entry();
            add_pending_callback(&conn, &entry).unwrap();
            let encoded: String = conn
                .query_row("SELECT payload_json FROM pending_callbacks", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert!(encoded.starts_with("V1BUQw"));
            let logical = crate::db::system::decrypt_config_value(&encoded).unwrap();
            if legacy {
                conn.execute("UPDATE pending_callbacks SET payload_json=?1", [&logical])
                    .unwrap();
            }
            if !legacy && sql.contains("json_set") {
                let mut payload: serde_json::Value = serde_json::from_str(&logical).unwrap();
                payload["object_id"] = json!(999);
                let altered = crate::db::system::encrypt_config_value(
                    &serde_json::to_string(&payload).unwrap(),
                )
                .unwrap();
                conn.execute("UPDATE pending_callbacks SET payload_json=?1", [&altered])
                    .unwrap();
            } else {
                conn.execute(sql, []).unwrap();
            }
            assert!(
                list_all_pending_callbacks(&conn).is_err(),
                "invalid saved callback was handed to recovery"
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM pending_callbacks", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
}

#[test]
fn followup_callback_raw_identity_must_not_hide_an_encrypted_new_endpoint() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    conn.execute(
        "UPDATE pending_callbacks SET api_base_url=?1",
        [&entry.api_base_url],
    )
    .unwrap();
    assert!(find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err());
}

#[test]
fn followup_callback_failed_mutation_rolls_back_with_outer_transaction_retained() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER owned_retry_abort BEFORE UPDATE ON pending_callbacks
                        BEGIN SELECT RAISE(ABORT,'owned update failure'); END; BEGIN;",
    )
    .unwrap();
    assert!(
        increment_retry_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err()
    );
    assert!(!conn.is_autocommit());
    conn.execute_batch("COMMIT").unwrap();
    assert_eq!(
        find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42)
            .unwrap()
            .unwrap()
            .retry_count,
        0
    );
}

#[test]
fn followup_callback_foreign_writer_lock_never_deletes_saved_results() {
    let _key = crate::db::owned_mock_bindings_key();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned-lock.db");
    let conn = open_db(path.to_str().unwrap()).unwrap();
    let other = open_db(path.to_str().unwrap()).unwrap();
    let entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    conn.busy_timeout(std::time::Duration::from_millis(30))
        .unwrap();
    assert!(remove_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err());
    other.execute_batch("ROLLBACK").unwrap();
    assert!(
        find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42)
            .unwrap()
            .is_some()
    );
}

fn owned_async_env(conn: Connection) -> AsyncJobEnv {
    AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(conn)),
        domain: "https://owned-followup.example.invalid".into(),
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

#[tokio::test]
async fn followup_async_separate_connections_never_replace_the_first_paid_job() {
    let _key = TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-followup-async-key",
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned-async.db");
    let env_a = owned_async_env(open_db(path.to_str().unwrap()).unwrap());
    let env_b = owned_async_env(open_db(path.to_str().unwrap()).unwrap());
    let ctx = HashMap::from([("computed.job_id".into(), "owned-first-job".into())]);
    upsert_polling_job(
        &env_a,
        "owned-component",
        "owned-first-job",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    assert!(upsert_polling_job(
        &env_b,
        "owned-component",
        "owned-second-job",
        &ctx,
        "en",
        "zh"
    )
    .await
    .is_err());
    assert_eq!(
        find_polling_job(&env_b).await.unwrap().unwrap().job_id,
        "owned-first-job"
    );
    assert_eq!(
        env_a
            .db
            .lock()
            .await
            .query_row("SELECT COUNT(*) FROM async_jobs", params![], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn followup_callback_identical_retry_keeps_payload_and_retry_progress() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    add_pending_callback(&conn, &entry).unwrap();
    increment_retry_pending_callback(&conn, &entry.api_base_url, 7, "post", 42).unwrap();
    add_pending_callback(&conn, &entry).unwrap();
    let found = find_pending_callback(&conn, &entry.api_base_url, 7, "post", 42)
        .unwrap()
        .unwrap();
    assert_eq!(found.retry_count, 1);
    let mut changed = entry.clone();
    changed.payload.target_lang = "fr".into();
    assert!(add_pending_callback(&conn, &changed).is_err());
    assert_eq!(
        find_pending_callback(&conn, &entry.api_base_url, 7, "post", 42)
            .unwrap()
            .unwrap()
            .payload
            .target_lang,
        "zh"
    );
}

#[test]
fn followup_callback_valid_legacy_and_native_types_remain_readable() {
    let _key = crate::db::owned_mock_bindings_key();
    for object_type in ["term", "language_pack", "option"] {
        let conn = open_db(":memory:").unwrap();
        let mut entry = owned_entry();
        entry.object_type = object_type.into();
        entry.payload.object_type = object_type.into();
        conn.execute(
            "INSERT INTO pending_callbacks (api_base_url,idempotency_key,payload_json,route_secret_enc,
             created_at,retry_count,last_retry_at,relation_id,object_id,object_type)
             VALUES (?1,?2,?3,?4,?5,0,0,7,42,?6)",
            params![entry.api_base_url, entry.idempotency_key, serde_json::to_string(&entry.payload).unwrap(),
                    entry.route_secret, entry.created_at as i64, entry.object_type],
        ).unwrap();
        assert!(
            find_pending_callback(&conn, &entry.api_base_url, 7, object_type, 42)
                .unwrap()
                .is_some()
        );
        increment_retry_pending_callback(&conn, &entry.api_base_url, 7, object_type, 42).unwrap();
        assert!(remove_pending_callback(&conn, &entry.api_base_url, 7, object_type, 42).unwrap());
    }
}

#[test]
fn followup_callback_registered_wire_types_keep_their_identity() {
    let _key = crate::db::owned_mock_bindings_key();
    for object_type in ["option", "media", "menu", "custom_table"] {
        let conn = open_db(":memory:").unwrap();
        let mut entry = owned_entry();
        entry.object_type = object_type.into();
        entry.payload.object_type = object_type.into();
        add_pending_callback(&conn, &entry)
            .expect("registered wire object type must not be coerced or rejected");
        let found = find_pending_callback(&conn, &entry.api_base_url, 7, object_type, 42)
            .unwrap()
            .unwrap();
        assert_eq!(found.object_type, object_type);
        assert_eq!(found.payload.object_type, object_type);
    }
}

#[tokio::test]
async fn followup_async_legacy_render_variables_are_not_envelope_discriminators() {
    let _key = crate::db::owned_mock_bindings_key();
    let env = owned_async_env(open_db(":memory:").unwrap());
    let ctx = HashMap::from([
        ("computed.job_id".into(), "owned-legacy-job".into()),
        ("format".into(), "json".into()),
        ("unit".into(), "pixels".into()),
    ]);
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
            "UPDATE async_jobs SET domain=?1,ctx_json=?2",
            params![env.domain, serde_json::to_string(&ctx).unwrap()],
        )
        .unwrap();
    assert_eq!(find_polling_job(&env).await.unwrap().unwrap().ctx, ctx);
    crate::db::async_jobs::test_writes::touch_polling_job(&env, 1)
        .await
        .unwrap();
    assert_eq!(find_polling_job(&env).await.unwrap().unwrap().attempts, 1);
}

#[tokio::test]
async fn followup_async_damaged_envelope_never_falls_back_to_legacy() {
    let _key = crate::db::owned_mock_bindings_key();
    for damage in [
        "missing_format",
        "wrong_format",
        "missing_unit",
        "unknown_field",
        "string_marker",
    ] {
        let env = owned_async_env(open_db(":memory:").unwrap());
        let ctx = HashMap::from([("computed.job_id".into(), "owned-envelope-job".into())]);
        upsert_polling_job(
            &env,
            "owned-component",
            "owned-envelope-job",
            &ctx,
            "en",
            "zh",
        )
        .await
        .unwrap();
        let stored: String = env
            .db
            .lock()
            .await
            .query_row("SELECT ctx_json FROM async_jobs", [], |row| row.get(0))
            .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&stored).unwrap())
                .unwrap();
        match damage {
            "missing_format" => {
                value.as_object_mut().unwrap().remove("format");
            }
            "wrong_format" => {
                value["format"] = json!("unknown-version");
            }
            "missing_unit" => {
                value.as_object_mut().unwrap().remove("unit");
            }
            "unknown_field" => {
                value["extra"] = json!("owned-unsupported");
            }
            "string_marker" => {
                value = json!({"format": "wptsall-async-context-v1", "unit": "pixels",
                    "computed.job_id": "owned-envelope-job"});
            }
            _ => unreachable!(),
        }
        let damaged = crate::db::system::encrypt_config_value(&value.to_string()).unwrap();
        env.db
            .lock()
            .await
            .execute("UPDATE async_jobs SET ctx_json=?1", [&damaged])
            .unwrap();
        assert!(find_polling_job(&env).await.is_err(), "{damage}");
        assert!(
            crate::db::async_jobs::test_writes::touch_polling_job(&env, 1)
                .await
                .is_err(),
            "{damage}"
        );
        let (retained, attempts): (String, i64) = env
            .db
            .lock()
            .await
            .query_row("SELECT ctx_json,attempts FROM async_jobs", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(retained, damaged, "{damage}");
        assert_eq!(attempts, 0, "{damage}");
    }
}

#[test]
fn followup_callback_zero_row_insert_and_retry_cannot_report_success() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    conn.execute_batch(
        "CREATE TRIGGER owned_ignore_insert BEFORE INSERT ON pending_callbacks
                        BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    assert!(add_pending_callback(&conn, &entry).is_err());
    conn.execute_batch("DROP TRIGGER owned_ignore_insert")
        .unwrap();
    add_pending_callback(&conn, &entry).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER owned_ignore_retry BEFORE UPDATE ON pending_callbacks
                        BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    assert!(
        increment_retry_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42).is_err()
    );
    assert_eq!(
        find_pending_callback(&conn, &entry.api_base_url, 7, "post", 42)
            .unwrap()
            .unwrap()
            .retry_count,
        0
    );
}

fn owned_callback_item(conn: &Connection, entry: &PendingCallbackEntry, target_lang: &str) -> i64 {
    use crate::db::jobs::*;
    let job = create_job(
        conn,
        &CreateJobRequest {
            domain: entry.api_base_url.clone(),
            relation_id: 7,
            business_line: "post_content".into(),
            triggered_by: "manual".into(),
        },
    )
    .unwrap();
    create_item(
        conn,
        &CreateItemRequest {
            job_id: job,
            domain: entry.api_base_url.clone(),
            relation_id: 7,
            business_line: "post_content".into(),
            object_type: "post_type".into(),
            wp_object_id: 42,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: "en".into(),
            target_lang: target_lang.into(),
            component_id: "owned-component".into(),
            component_ids: vec!["owned-component".into()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: String::new(),
            client_task_id: entry.payload.client_task_id.clone(),
            max_retries: 3,
        },
    )
    .unwrap()
}

#[test]
fn followup_callback_completion_refuses_an_unrelated_ack_or_language() {
    let _key = crate::db::owned_mock_bindings_key();
    for mismatch in ["idempotency", "language"] {
        let conn = open_db(":memory:").unwrap();
        let entry = owned_entry();
        let item = owned_callback_item(
            &conn,
            &entry,
            if mismatch == "language" { "fr" } else { "zh" },
        );
        add_pending_callback(&conn, &entry).unwrap();
        let idempotency = if mismatch == "idempotency" {
            "owned-unrelated-ack"
        } else {
            &entry.idempotency_key
        };
        assert!(crate::db::jobs::complete_saved_callback(
            &conn,
            item,
            &entry.api_base_url,
            idempotency,
            r#"{"success":true}"#
        )
        .is_err());
        assert_eq!(
            crate::db::jobs::get_item(&conn, item).unwrap().status,
            "pending"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM translation_records", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(
            find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42)
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn followup_callback_completion_with_saved_identity_remains_atomic_and_legal() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = open_db(":memory:").unwrap();
    let entry = owned_entry();
    let item = owned_callback_item(&conn, &entry, "zh");
    add_pending_callback(&conn, &entry).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    crate::db::jobs::complete_saved_callback(
        &conn,
        item,
        &entry.api_base_url,
        &entry.idempotency_key,
        r#"{"success":true}"#,
    )
    .unwrap();
    assert!(!conn.is_autocommit());
    assert_eq!(
        crate::db::jobs::get_item(&conn, item).unwrap().status,
        "done"
    );
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        crate::db::jobs::get_item(&conn, item).unwrap().status,
        "pending"
    );
    assert!(
        find_pending_callback(&conn, &entry.api_base_url, 7, "post_type", 42)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn followup_async_zero_row_progress_and_close_cannot_report_success() {
    let _key = crate::db::owned_mock_bindings_key();
    let env = owned_async_env(open_db(":memory:").unwrap());
    let ctx = HashMap::from([("computed.job_id".into(), "owned-first-job".into())]);
    upsert_polling_job(&env, "owned-component", "owned-first-job", &ctx, "en", "zh")
        .await
        .unwrap();
    env.db.lock().await.execute_batch(
        "CREATE TRIGGER owned_ignore_async_update BEFORE UPDATE ON async_jobs BEGIN SELECT RAISE(IGNORE); END;
         CREATE TRIGGER owned_ignore_async_delete BEFORE DELETE ON async_jobs BEGIN SELECT RAISE(IGNORE); END;"
    ).unwrap();
    assert!(
        crate::db::async_jobs::test_writes::touch_polling_job(&env, 1)
            .await
            .is_err()
    );
    assert!(crate::db::async_jobs::test_writes::close_job(&env)
        .await
        .is_err());
    assert!(find_polling_job(&env).await.unwrap().is_some());
}

async fn owned_poll_server() -> (
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
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
            let body = r#"{"job_id":"owned-live-job","status":"done","result":{"text":"Owned result","video_url":"https://owned.example.invalid/result.mp4"}}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    (base, requests, server)
}

fn owned_runtime(base: &str, non_text: bool) -> crate::types::ComponentRuntime {
    let template = serde_json::from_value(json!({
        "id":"owned-component", "name":"Owned followup", "version":"1.0.0",
        "type": if non_text { "video_translation" } else { "text_translation" },
        "request":{"method":"POST","url":format!("{base}/submit"),"body":{},"body_type":"json"},
        "response": if non_text { json!({"translated_ref_path":"result.video_url"}) }
                    else { json!({"translated_text_path":"result.text"}) },
        "async_poll":{"job_id_path":"job_id","request":{"method":"GET","url":format!("{base}/poll/{{{{computed.job_id}}}}"),"body_type":"none"},
            "status_path":"status","pending_values":["processing"],"done_values":["done"],"failed_values":["failed"],
            "interval_seconds":1,"timeout_seconds":3,"result_text_path":"result.text","result_ref_path":"result.video_url"}
    })).unwrap();
    crate::types::ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: Vec::new(),
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        proxy_profile_id: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
    }
}

#[tokio::test]
async fn followup_async_wrong_component_or_language_never_reaches_the_provider() {
    let _key = crate::db::owned_mock_bindings_key();
    let (base, requests, server) = owned_poll_server().await;
    for (component, source, target) in [
        ("owned-other-component", "en", "zh"),
        ("owned-component", "fr", "zh"),
        ("owned-component", "en", "fr"),
    ] {
        let mut submitted_runtime = owned_runtime(&base, false);
        submitted_runtime.template.id = component.into();
        let env = owned_async_env(open_db(":memory:").unwrap())
            .for_runtime(&submitted_runtime, &json!("Hello"), source, target)
            .unwrap();
        let ctx = HashMap::from([("computed.job_id".into(), "owned-live-job".into())]);
        upsert_polling_job(&env, component, "owned-live-job", &ctx, source, target)
            .await
            .unwrap();
        let result = crate::component_rt::runner::translate_text_via_component_with_env(
            &reqwest::Client::new(),
            &owned_runtime(&base, false),
            "Hello",
            "en",
            "zh",
            Some(env.clone()),
        )
        .await;
        assert!(
            result.is_err(),
            "resume ignored the saved component or language"
        );
        assert!(
            requests.lock().unwrap().is_empty(),
            "invalid resume must make zero provider requests"
        );
        assert!(find_polling_job(&env).await.unwrap().is_some());
    }
    server.abort();
}

#[tokio::test]
async fn followup_async_template_input_and_source_revision_changes_stop_before_egress() {
    let _key = crate::db::owned_mock_bindings_key();
    let (base, requests, server) = owned_poll_server().await;
    for changed in ["template", "input", "revision"] {
        let runtime = owned_runtime(&base, false);
        let mut env = owned_async_env(open_db(":memory:").unwrap());
        env.source_snapshot =
            Some(json!({"source_revision":"owned-rev-1","policy_version":"owned-policy-1"}));
        let submitted = env
            .for_runtime(&runtime, &json!("Hello"), "en", "zh")
            .unwrap();
        let ctx = HashMap::from([("computed.job_id".into(), "owned-live-job".into())]);
        upsert_polling_job(
            &submitted,
            "owned-component",
            "owned-live-job",
            &ctx,
            "en",
            "zh",
        )
        .await
        .unwrap();
        let mut current = runtime.clone();
        let input = if changed == "input" {
            "Goodbye"
        } else {
            "Hello"
        };
        if changed == "template" {
            current.template.version = "1.0.1".into();
        }
        if changed == "revision" {
            env.source_snapshot =
                Some(json!({"source_revision":"owned-rev-2","policy_version":"owned-policy-1"}));
        }
        let result = crate::component_rt::runner::translate_text_via_component_with_env(
            &reqwest::Client::new(),
            &current,
            input,
            "en",
            "zh",
            Some(env),
        )
        .await;
        assert!(
            result.is_err(),
            "{changed} change reused the paid job's result"
        );
        assert!(requests.lock().unwrap().is_empty());
        assert_eq!(
            submitted
                .db
                .lock()
                .await
                .query_row("SELECT COUNT(*) FROM async_jobs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    server.abort();
}

#[tokio::test]
async fn followup_async_bound_sql_columns_cannot_retarget_a_saved_job() {
    let _key = crate::db::owned_mock_bindings_key();
    for sql in [
        "UPDATE async_jobs SET component_id='owned-other-component'",
        "UPDATE async_jobs SET source_lang='fr'",
        "UPDATE async_jobs SET target_lang='fr'",
        "UPDATE async_jobs SET job_id='owned-other-paid-job'",
        "UPDATE async_jobs SET attempts=-1",
    ] {
        let runtime = owned_runtime("http://127.0.0.1:1", false);
        let env = owned_async_env(open_db(":memory:").unwrap())
            .for_runtime(&runtime, &json!("Hello"), "en", "zh")
            .unwrap();
        let ctx = HashMap::from([("computed.job_id".into(), "owned-live-job".into())]);
        upsert_polling_job(&env, "owned-component", "owned-live-job", &ctx, "en", "zh")
            .await
            .unwrap();
        env.db.lock().await.execute(sql, []).unwrap();
        assert!(
            find_polling_job(&env).await.is_err(),
            "damaged stored identity reached resume"
        );
        assert!(crate::db::async_jobs::test_writes::close_job(&env)
            .await
            .is_err());
        assert_eq!(
            env.db
                .lock()
                .await
                .query_row("SELECT COUNT(*) FROM async_jobs", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[tokio::test]
async fn followup_async_bound_text_resume_skips_current_key_and_oauth_pools() {
    let _key = crate::db::owned_mock_bindings_key();
    let (base, requests, server) = owned_poll_server().await;
    let mut runtime = owned_runtime(&base, false);
    let env = owned_async_env(open_db(":memory:").unwrap())
        .for_runtime(&runtime, &json!("Hello"), "en", "zh")
        .unwrap();
    let ctx = HashMap::from([("computed.job_id".into(), "owned-live-job".into())]);
    upsert_polling_job(&env, "owned-component", "owned-live-job", &ctx, "en", "zh")
        .await
        .unwrap();
    runtime.key_pool = Some(Arc::new(crate::component_rt::key_pool::KeyPool::new(
        Vec::new(),
        crate::types::KeySelectionStrategy::RoundRobin,
    )));
    runtime.oauth_pool = Some(Arc::new(crate::component_rt::oauth::OAuthPool::new(
        Vec::new(),
        crate::types::KeySelectionStrategy::RoundRobin,
    )));
    let dir = tempfile::tempdir().unwrap();
    runtime.oauth_manager = Some(Arc::new(
        crate::component_rt::oauth::OAuthTokenManager::new(
            HashMap::new(),
            crate::component_rt::oauth::OAuthHttpClient::direct().unwrap(),
            dir.path().join("owned-oauth.json").display().to_string(),
        ),
    ));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        crate::component_rt::runner::translate_text_via_component_with_env(
            &reqwest::Client::new(),
            &runtime,
            "Hello",
            "en",
            "zh",
            Some(env),
        ),
    )
    .await
    .expect("resuming must not wait for a new credential")
    .unwrap();
    assert_eq!(result, "Owned result");
    assert_eq!(
        *requests.lock().unwrap(),
        ["GET /poll/owned-live-job HTTP/1.1"]
    );
    assert!(!dir.path().join("owned-oauth.json").exists());
    server.abort();
}

#[tokio::test]
async fn followup_async_same_job_id_cannot_move_to_a_different_component() {
    let _key = crate::db::owned_mock_bindings_key();
    let env = owned_async_env(open_db(":memory:").unwrap());
    let ctx = HashMap::from([("computed.job_id".into(), "owned-first-job".into())]);
    upsert_polling_job(&env, "owned-component", "owned-first-job", &ctx, "en", "zh")
        .await
        .unwrap();
    assert!(upsert_polling_job(
        &env,
        "owned-other-component",
        "owned-first-job",
        &ctx,
        "en",
        "zh"
    )
    .await
    .is_err());
    let component: String = env
        .db
        .lock()
        .await
        .query_row("SELECT component_id FROM async_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(component, "owned-component");
}

#[tokio::test]
async fn followup_async_resume_never_requires_a_new_key_selection_or_source_head() {
    let _key = crate::db::owned_mock_bindings_key();
    let (base, requests, server) = owned_poll_server().await;
    let mut runtime = owned_runtime(&base, true);
    runtime.key_pool = Some(Arc::new(
        crate::component_rt::key_pool::KeyPool::new_with_ext(
            vec![("owned-sized-key".into(), HashMap::new(), 1, 1, 0, 1.0)],
            crate::types::KeySelectionStrategy::RoundRobin,
        ),
    ));
    let mut env = owned_async_env(open_db(":memory:").unwrap());
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
    let ctx = HashMap::from([("computed.job_id".into(), "owned-live-job".into())]);
    upsert_polling_job(&env, "owned-component", "owned-live-job", &ctx, "en", "zh")
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        crate::component_rt::runner::translate_non_text_via_component_with_env(
            &reqwest::Client::new(),
            &runtime,
            "",
            None,
            &format!("{base}/source"),
            "video",
            "media_file",
            "en",
            "zh",
            Some(env),
        ),
    )
    .await
    .expect("owned resume must not hang")
    .unwrap();
    assert_eq!(
        outcome.translated_ref,
        "https://owned.example.invalid/result.mp4"
    );
    assert_eq!(
        *requests.lock().unwrap(),
        ["GET /poll/owned-live-job HTTP/1.1"]
    );
    server.abort();
}
