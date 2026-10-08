// catalog: WEBUI-MOD-db-jobs-rs
// catalog: WEBUI-MOD-task-engine-discoverer-rs
// catalog: WEBUI-MOD-task-engine-discoverer-execute-rs
// oracle: L2
use super::*;

#[derive(Clone, Copy)]
enum AccountingFault {
    None,
    Finalization,
    ItemInsert,
}

async fn run_claimed_item_fixture(
    fail_translation: bool,
    fault: AccountingFault,
    rounds: usize,
    with_outbox: bool,
) -> (
    Vec<crate::db::jobs::TranslationJob>,
    Vec<crate::db::jobs::TranslationItem>,
    anyhow::Result<DomainRunReport>,
) {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().unwrap();
    let _data_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let wp_base = format!("{base}/wp-json/wptsall/v2/secret/client");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0; 8192];
            let header_end = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "incomplete fixture request");
                raw.extend_from_slice(&buffer[..count]);
                assert!(raw.len() < 64 * 1024);
                if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&raw[..header_end]).unwrap().to_string();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while raw.len() < header_end + length {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                raw.extend_from_slice(&buffer[..count]);
            }
            let first = headers.lines().next().unwrap().to_string();
            observed.lock().await.push(first.clone());
            let target = first.split_whitespace().nth(1).unwrap();
            let body = if target.ends_with("/site-relations") {
                json!({ "relations": [{
                    "id": 7, "source_site_id": 1, "source_lang": "en",
                    "target_site_id": 2, "target_site_type": "site",
                    "target_lang": "zh", "sync_mode": "push", "models": []
                }] })
            } else if target.contains("/content-changes/11/ack") {
                json!({ "success": true, "data": {} })
            } else if target.contains("/content-changes") && with_outbox {
                json!({ "success": true, "data": { "items": [{
                    "relation_id": 7, "outbox_id": 11, "task_id": 21,
                    "client_task_id": "fixture-outbox-task",
                    "item": {
                        "object_type": "post_type", "subtype": "post",
                        "object_id": 6001,
                        "complete_data": { "post_title": "fixture text" }
                    }
                }], "schema_version": 1 } })
            } else if target.contains("/content-changes") {
                json!({ "success": true, "data": { "items": [], "schema_version": 1 } })
            } else if target.contains("/rules?") {
                json!({ "rules": [{
                    "id": 1, "model_id": 1, "name": "fixture-post",
                    "data_type": "post", "object_name": "post",
                    "translate_fields": if fail_translation {
                        vec!["post_title"]
                    } else {
                        Vec::<&str>::new()
                    }
                }] })
            } else if target.ends_with("/content/claim") {
                json!({ "claimed_count": 1, "claimed_items": [{
                    "object_id": 6001, "post_type": "post"
                }] })
            } else if target.contains("/content?") {
                json!({
                    "items": [{
                        "object_type": "post_type", "subtype": "post",
                        "object_id": 6001,
                        "complete_data": { "post_title": "fixture text" }
                    }],
                    "total": 1, "page": 1, "per_page": 20
                })
            } else {
                panic!("unexpected fixture request: {first}");
            };
            let body = serde_json::to_vec(&body).unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").unwrap()));
    if matches!(fault, AccountingFault::Finalization) {
        db.lock()
            .await
            .execute_batch(
                "CREATE TRIGGER fail_scan_finalization BEFORE UPDATE ON translation_jobs
             WHEN OLD.status = 'running' AND NEW.status IN ('completed', 'failed', 'partial')
             BEGIN SELECT RAISE(ABORT, 'fixture finalization failure'); END;",
            )
            .unwrap();
    }
    if matches!(fault, AccountingFault::ItemInsert) {
        db.lock()
            .await
            .execute_batch(
                "CREATE TRIGGER fail_item_insert BEFORE INSERT ON translation_items
             BEGIN SELECT RAISE(ABORT, 'fixture item insert failure'); END;",
            )
            .unwrap();
    }
    let pending = Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap()));
    let client = Client::builder().no_proxy().build().unwrap();
    let mut reports = Vec::new();
    for _ in 0..rounds {
        reports.push(
            discover_and_translate(
                &client,
                &wp_base,
                "fixture-token",
                &temp.path().join("worker.log").display().to_string(),
                None,
                None,
                "",
                &[],
                None,
                None,
                &discovery_test_worker_config(),
                Some("secret"),
                &pending,
                None,
                None,
                None,
                Some(Arc::clone(&db)),
                crate::types::PluginIdentity::WpmmccAts,
            )
            .await,
        );
    }
    server.abort();
    let _ = server.await;
    let requests = requests.lock().await;
    if with_outbox {
        assert!(requests
            .iter()
            .any(|line| line.contains("/content-changes/11/ack")));
    } else {
        assert!(requests.iter().any(|line| line.contains("/content/claim")));
    }
    let report = reports.pop().unwrap();
    for earlier in reports {
        earlier.unwrap();
    }
    let conn = db.lock().await;
    let jobs = crate::db::jobs::list_jobs(&conn, None, 10, 0).unwrap();
    if !with_outbox {
        assert_eq!(
            jobs.len(),
            rounds,
            "fixture must enter every relation scan job"
        );
    } else {
        assert_eq!(
            jobs.iter()
                .filter(|job| job.triggered_by == "outbox")
                .count(),
            1
        );
    }
    let items = jobs
        .iter()
        .flat_map(|job| crate::db::jobs::list_items_by_job(&conn, job.id, None).unwrap())
        .collect();
    (jobs, items, report)
}

async fn run_single_claimed_item(
    fail_translation: bool,
    fail_finalization: bool,
) -> (
    crate::db::jobs::TranslationJob,
    Vec<crate::db::jobs::TranslationItem>,
    anyhow::Result<DomainRunReport>,
) {
    let fault = if fail_finalization {
        AccountingFault::Finalization
    } else {
        AccountingFault::None
    };
    let (mut jobs, items, report) =
        run_claimed_item_fixture(fail_translation, fault, 1, false).await;
    (jobs.pop().unwrap(), items, report)
}

#[tokio::test]
async fn cli06_relation_no_change_counts_the_persisted_item_once() {
    let (job, items, report) = run_single_claimed_item(false, false).await;
    let report = report.unwrap();
    assert_eq!(report.processed, 1);
    assert_eq!(
        report.completed, 0,
        "run report counts callback submissions, not terminal items"
    );
    assert_eq!(report.failed, 0);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "done");
    assert_eq!(
        (job.total_items, job.done_items, job.failed_items),
        (1, 1, 0),
        "job counters must equal its persisted item states, not two lane deltas"
    );
    assert_eq!(job.status, "completed", "a no-change item is terminal");
}

#[tokio::test]
async fn cli06_relation_failure_counts_the_persisted_item_once() {
    let (job, items, report) = run_single_claimed_item(true, false).await;
    let report = report.unwrap();
    assert_eq!(report.processed, 1);
    assert_eq!(report.failed, 1);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "failed");
    assert!(items[0]
        .error_message
        .as_deref()
        .unwrap()
        .contains("no component runtime available"));
    assert_eq!(
        (job.total_items, job.done_items, job.failed_items),
        (1, 0, 1),
        "job counters must equal its persisted item states, not two lane deltas"
    );
    assert_eq!(job.status, "failed");
}

#[tokio::test]
async fn cli06_relation_projection_failure_is_not_reported_as_success() {
    let (job, items, report) = run_single_claimed_item(false, true).await;
    let report = report.unwrap();
    assert_eq!(
        report.failed, 1,
        "the failed finalization must enter the run failure report"
    );
    assert_eq!(report.completed, 0);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "done");
    assert_eq!(
        (job.total_items, job.done_items, job.failed_items),
        (1, 1, 0)
    );
    assert_eq!(
        job.status, "running",
        "the failed finalization must not partly commit"
    );
}

#[tokio::test]
async fn cli06_repeated_relation_scan_has_no_phantom_items_or_partial_no_change_job() {
    let (jobs, items, report) =
        run_claimed_item_fixture(false, AccountingFault::None, 2, false).await;
    assert_eq!(report.unwrap().failed, 0);
    assert_eq!(
        items.len(),
        1,
        "cross-job identity dedup must keep one item"
    );
    let owner = jobs.iter().find(|job| job.id == items[0].job_id).unwrap();
    let empty = jobs.iter().find(|job| job.id != items[0].job_id).unwrap();
    assert_eq!(
        (owner.total_items, owner.done_items, owner.failed_items),
        (1, 1, 0)
    );
    assert_eq!(
        (empty.total_items, empty.done_items, empty.failed_items),
        (0, 0, 0)
    );
    assert_eq!(owner.status, "completed");
    assert_eq!(
        empty.status, "completed",
        "no owned pending item means no partial no-change job"
    );
}

#[tokio::test]
async fn cli06_outbox_no_change_owns_one_terminal_item_and_scan_dedup_owns_none() {
    let (jobs, items, report) =
        run_claimed_item_fixture(false, AccountingFault::None, 1, true).await;
    assert_eq!(report.unwrap().failed, 0);
    let job = jobs
        .iter()
        .find(|job| job.triggered_by == "outbox")
        .unwrap();
    assert_eq!(
        (job.total_items, job.done_items, job.failed_items),
        (1, 1, 0)
    );
    assert_eq!(job.status, "completed");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].job_id, job.id);
    assert_eq!(items[0].status, "done");
    for scan in jobs.iter().filter(|job| job.triggered_by != "outbox") {
        assert_eq!(
            (scan.total_items, scan.done_items, scan.failed_items),
            (0, 0, 0)
        );
        assert_eq!(scan.status, "completed");
    }
}

#[tokio::test]
async fn cli06_empty_outbox_execution_failure_does_not_claim_completed() {
    let (jobs, items, report) =
        run_claimed_item_fixture(true, AccountingFault::None, 1, true).await;
    assert!(report.unwrap().failed > 0);
    let job = jobs
        .iter()
        .find(|job| job.triggered_by == "outbox")
        .unwrap();
    assert_eq!(
        (job.total_items, job.done_items, job.failed_items),
        (0, 0, 0)
    );
    assert_eq!(job.status, "failed");
    assert!(items.iter().all(|item| item.job_id != job.id));
}

#[tokio::test]
async fn cli06_outbox_projection_failure_propagates_without_partial_commit() {
    let (jobs, items, report) =
        run_claimed_item_fixture(false, AccountingFault::Finalization, 1, true).await;
    let error = report.unwrap_err();
    assert!(format!("{error:#}").contains("finalize outbox job projection"));
    assert!(format!("{error:#}").contains("fixture finalization failure"));
    assert_eq!(
        jobs.len(),
        1,
        "outbox finalization failure must stop before the scan"
    );
    assert_eq!(jobs[0].status, "running");
    assert_eq!(
        (
            jobs[0].total_items,
            jobs[0].done_items,
            jobs[0].failed_items
        ),
        (1, 1, 0)
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "done");
}

#[tokio::test]
async fn cli06_relation_item_insert_failure_is_reported_without_phantom_counters() {
    let (jobs, items, report) =
        run_claimed_item_fixture(false, AccountingFault::ItemInsert, 1, false).await;
    let report = report.unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.completed, 0);
    assert!(items.is_empty());
    assert_eq!(jobs[0].status, "failed");
    assert_eq!(
        (
            jobs[0].total_items,
            jobs[0].done_items,
            jobs[0].failed_items
        ),
        (0, 0, 0)
    );
}
