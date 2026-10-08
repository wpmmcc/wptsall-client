//! Damaged retained state is an error, never a successful empty or partial page.
use super::*;
use crate::web_ui::test_support::WebUiTestHarness;

fn history_record(conn: &rusqlite::Connection, object_id: i64) -> i64 {
    crate::db::translations::insert_translation_record(
        conn,
        &crate::db::translations::InsertTranslationRecord {
            domain: "https://owned.invalid".into(),
            relation_id: Some(7),
            object_id: Some(object_id),
            object_type: Some("post_type".into()),
            business_line: Some("post_content".into()),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            status: "failed".into(),
            component_ids: vec!["owned".into()],
            ..Default::default()
        },
    )
    .unwrap()
}

#[tokio::test]
async fn retained_state_reads_single_retry_reports_a_pending_noop_without_replacing_identity() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let db = harness.state.lock().await.db.clone();
    let id = history_record(&*db.lock().await, 8);
    let path = format!("/api/translations/{id}/retry");
    let first = harness.post_json(&path, json!({})).await.unwrap();
    assert!(first.status_line.contains("200"));
    assert_eq!(first.body["data"]["queued"], true);
    let before = db
        .lock()
        .await
        .query_row("SELECT id,created_at FROM retry_queue", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap();
    let replay = harness.post_json(&path, json!({})).await.unwrap();
    assert!(replay.status_line.contains("200"));
    assert_eq!(
        replay.body["data"]["queued"], false,
        "already pending is not new work"
    );
    let after = db
        .lock()
        .await
        .query_row("SELECT id,created_at FROM retry_queue", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn retained_state_reads_single_retry_rejects_nonpositive_ids_before_storage() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch("ALTER TABLE translation_records RENAME TO owned_history")
        .unwrap();
    for id in [0, -1] {
        let response = harness
            .post_json(&format!("/api/translations/{id}/retry"), json!({}))
            .await
            .unwrap();
        assert!(
            response.status_line.contains("400"),
            "invalid identity must not read retained storage"
        );
        assert_eq!(response.body["error"]["code"], "INVALID_ID");
    }
}

async fn retained_batch_positive_identity_contract(route: &str) {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let db = harness.state.lock().await.db.clone();
    let id = history_record(&*db.lock().await, 8);
    for invalid in [0, -1] {
        let response = harness
            .post_json(route, json!({"ids":[id,invalid]}))
            .await
            .unwrap();
        assert!(
            response.status_line.contains("400"),
            "invalid mixed batch cannot apply valid prefix"
        );
        let conn = db.lock().await;
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM translation_records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM retry_queue", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn retained_state_reads_batch_retry_rejects_nonpositive_ids_before_mutation() {
    retained_batch_positive_identity_contract("/api/translations/batch-retry").await;
}

#[tokio::test]
async fn retained_state_reads_batch_delete_rejects_nonpositive_ids_before_mutation() {
    retained_batch_positive_identity_contract("/api/translations/batch-delete").await;
}

#[test]
fn retained_state_reads_history_delete_refusal_cannot_confirm_or_partly_delete() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let first = history_record(&conn, 8);
    let second = history_record(&conn, 9);
    conn.execute_batch(&format!(
        "CREATE TRIGGER refuse_owned_history_delete BEFORE DELETE ON translation_records
         WHEN OLD.id={second} BEGIN SELECT RAISE(IGNORE); END"
    ))
    .unwrap();
    assert!(crate::db::translations::batch_delete_translations(&conn, &[first, second]).is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM translation_records", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn retained_state_reads_history_delete_late_chunk_error_rolls_back_all_chunks() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let ids: Vec<_> = (1..=501).map(|id| history_record(&conn, id)).collect();
    conn.execute_batch(&format!(
        "CREATE TRIGGER fail_owned_second_chunk BEFORE DELETE ON translation_records
         WHEN OLD.id={} BEGIN SELECT RAISE(ABORT,'owned second chunk refusal'); END",
        ids[500]
    ))
    .unwrap();
    assert!(crate::db::translations::batch_delete_translations(&conn, &ids).is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM translation_records", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        501,
        "an error in a later delete chunk must retain the earlier chunks"
    );
}

#[test]
fn retained_state_reads_history_delete_requires_absence_after_reported_change() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let id = history_record(&conn, 8);
    conn.execute_batch(
        "CREATE TRIGGER restore_owned_history AFTER DELETE ON translation_records BEGIN
         INSERT INTO translation_records(id,created_at,domain,source_lang,target_lang,status)
         VALUES(OLD.id,OLD.created_at,OLD.domain,OLD.source_lang,OLD.target_lang,OLD.status); END",
    )
    .unwrap();
    assert!(crate::db::translations::batch_delete_translations(&conn, &[id]).is_err());
    assert_eq!(
        crate::db::translations::get_translation_record_by_id(&conn, id)
            .unwrap()
            .unwrap()
            .component_ids,
        vec!["owned".to_string()],
        "failed confirmation must roll back all trigger effects too"
    );
}

#[test]
fn retained_state_reads_core_batches_reject_invalid_identity_without_partial_work() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let id = history_record(&conn, 8);
    for invalid in [0, -1] {
        assert!(crate::db::translations::batch_retry_translations(&conn, &[id, invalid]).is_err());
        assert!(crate::db::translations::batch_delete_translations(&conn, &[id, invalid]).is_err());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM translation_records", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM retry_queue", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn retained_state_reads_history_delete_keeps_unselected_rows_and_outer_rollback() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let first = history_record(&conn, 8);
    let second = history_record(&conn, 9);
    let retained = history_record(&conn, 10);
    conn.execute_batch("BEGIN").unwrap();
    assert_eq!(
        crate::db::translations::batch_delete_translations(&conn, &[first, first, second, 999])
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT id FROM translation_records", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        retained
    );
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM translation_records", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        crate::db::translations::batch_delete_translations(&conn, &[]).unwrap(),
        0
    );
}

#[tokio::test]
async fn retained_state_reads_jobs_missing_table_is_not_empty_or_not_found() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch("ALTER TABLE translation_jobs RENAME TO retained_jobs")
        .unwrap();
    for path in ["/api/jobs", "/api/jobs/1"] {
        let response = harness.get_json(path).await.unwrap();
        assert!(
            response.status_line.contains("500"),
            "damaged jobs must not return success/not-found"
        );
        assert_eq!(response.body["success"], false);
    }
}

#[tokio::test]
async fn retained_state_reads_job_items_cannot_hide_damaged_policy() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let item = seed_translation_item_for_web_ui_test(
        &harness.state,
        "https://owned.invalid",
        "/owned/translated.json",
        "pending_review",
        "owned-read-item",
    )
    .await;
    let job = {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        conn.execute(
            "UPDATE translation_items SET component_ids_json='{' WHERE id=?1",
            [item],
        )
        .unwrap();
        conn.query_row(
            "SELECT job_id FROM translation_items WHERE id=?1",
            [item],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let response = harness.list_job_items(job).await.unwrap();
    assert!(
        response.status_line.contains("500"),
        "a malformed retained row must not disappear"
    );
}

#[tokio::test]
async fn retained_state_reads_review_page_cannot_hide_a_damaged_row() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let damaged = seed_translation_item_for_web_ui_test(
        &harness.state,
        "https://owned.invalid",
        "/owned/translated.json",
        "pending_review",
        "owned-damaged-item",
    )
    .await;
    seed_translation_item_for_web_ui_test(
        &harness.state,
        "https://owned.invalid",
        "/owned/translated.json",
        "pending_review",
        "owned-valid-item",
    )
    .await;
    harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute(
            "UPDATE translation_items SET editable_overrides_json='{' WHERE id=?1",
            [damaged],
        )
        .unwrap();
    let response = harness.get_json("/api/items/pending-review").await.unwrap();
    assert!(
        response.status_line.contains("500"),
        "partial inbox success hides retained work"
    );
}

#[tokio::test]
async fn retained_state_reads_review_count_missing_table_is_not_zero() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch("ALTER TABLE translation_items RENAME TO retained_items")
        .unwrap();
    let response = harness
        .get_json("/api/items/pending-review?count_only=1")
        .await
        .unwrap();
    assert!(
        response.status_line.contains("500"),
        "damaged inbox must not appear empty"
    );
}

#[tokio::test]
async fn retained_state_reads_history_json_damage_is_not_empty_components() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        let id = history_record(&conn, 8);
        conn.execute(
            "UPDATE translation_records SET component_ids_json='{' WHERE id=?1",
            [id],
        )
        .unwrap();
    }
    let response = harness.get_json("/api/translations").await.unwrap();
    assert!(
        response.status_line.contains("500"),
        "malformed history is not valid empty metadata"
    );
}

#[tokio::test]
async fn retained_state_reads_history_row_fault_is_not_a_partial_page() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        history_record(&conn, 8);
        let bad = history_record(&conn, 9);
        conn.execute(
            "UPDATE translation_records SET fields_count='damaged' WHERE id=?1",
            [bad],
        )
        .unwrap();
    }
    let response = harness.get_json("/api/translations").await.unwrap();
    assert!(
        response.status_line.contains("500"),
        "row conversion failure must not be filter_map'ed away"
    );
}

#[test]
fn retained_state_reads_history_large_page_does_not_overflow() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let result = crate::db::translations::query_translation_records(
        &conn,
        &crate::db::translations::TranslationQueryParams {
            page: u32::MAX,
            limit: 100,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.page, u32::MAX);
    assert!(result.records.is_empty());
}

#[tokio::test]
async fn retained_state_reads_batch_retry_refusal_rolls_back_every_enqueue() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let ids = {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        let ids = [history_record(&conn, 8), history_record(&conn, 99)];
        conn.execute_batch(
            "CREATE TRIGGER refuse_owned_retry BEFORE INSERT ON retry_queue
            WHEN NEW.object_id=99 BEGIN SELECT RAISE(IGNORE); END",
        )
        .unwrap();
        ids
    };
    let response = harness
        .post_json("/api/translations/batch-retry", json!({"ids":ids}))
        .await
        .unwrap();
    assert!(
        response.status_line.contains("500"),
        "partial/ignored enqueue is not successful retry"
    );
    let count = harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .query_row("SELECT COUNT(*) FROM retry_queue", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 0, "a refused batch must retain all original state");
}

#[tokio::test]
async fn retained_state_reads_stats_missing_history_is_not_empty_success() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch("ALTER TABLE translation_records RENAME TO retained_history")
        .unwrap();
    let response = harness.get_json("/api/stats/overview").await.unwrap();
    assert!(response.status_line.contains("500"));
}

#[tokio::test]
async fn retained_state_reads_discovery_saved_policy_damage_cannot_be_edited_or_hidden() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let id = {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        crate::db::discovery_tasks::ensure_discovery_tasks(&conn, "https://owned.invalid", &[7])
            .unwrap();
        conn.execute(
            "UPDATE discovery_tasks SET editable_overrides_json='{' WHERE relation_id=7",
            [],
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM discovery_tasks WHERE relation_id=7",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let listed = harness.get_json("/api/discovery-tasks").await.unwrap();
    assert!(listed.status_line.contains("500"));
    let edited = harness
        .put_json(
            &format!("/api/discovery-tasks/{id}"),
            json!({"concurrency":9}),
        )
        .await
        .unwrap();
    assert!(edited.status_line.contains("500"));
    let concurrency = harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .query_row(
            "SELECT concurrency FROM discovery_tasks WHERE id=?1",
            [id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(
        concurrency, 4,
        "damaged original policy must remain untouched"
    );
}

#[tokio::test]
async fn retained_state_reads_discovery_partial_save_refusal_rolls_back_original() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let id = {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        crate::db::discovery_tasks::ensure_discovery_tasks(&conn, "https://owned.invalid", &[7])
            .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_owned_task_timeout BEFORE UPDATE OF timeout_secs
            ON discovery_tasks BEGIN SELECT RAISE(IGNORE); END",
        )
        .unwrap();
        conn.query_row(
            "SELECT id FROM discovery_tasks WHERE relation_id=7",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let response = harness
        .put_json(
            &format!("/api/discovery-tasks/{id}"),
            json!({"concurrency":9,"timeout_secs":11,"enabled":false}),
        )
        .await
        .unwrap();
    assert!(response.status_line.contains("500"));
    let saved = harness
        .state
        .lock()
        .await
        .db
        .lock()
        .await
        .query_row(
            "SELECT concurrency, timeout_secs, enabled FROM discovery_tasks WHERE id=?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(saved, (4, 60, 1));
}

#[test]
fn retained_state_reads_pending_retry_keeps_its_original_identity() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let id = history_record(&conn, 8);
    assert_eq!(
        crate::db::translations::batch_retry_translations(&conn, &[id]).unwrap(),
        1
    );
    let before = conn
        .query_row("SELECT id, created_at FROM retry_queue", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap();
    crate::db::translations::batch_retry_translations(&conn, &[id]).unwrap();
    let after = conn
        .query_row("SELECT id, created_at FROM retry_queue", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap();
    assert_eq!(
        before, after,
        "repeated enqueue must not replace unfinished retry identity"
    );
}

#[test]
fn retained_state_reads_retry_completion_refusal_is_an_error() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let id = history_record(&conn, 8);
    crate::db::translations::batch_retry_translations(&conn, &[id]).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_owned_retry_done BEFORE UPDATE ON retry_queue
        BEGIN SELECT RAISE(IGNORE); END",
    )
    .unwrap();
    assert!(crate::db::translations::mark_retry_done(
        &conn,
        "https://owned.invalid",
        7,
        "post_type",
        8
    )
    .is_err());
}

#[test]
fn retained_state_reads_task_bootstrap_refusal_cannot_leave_a_partial_policy() {
    let conn = crate::db::open_db(":memory:").unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_owned_task_insert BEFORE INSERT ON discovery_tasks
        WHEN NEW.relation_id=8 BEGIN SELECT RAISE(IGNORE); END",
    )
    .unwrap();
    assert!(crate::db::discovery_tasks::ensure_discovery_tasks(
        &conn,
        "https://owned.invalid",
        &[7, 8]
    )
    .is_err());
    let count = conn
        .query_row("SELECT COUNT(*) FROM discovery_tasks", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(
        count, 0,
        "an unconfirmed bootstrap must not partly initialize settings"
    );
}

#[tokio::test]
async fn retained_state_reads_stats_damaged_numeric_authority_is_not_zero() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    {
        let db = harness.state.lock().await.db.clone();
        let conn = db.lock().await;
        history_record(&conn, 8);
        conn.execute(
            "UPDATE translation_records SET fields_count='owned-bad-count'",
            [],
        )
        .unwrap();
    }
    let response = harness.get_json("/api/stats/overview").await.unwrap();
    assert!(response.status_line.contains("500"));
}

#[test]
fn retained_state_reads_retry_preserves_native_object_type_instead_of_post_alias() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let id = history_record(&conn, 8);
    conn.execute(
        "UPDATE translation_records SET object_type='custom_table' WHERE id=?1",
        [id],
    )
    .unwrap();
    crate::db::translations::batch_retry_translations(&conn, &[id]).unwrap();
    let object_type = conn
        .query_row("SELECT object_type FROM retry_queue", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(
        object_type, "custom_table",
        "native identity cannot silently become a post"
    );
}

#[test]
fn retained_state_reads_claim_sql_fault_is_not_a_busy_owner() {
    let conn = crate::db::open_db(":memory:").unwrap();
    conn.execute_batch("ALTER TABLE translation_in_progress RENAME TO retained_claims")
        .unwrap();
    let outcome = format!(
        "{:?}",
        crate::db::discovery_tasks::try_claim_in_progress(&conn, "owned", 7, "post_type", 8)
    );
    assert!(
        outcome.starts_with("Err"),
        "SQL damage must be an explicit failure, not a busy owner: {outcome}"
    );
}

#[test]
fn retained_state_reads_claim_release_refusal_is_not_confirmed() {
    let conn = crate::db::open_db(":memory:").unwrap();
    conn.execute(
        "INSERT INTO translation_in_progress(domain,relation_id,object_type,object_id,claimed_at)
        VALUES ('owned',7,'post_type',8,1)",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_owned_release BEFORE DELETE ON translation_in_progress
        BEGIN SELECT RAISE(IGNORE); END",
    )
    .unwrap();
    let outcome = format!(
        "{:?}",
        crate::db::discovery_tasks::release_in_progress(&conn, "owned", 7, "post_type", 8)
    );
    assert!(
        outcome.starts_with("Err"),
        "retained claim cannot be reported as released: {outcome}"
    );
}

#[test]
fn retained_state_reads_discovery_touch_refusal_is_not_confirmed() {
    let conn = crate::db::open_db(":memory:").unwrap();
    crate::db::discovery_tasks::ensure_discovery_tasks(&conn, "owned", &[7]).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_owned_touch BEFORE UPDATE ON discovery_tasks
        BEGIN SELECT RAISE(IGNORE); END",
    )
    .unwrap();
    let outcome = format!(
        "{:?}",
        crate::db::discovery_tasks::touch_last_run_at(&conn, "owned", 7)
    );
    assert!(
        outcome.starts_with("Err"),
        "last-run projection needs confirmation: {outcome}"
    );
}
