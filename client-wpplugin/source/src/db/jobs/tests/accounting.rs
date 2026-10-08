// catalog: WEBUI-MOD-db-jobs-rs
// oracle: L2
use super::*;

fn assert_projection(conn: &Connection, job_id: i64, expected: (i64, i64, i64)) {
    let job = get_job(conn, job_id).unwrap();
    assert_eq!(
        (job.total_items, job.done_items, job.failed_items),
        expected
    );
    let actual = conn
        .query_row(
            "SELECT COUNT(*),
            COALESCE(SUM(status IN ('done', 'skipped')), 0),
            COALESCE(SUM(status = 'failed'), 0)
         FROM translation_items WHERE job_id = ?1",
            params![job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(expected, actual);
}

#[test]
fn cli06_projection_partitions_states_and_tracks_closed_job_transitions() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    update_job_status(&conn, job_id, "running").unwrap();
    let mut ids = Vec::new();
    for (index, status) in [
        "pending",
        "fetching",
        "fetched",
        "translating",
        "translated",
        "pending_review",
        "syncing",
        "done",
        "skipped",
        "failed",
        "unknown",
    ]
    .into_iter()
    .enumerate()
    {
        let id = create_item(&conn, &sample_item_req(job_id, index as i64 + 1)).unwrap();
        update_item_status(&conn, id, status, Some("fixture retry error")).unwrap();
        ids.push(id);
    }
    assert_projection(&conn, job_id, (11, 2, 1));
    assert_eq!(get_job(&conn, job_id).unwrap().status, "running");
    assert_eq!(
        project_job_from_items(&conn, job_id, true, "completed").unwrap(),
        "partial"
    );
    let progress = get_job_progress(&conn, job_id).unwrap();
    assert_eq!(
        (
            progress.total,
            progress.done,
            progress.failed,
            progress.pending_review,
            progress.translating
        ),
        (11, 2, 1, 1, 7),
    );
    conn.execute(
        "UPDATE translation_jobs SET started_at = 100, completed_at = 200 WHERE id = ?1",
        params![job_id],
    )
    .unwrap();
    for id in ids {
        update_item_status(&conn, id, "done", None).unwrap();
        update_item_status(&conn, id, "done", None).unwrap();
    }
    assert_projection(&conn, job_id, (11, 11, 0));
    assert_eq!(
        project_job_from_items(&conn, job_id, true, "failed").unwrap(),
        "completed"
    );
    let job = get_job(&conn, job_id).unwrap();
    assert_eq!(job.started_at, Some(100));
    assert_eq!(job.completed_at, Some(200));
}

#[test]
fn cli06_dedup_keeps_the_existing_owner_and_does_not_count_phantom_items() {
    let conn = make_db();
    let first = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let second = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let mut request = sample_item_req(first, 42);
    let id = create_item(&conn, &request).unwrap();
    project_job_from_items(&conn, first, true, "completed").unwrap();
    conn.execute(
        "UPDATE translation_jobs SET total_items = 2 WHERE id = ?1",
        params![first],
    )
    .unwrap();
    request.job_id = second;
    assert_eq!(create_item(&conn, &request).unwrap(), id);
    assert_projection(&conn, first, (1, 0, 0));
    assert_projection(&conn, second, (0, 0, 0));
    update_item_status(&conn, id, "done", None).unwrap();
    assert_eq!(get_item(&conn, id).unwrap().job_id, first);
    assert_projection(&conn, first, (1, 1, 0));
    assert_projection(&conn, second, (0, 0, 0));
    assert_eq!(get_job(&conn, first).unwrap().status, "completed");
    assert_eq!(
        project_job_from_items(&conn, second, true, "failed").unwrap(),
        "failed"
    );
    update_item_status(&conn, id, "failed", Some("fixture failure")).unwrap();
    assert_projection(&conn, first, (1, 0, 1));
    assert_projection(&conn, second, (0, 0, 0));
    assert_eq!(get_job(&conn, first).unwrap().status, "failed");
}

#[test]
fn cli06_projection_failure_rolls_back_item_insert_and_status_change() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let id = create_item(&conn, &sample_item_req(job_id, 1)).unwrap();
    let before_item = serde_json::to_value(get_item(&conn, id).unwrap()).unwrap();
    let before_job = serde_json::to_value(get_job(&conn, job_id).unwrap()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_job_projection BEFORE UPDATE ON translation_jobs
         BEGIN SELECT RAISE(ABORT, 'fixture projection failure'); END;",
    )
    .unwrap();
    assert!(create_item(&conn, &sample_item_req(job_id, 2)).is_err());
    assert!(update_item_status(&conn, id, "done", Some("not persisted")).is_err());
    assert_eq!(list_items_by_job(&conn, job_id, None).unwrap().len(), 1);
    assert_eq!(
        serde_json::to_value(get_item(&conn, id).unwrap()).unwrap(),
        before_item
    );
    assert_eq!(
        serde_json::to_value(get_job(&conn, job_id).unwrap()).unwrap(),
        before_job
    );
    assert!(conn.is_autocommit(), "failed savepoints must be released");
    conn.execute_batch("DROP TRIGGER fail_job_projection")
        .unwrap();
    update_item_status(&conn, id, "done", None).unwrap();
    assert_projection(&conn, job_id, (1, 1, 0));
}

#[test]
fn cli06_item_projection_respects_an_outer_transaction_rollback() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let transaction = conn.unchecked_transaction().unwrap();
    let id = create_item(&transaction, &sample_item_req(job_id, 1)).unwrap();
    update_item_status(&transaction, id, "done", None).unwrap();
    assert_projection(&transaction, job_id, (1, 1, 0));
    transaction.rollback().unwrap();
    assert!(get_item(&conn, id).is_none());
    assert_projection(&conn, job_id, (0, 0, 0));
}

#[test]
fn cli06_compound_outcome_failure_rolls_back_metadata_and_projection() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let mut request = sample_item_req(job_id, 1);
    conn.execute_batch(
        "CREATE TRIGGER fail_outcome_path BEFORE UPDATE OF translated_path ON translation_items
         WHEN NEW.translated_path = 'blocked-path'
         BEGIN SELECT RAISE(ABORT, 'fixture path failure'); END;",
    )
    .unwrap();
    assert!(record_item_outcome(&conn, &request, "blocked-path", "done", None).is_err());
    assert!(list_items_by_job(&conn, job_id, None).unwrap().is_empty());
    assert_projection(&conn, job_id, (0, 0, 0));
    let id = record_item_outcome(&conn, &request, "review-path", "pending_review", None).unwrap();
    project_job_from_items(&conn, job_id, true, "completed").unwrap();
    let before_item = serde_json::to_value(get_item(&conn, id).unwrap()).unwrap();
    let before_job = serde_json::to_value(get_job(&conn, job_id).unwrap()).unwrap();
    request.component_id = "new-component".to_string();
    request.component_ids = vec!["new-component".to_string()];
    assert!(record_item_outcome(&conn, &request, "blocked-path", "done", None).is_err());
    conn.execute_batch(
        "DROP TRIGGER fail_outcome_path;
         CREATE TRIGGER fail_outcome_projection BEFORE UPDATE ON translation_jobs
         WHEN NEW.done_items > 0
         BEGIN SELECT RAISE(ABORT, 'fixture terminal projection failure'); END;",
    )
    .unwrap();
    assert!(record_item_outcome(&conn, &request, "done-path", "done", None).is_err());
    assert_eq!(
        serde_json::to_value(get_item(&conn, id).unwrap()).unwrap(),
        before_item
    );
    assert_eq!(
        serde_json::to_value(get_job(&conn, job_id).unwrap()).unwrap(),
        before_job
    );
    assert!(conn.is_autocommit());
    conn.execute_batch("DROP TRIGGER fail_outcome_projection")
        .unwrap();
    assert_eq!(
        record_item_outcome(&conn, &request, "done-path", "done", None).unwrap(),
        id
    );
    assert_projection(&conn, job_id, (1, 1, 0));
    let item = get_item(&conn, id).unwrap();
    assert_eq!(item.translated_path, "done-path");
    assert_eq!(item.component_ids, vec!["new-component"]);
    assert_eq!(item.status, "done");
    assert_eq!(get_job(&conn, job_id).unwrap().status, "completed");
}

#[test]
fn cli06_finalization_repairs_legacy_deltas_and_keeps_empty_failure_semantics() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    update_job_status(&conn, job_id, "running").unwrap();
    let first = create_item(&conn, &sample_item_req(job_id, 1)).unwrap();
    let second = create_item(&conn, &sample_item_req(job_id, 2)).unwrap();
    update_item_status(&conn, first, "done", None).unwrap();
    update_item_status(&conn, second, "failed", Some("fixture failure")).unwrap();
    conn.execute(
        "UPDATE translation_jobs SET done_items = 101, failed_items = 201,
         total_items = 302 WHERE id = ?1",
        params![job_id],
    )
    .unwrap();
    assert_eq!(
        project_job_from_items(&conn, job_id, true, "failed").unwrap(),
        "partial"
    );
    assert_projection(&conn, job_id, (2, 1, 1));
    let empty = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    assert_eq!(
        project_job_from_items(&conn, empty, true, "failed").unwrap(),
        "failed"
    );
    assert_projection(&conn, empty, (0, 0, 0));
    assert!(project_job_from_items(&conn, 999999, true, "completed").is_err());
}

#[test]
fn cli06_language_pack_batch_counts_one_stored_item_not_its_entries() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let mut request = sample_item_req(job_id, 42);
    request.object_type = "language_pack".to_string();
    request.business_line = "plugin_i18n".to_string();
    let id = create_item(&conn, &request).unwrap();
    update_item_status(&conn, id, "translated", Some("callback can retry")).unwrap();
    assert_eq!(
        project_job_from_items(&conn, job_id, true, "completed").unwrap(),
        "partial"
    );
    assert_projection(&conn, job_id, (1, 0, 0));
    update_item_status(&conn, id, "done", None).unwrap();
    assert_projection(&conn, job_id, (1, 1, 0));
    assert_eq!(get_job(&conn, job_id).unwrap().status, "completed");
}

#[test]
fn cli06_wal_writers_keep_snapshot_counters_equal_to_item_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("jobs.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
        .unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    create_tables(&conn).unwrap();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    update_job_status(&conn, job_id, "running").unwrap();
    let ids: Vec<i64> = (1..=3)
        .map(|object_id| create_item(&conn, &sample_item_req(job_id, object_id)).unwrap())
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let writers: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let conn = Connection::open(path).unwrap();
                conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                barrier.wait();
                for round in 0..20 {
                    let status = if round % 2 == 0 { "failed" } else { "done" };
                    update_item_status(&conn, id, status, None).unwrap();
                }
            })
        })
        .collect();
    barrier.wait();
    for _ in 0..40 {
        let snapshot = conn.unchecked_transaction().unwrap();
        let job = get_job(&snapshot, job_id).unwrap();
        assert_projection(
            &snapshot,
            job_id,
            (job.total_items, job.done_items, job.failed_items),
        );
        snapshot.rollback().unwrap();
    }
    for writer in writers {
        writer.join().unwrap();
    }
    assert_eq!(
        project_job_from_items(&conn, job_id, true, "failed").unwrap(),
        "completed"
    );
    assert_projection(&conn, job_id, (3, 3, 0));
}
