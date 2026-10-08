//! Current W3 owned bad-read regressions; no live configuration or services.

use crate::db::discovery_tasks::{self, DiscoveryTaskParams};
use rusqlite::Connection;

fn policy(conn: &Connection) -> anyhow::Result<DiscoveryTaskParams> {
    discovery_tasks::get_discovery_task_params(conn, "owned", 7)
}

fn claims(conn: &Connection) -> anyhow::Result<std::collections::HashSet<String>> {
    discovery_tasks::selected_components_claimed_by_other_relations(conn, "owned", 7)
}

#[test]
fn current_missing_policy_table_is_not_an_enabled_default() {
    let conn = crate::db::open_db(":memory:").unwrap();
    conn.execute_batch("DROP TABLE discovery_tasks").unwrap();
    assert!(
        policy(&conn).is_err(),
        "storage failure is not a missing task"
    );
}

#[test]
fn current_bad_policy_scalar_is_not_a_default() {
    let conn = crate::db::open_db(":memory:").unwrap();
    discovery_tasks::ensure_discovery_tasks(&conn, "owned", &[7]).unwrap();
    conn.execute(
        "UPDATE discovery_tasks SET enabled='damaged' WHERE relation_id=7",
        [],
    )
    .unwrap();
    assert!(
        policy(&conn).is_err(),
        "bad enabled value must not enable execution"
    );
}

#[test]
fn current_bad_policy_json_is_not_an_empty_override() {
    let conn = crate::db::open_db(":memory:").unwrap();
    discovery_tasks::ensure_discovery_tasks(&conn, "owned", &[7]).unwrap();
    conn.execute(
        "UPDATE discovery_tasks SET editable_overrides_json='{broken' WHERE relation_id=7",
        [],
    )
    .unwrap();
    assert!(
        policy(&conn).is_err(),
        "bad JSON must not discard user policy"
    );
}

#[test]
fn current_missing_ownership_table_is_not_an_unclaimed_component_set() {
    let conn = crate::db::open_db(":memory:").unwrap();
    conn.execute_batch("DROP TABLE discovery_tasks").unwrap();
    assert!(
        claims(&conn).is_err(),
        "unknown ownership must not permit fallback"
    );
}

#[test]
fn current_valid_missing_task_and_disabled_task_are_distinct() {
    let conn = crate::db::open_db(":memory:").unwrap();
    assert!(policy(&conn).unwrap().enabled);
    discovery_tasks::ensure_discovery_tasks(&conn, "owned", &[7]).unwrap();
    conn.execute(
        "UPDATE discovery_tasks SET enabled=0 WHERE relation_id=7",
        [],
    )
    .unwrap();
    assert!(!policy(&conn).unwrap().enabled);
    assert!(claims(&conn).unwrap().is_empty());
}

#[test]
fn current_parallel_rules_share_an_atomic_item_budget() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    };
    let counter = AtomicUsize::new(0);
    let start = Barrier::new(17);
    let finish = Barrier::new(17);
    let mut largest = 0;
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let counter = &counter;
            let start = &start;
            let finish = &finish;
            scope.spawn(move || {
                for _ in 0..512 {
                    start.wait();
                    crate::task_engine::discoverer::try_reserve_discovery_item(counter, 1);
                    finish.wait();
                }
            });
        }
        for _ in 0..512 {
            counter.store(0, Ordering::Relaxed);
            start.wait();
            finish.wait();
            largest = largest.max(counter.load(Ordering::Relaxed));
        }
    });
    assert_eq!(
        largest, 1,
        "all rule workers must reserve against the same strict limit"
    );
}

async fn run_current_discovery_fixture(
    damage_policy: bool,
) -> (
    anyhow::Result<crate::types::DomainRunReport>,
    usize,
    tempfile::TempDir,
    std::sync::Arc<tokio::sync::Mutex<Connection>>,
) {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", temp.path().display().to_string());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let downstream = Arc::new(AtomicUsize::new(0));
    let seen = downstream.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                raw.extend_from_slice(&buffer[..n]);
                if raw.windows(4).any(|b| b == b"\r\n\r\n") {
                    break;
                }
                assert!(raw.len() < 64 * 1024);
            }
            let text = String::from_utf8(raw).unwrap();
            let route = text
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let body = if route.ends_with("/site-relations") {
                serde_json::json!({"relations":[{
                    "id":7,"source_site_id":1,"target_site_id":2,"source_lang":"en",
                    "target_lang":"zh","target_site_type":"site","sync_mode":"push","models":[]
                }]})
            } else {
                seen.fetch_add(1, Ordering::SeqCst);
                if route.contains("/rules?") {
                    serde_json::json!({"rules":[]})
                } else {
                    serde_json::json!({"success":true,"data":{"items":[],"schema_version":1},
                        "items":[],"total":0,"page":1,"per_page":20})
                }
            };
            let body = body.to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let conn = crate::db::open_db(":memory:").unwrap();
    if damage_policy {
        conn.execute_batch("DROP TABLE discovery_tasks").unwrap();
    } else {
        conn.execute_batch(
            "INSERT INTO translation_records (domain,status,created_at) VALUES ('owned','done',0);
             INSERT INTO retry_queue (domain,relation_id,object_id,status,created_at)
             VALUES ('owned',7,42,'done',0);",
        )
        .unwrap();
        for area in ["raw", "translated"] {
            let path = temp.path().join(area).join("owned/result.json");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"owned saved result").unwrap();
            std::fs::File::open(path)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH),
                )
                .unwrap();
        }
    }
    let db = Arc::new(Mutex::new(conn));
    let pending = Arc::new(Mutex::new(
        crate::persistence::PendingCallbackStore::open(":memory:").unwrap(),
    ));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        crate::task_engine::discoverer::discover_and_translate(
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            &format!("{base}/wp-json/wptsall/v2/owned/client"),
            "owned-token",
            &temp.path().join("run.log").display().to_string(),
            None,
            None,
            "",
            &[],
            None,
            None,
            &crate::worker::build_worker_config("owned-w3"),
            Some("owned"),
            &pending,
            None,
            None,
            None,
            Some(db.clone()),
            crate::types::PluginIdentity::WpmmccAts,
        ),
    )
    .await;
    server.abort();
    let _ = server.await;
    (
        result.expect("owned run must not hang"),
        downstream.load(Ordering::SeqCst),
        temp,
        db,
    )
}

#[tokio::test]
async fn current_discovery_storage_error_blocks_relation_requests() {
    let (result, requests, _, _) = run_current_discovery_fixture(true).await;
    assert!(result.is_err());
    assert_eq!(requests, 0, "bad local policy must block WP relation I/O");
}

#[tokio::test]
async fn current_default_scan_retains_saved_results_and_completed_records() {
    let (result, _, temp, db) = run_current_discovery_fixture(false).await;
    result.unwrap();
    let conn = db.lock().await;
    let history: i64 = conn
        .query_row("SELECT COUNT(*) FROM translation_records", [], |row| {
            row.get(0)
        })
        .unwrap();
    let retries: i64 = conn
        .query_row("SELECT COUNT(*) FROM retry_queue", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        (
            temp.path().join("raw/owned/result.json").exists(),
            temp.path().join("translated/owned/result.json").exists(),
            history,
            retries
        ),
        (true, true, 1, 1),
        "ordinary discovery is not permission to delete saved work"
    );
}

#[test]
fn current_default_boot_retains_aged_unconfirmed_async_jobs() {
    let conn = crate::db::open_db(":memory:").unwrap();
    conn.execute_batch(
        "INSERT INTO async_jobs (domain,relation_id,object_id,job_id,ctx_json,status,updated_at)
         VALUES ('owned',7,1,'owned-paid-1','owned-context-1','polling',0),
                ('owned',7,2,'owned-paid-2','owned-context-2','failed',0);",
    )
    .unwrap();
    let summary = crate::db::async_jobs::async_jobs_inventory(&conn).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM async_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        summary,
        (1, 1),
        "boot may count recoverable rows, not age-delete them"
    );
    assert_eq!(count, 2);
}
