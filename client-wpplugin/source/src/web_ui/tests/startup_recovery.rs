// catalog: WEBUI-MOD-web-ui-mod-rs
// catalog: WEBUI-MOD-worker-rs
// catalog: WEBUI-MOD-db-jobs-rs
// catalog: WEBUI-API-GET-api-status
// catalog: WEBUI-API-POST-api-worker-start
// catalog: WEBUI-API-POST-api-worker-stop
// oracle: L2
use super::*;
use rusqlite::{params, Connection};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DOMAIN: &str = "https://cli05.invalid";
const TEST_PREFIX: &str = "web_ui::tests::startup_recovery::";

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/modules/client-wpplugin/unit/physical_logging_startup.rs"
));

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/modules/client-wpplugin/unit/physical_sqlite_startup.rs"
));

struct OwnedProbe {
    child: Child,
    result: PathBuf,
    log: PathBuf,
}

impl OwnedProbe {
    fn wait_result(&mut self) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(bytes) = std::fs::read(&self.result) {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    return value;
                }
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "owned startup probe exited before readiness: {status}; {}",
                    std::fs::read_to_string(&self.log).unwrap_or_default()
                );
            }
            assert!(Instant::now() < deadline, "owned startup probe timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn crash(&mut self) {
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9), "exercise an actual SIGKILL");
        }
        assert!(!status.success());
    }
}

impl Drop for OwnedProbe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_probe(root: &Path, test: &str, mode: &str, label: &str) -> OwnedProbe {
    let result = root.join(format!("{label}-result.json"));
    let log = root.join(format!("{label}-process.log"));
    let output = std::fs::File::create(&log).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &format!("{TEST_PREFIX}{test}"), "--nocapture"])
        .current_dir(root)
        .env("WPTSALL_CLI05_RUNTIME_ROOT", root)
        .env("WPTSALL_CLI05_RUNTIME_MODE", mode)
        .env("WPTSALL_CLI05_RESULT_FILE", &result)
        .env("WPTSALL_DB_PATH", root.join("runtime/wptsall.db"))
        .env("WPTSALL_DATA_DIR", root)
        .env(
            "WPTSALL_LOG_FILE",
            root.join(format!("{label}-runtime.log")),
        )
        .env("WPTSALL_SERVER_BASE", "http://127.0.0.1:1")
        .env(
            "WPTSALL_USE_SERVER_CONTROL_PLANE",
            if mode == "server" { "1" } else { "0" },
        )
        .env(
            "WPTSALL_WEB_UI",
            if mode == "local" || mode == "server" {
                "0"
            } else {
                "1"
            },
        )
        .env("WPTSALL_WEB_UI_BIND", "127.0.0.1:0")
        .env("WPTSALL_WP_CLIENT_TOKEN", "")
        .env("WPTSALL_COMPONENT_RUNTIME", "0")
        .env("WPTSALL_COMPONENT_FALLBACK", "1")
        .env("WPTSALL_EVENT_WAIT_ENABLED", "0")
        .env("WPTSALL_ONESHOT", "1")
        .env("WPTSALL_POLL_SECONDS", "60")
        .env("WPTSALL_LOG_ENABLED", "0")
        .env_remove("WPTSALL_COMPONENT_BINDINGS_SECRET")
        .env_remove("WPTSALL_DEVICE_ID")
        .env_remove("WPTSALL_WP_DEVICE_ID")
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output));
    for env in [
        "WPTSALL_COMPONENT_BINDINGS_FILE",
        "WPTSALL_DOMAIN_TOKEN_BINDINGS_FILE",
        "WPTSALL_TASK_TYPE_COMPONENT_BINDINGS_FILE",
        "WPTSALL_RULE_COMPONENT_BINDINGS_FILE",
        "WPTSALL_VENDOR_KEYS_FILE",
        "WPTSALL_VENDOR_OAUTH_FILE",
        "WPTSALL_PROXY_PROFILES_FILE",
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        "WPTSALL_SIGNING_PUBLIC_KEY_FILE",
    ] {
        command.env(env, root.join(env));
    }
    OwnedProbe {
        child: command.spawn().unwrap(),
        result,
        log,
    }
}

fn write_result(path: &Path, value: serde_json::Value) {
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn child_probe() -> bool {
    let Ok(root) = std::env::var("WPTSALL_CLI05_RUNTIME_ROOT") else {
        return false;
    };
    let root = PathBuf::from(root);
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("cli05-runtime-"));
    let mode = std::env::var("WPTSALL_CLI05_RUNTIME_MODE").unwrap();
    let result_file = PathBuf::from(std::env::var("WPTSALL_CLI05_RESULT_FILE").unwrap());
    assert!(result_file.starts_with(&root));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        if mode == "local" || mode == "server" {
            let result = crate::worker::run_worker_cli(CancellationToken::new()).await;
            write_result(
                &result_file,
                json!({
                    "event": "returned", "ok": result.is_ok(),
                    "error": result.err().map(|error| format!("{error:#}"))
                }),
            );
            return;
        }
        assert!(mode == "seed" || mode == "webui");
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reservation.local_addr().unwrap();
        drop(reservation);
        std::env::set_var("WPTSALL_WEB_UI_BIND", addr.to_string());
        let shutdown = CancellationToken::new();
        let mut task = tokio::spawn(run_web_ui(shutdown.clone(), Instant::now()));
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if task.is_finished() {
                let result = (&mut task).await.unwrap();
                write_result(
                    &result_file,
                    json!({
                        "event": "returned", "ok": result.is_ok(),
                        "error": result.err().map(|error| format!("{error:#}"))
                    }),
                );
                return;
            }
            if let Ok(response) = client.get(format!("http://{addr}/api/status")).send().await {
                assert!(response.status().is_success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned WebUI did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if mode == "seed" {
            seed_interrupted_state(&root);
        }
        write_result(
            &result_file,
            json!({ "event": "started", "addr": addr.to_string() }),
        );
        let stop_marker = result_file.with_extension("shutdown");
        tokio::select! {
            result = &mut task => { result.unwrap().unwrap(); }
            _ = async {
                while !stop_marker.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {
                shutdown.cancel();
                task.await.unwrap().unwrap();
                let db_path = root.join("runtime/wptsall.db");
                let _reusable = crate::db::runtime::RuntimeLease::acquire(db_path.to_str().unwrap())
                    .expect("shutdown must finish owned connections before reporting stopped");
                write_result(&result_file.with_extension("stopped"), json!({"event":"stopped"}));
            }
        }
    });
    true
}

fn fixture_connection(root: &Path) -> Connection {
    crate::db::open_db(root.join("runtime/wptsall.db").to_str().unwrap()).unwrap()
}

fn seed_interrupted_state(root: &Path) {
    let conn = fixture_connection(root);
    let job_id = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: DOMAIN.to_string(),
            relation_id: 7,
            business_line: "discovery".to_string(),
            triggered_by: "auto".to_string(),
        },
    )
    .unwrap();
    crate::db::jobs::update_job_status(&conn, job_id, "running").unwrap();
    for (index, phase) in [
        "fetching",
        "fetched",
        "translating",
        "syncing",
        "translated",
        "pending_review",
        "done",
        "skipped",
        "failed",
        "pending",
        "unknown",
        "translating-paid",
    ]
    .into_iter()
    .enumerate()
    {
        let raw = root.join(format!("raw-{index}.json"));
        std::fs::write(&raw, format!("fixture raw {index}")).unwrap();
        let translated = if [
            "syncing",
            "translated",
            "pending_review",
            "translating-paid",
        ]
        .contains(&phase)
        {
            let path = root.join(format!("translated-{index}.json"));
            std::fs::write(&path, format!("fixture paid-result {index}")).unwrap();
            path.display().to_string()
        } else {
            String::new()
        };
        let id = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: DOMAIN.to_string(),
                relation_id: 7,
                business_line: "post_content".to_string(),
                object_type: "post_type".to_string(),
                wp_object_id: index as i64 + 1,
                wp_object_subtype: "post".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "fixture-component".to_string(),
                component_ids: vec!["fixture-component".to_string()],
                selected_component_id: Some("fixture-component".to_string()),
                effective_source_lang: Some("en".to_string()),
                effective_target_lang: Some("zh".to_string()),
                editable_overrides: Some(json!({"fixture": true})),
                raw_path: raw.display().to_string(),
                client_task_id: format!("fixture-task-{index}"),
                max_retries: 3,
            },
        )
        .unwrap();
        let status = if phase == "translating-paid" {
            "translating"
        } else {
            phase
        };
        conn.execute(
            "UPDATE translation_items SET status=?1, translated_path=?2, content_hash='fixture-hash',
             error_message='fixture retry error', retry_count=1, fetched_at=22, translated_at=33 WHERE id=?3",
            params![status, translated, id],
        ).unwrap();
        assert!(crate::db::discovery_tasks::try_claim_in_progress(
            &conn,
            DOMAIN,
            7,
            "post_type",
            index as i64 + 1
        )
        .unwrap());
    }
    conn.execute("UPDATE translation_jobs SET total_items=24, done_items=4, failed_items=2, started_at=100 WHERE id=?1", params![job_id]).unwrap();
    let empty = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: DOMAIN.to_string(),
            relation_id: 8,
            business_line: "discovery".to_string(),
            triggered_by: "auto".to_string(),
        },
    )
    .unwrap();
    crate::db::jobs::update_job_status(&conn, empty, "running").unwrap();
    let now = unix_ts() as i64;
    conn.execute(
        "INSERT INTO async_jobs (domain,relation_id,object_type,object_id,field_name,chunk_index,lane,
         component_id,job_id,ctx_json,source_lang,target_lang,status,attempts,error,created_at,updated_at)
         VALUES (?1,7,'post_type',3,'post_title',0,'text','fixture-component','fixture-provider-job',
         '{\"fixture-context\":\"submitted\"}','en','zh','polling',2,'',?2,?2)",
        params![DOMAIN, now],
    ).unwrap();
}

fn snapshot(conn: &Connection) -> serde_json::Value {
    let mut jobs = crate::db::jobs::list_jobs(conn, None, 100, 0).unwrap();
    jobs.sort_by_key(|job| std::cmp::Reverse(job.id));
    let items: Vec<_> = jobs
        .iter()
        .flat_map(|job| crate::db::jobs::list_items_by_job(conn, job.id, None).unwrap())
        .collect();
    let claims: i64 = conn
        .query_row("SELECT COUNT(*) FROM translation_in_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    let async_row: String = conn.query_row("SELECT job_id || ':' || ctx_json || ':' || status || ':' || attempts || ':' || updated_at FROM async_jobs", [], |row| row.get(0)).unwrap();
    json!({"jobs": jobs, "items": items, "claims": claims, "async": async_row})
}

fn interrupted_root(test: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("cli05-runtime-")
        .tempdir()
        .unwrap();
    let mut initial = spawn_probe(root.path(), test, "seed", "initial");
    assert_eq!(initial.wait_result()["event"], "started");
    initial.crash();
    root
}

fn assert_recovered(root: &Path, before: &serde_json::Value) {
    let conn = fixture_connection(root);
    let after = snapshot(&conn);
    assert_eq!(
        after["jobs"][1]["status"], "partial",
        "actual boot must close the orphan running job by item truth"
    );
    assert_eq!(
        (
            after["jobs"][1]["total_items"].as_i64(),
            after["jobs"][1]["done_items"].as_i64(),
            after["jobs"][1]["failed_items"].as_i64()
        ),
        (Some(12), Some(2), Some(1))
    );
    assert_eq!(
        after["jobs"][0]["status"], "failed",
        "an interrupted empty job is not successful"
    );
    assert_eq!(
        after["claims"], 0,
        "fresh claims from the killed runtime must be reclaimable"
    );
    assert_eq!(
        after["async"], before["async"],
        "keep the submitted provider snapshot"
    );
    for (old, new) in before["items"]
        .as_array()
        .unwrap()
        .iter()
        .zip(after["items"].as_array().unwrap())
    {
        let interrupted = ["fetching", "fetched", "translating", "syncing"]
            .contains(&old["status"].as_str().unwrap());
        if interrupted {
            let expected = if old["translated_path"].as_str().unwrap().is_empty() {
                "pending"
            } else {
                "translated"
            };
            assert_eq!(new["status"], expected);
            for key in old.as_object().unwrap().keys() {
                if key != "status" && key != "updated_at" {
                    assert_eq!(new[key], old[key], "{key} survives recovery");
                }
            }
        } else {
            assert_eq!(
                new, old,
                "paid/review/terminal/pending items stay byte-equivalent"
            );
        }
        for key in ["raw_path", "translated_path"] {
            let path = old[key].as_str().unwrap();
            if !path.is_empty() {
                assert!(
                    Path::new(path).exists(),
                    "recovery must not remove artifacts"
                );
                let index = old["wp_object_id"].as_i64().unwrap() - 1;
                let expected = if key == "raw_path" {
                    format!("fixture raw {index}")
                } else {
                    format!("fixture paid-result {index}")
                };
                assert_eq!(std::fs::read(path).unwrap(), expected.as_bytes());
            }
        }
    }
    assert!(crate::db::with_recovery_credit(&conn, || {
        crate::db::discovery_tasks::try_claim_in_progress(&conn, DOMAIN, 7, "post_type", 1)
    })
    .unwrap());
}

fn killed_runtime_recovery(test: &str, mode: &str) {
    if child_probe() {
        return;
    }
    let root = interrupted_root(test);
    let before = snapshot(&fixture_connection(root.path()));
    let mut restarted = spawn_probe(root.path(), test, mode, "restart");
    let result = restarted.wait_result();
    if mode == "webui" {
        assert_eq!(result["event"], "started");
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let response: serde_json::Value = client
            .get(format!(
                "http://{}/api/jobs",
                result["addr"].as_str().unwrap()
            ))
            .send()
            .unwrap()
            .json()
            .unwrap();
        let job = response["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|job| job["relation_id"] == 7)
            .unwrap();
        assert_eq!(job["status"], "partial", "HTTP reads the recovered job");
    } else {
        assert_eq!(result["ok"], true);
    }
    assert_recovered(root.path(), &before);
}

#[test]
fn cli05_killed_webui_recovers_rows_without_losing_paid_or_review_data() {
    killed_runtime_recovery(
        "cli05_killed_webui_recovers_rows_without_losing_paid_or_review_data",
        "webui",
    );
}

#[test]
fn cli05_local_worker_boot_recovers_the_same_persistent_database() {
    killed_runtime_recovery(
        "cli05_local_worker_boot_recovers_the_same_persistent_database",
        "local",
    );
}

#[test]
fn cli05_legacy_worker_boot_recovers_before_any_login_or_network() {
    killed_runtime_recovery(
        "cli05_legacy_worker_boot_recovers_before_any_login_or_network",
        "server",
    );
}

#[test]
fn cli05_second_runtime_refuses_before_reclaiming_an_active_database() {
    if child_probe() {
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("cli05-runtime-")
        .tempdir()
        .unwrap();
    let test = "cli05_second_runtime_refuses_before_reclaiming_an_active_database";
    let mut initial = spawn_probe(root.path(), test, "seed", "initial");
    assert_eq!(initial.wait_result()["event"], "started");
    let before = snapshot(&fixture_connection(root.path()));
    let mut errors = Vec::new();
    for mode in ["webui", "local", "server"] {
        let mut second = spawn_probe(root.path(), test, mode, &format!("second-{mode}"));
        let result = second.wait_result();
        assert_eq!(
            snapshot(&fixture_connection(root.path())),
            before,
            "a second {mode} must not touch active work"
        );
        if result["error"]
            .as_str()
            .is_none_or(|error| !error.contains("CLIENT_RUNTIME_ALREADY_RUNNING"))
        {
            errors.push(mode);
        }
    }
    assert!(
        errors.is_empty(),
        "second runtimes must refuse the owned DB before boot recovery: {errors:?}"
    );
}

#[test]
fn cli05_recovery_storage_failure_aborts_every_boot_and_releases_its_runtime_lease() {
    if child_probe() {
        return;
    }
    let test = "cli05_recovery_storage_failure_aborts_every_boot_and_releases_its_runtime_lease";
    let root = interrupted_root(test);
    let conn = fixture_connection(root.path());
    let before = snapshot(&conn);
    conn.execute_batch(
        "CREATE TRIGGER cli05_fail_recovery BEFORE DELETE ON translation_in_progress
         BEGIN SELECT RAISE(ABORT, 'fixture recovery failure'); END;",
    )
    .unwrap();
    for mode in ["webui", "local", "server"] {
        let mut failed = spawn_probe(root.path(), test, mode, &format!("failed-{mode}"));
        let result = failed.wait_result();
        assert_eq!(result["event"], "returned");
        assert_eq!(
            result["ok"], false,
            "boot cannot serve/run with incomplete recovery"
        );
        assert!(result["error"]
            .as_str()
            .unwrap()
            .contains("fixture recovery failure"));
        assert_eq!(
            snapshot(&conn),
            before,
            "all recovered rows roll back before boot fails"
        );
    }
    conn.execute_batch("DROP TRIGGER cli05_fail_recovery")
        .unwrap();
    let mut healthy = spawn_probe(root.path(), test, "webui", "healthy");
    assert_eq!(
        healthy.wait_result()["event"],
        "started",
        "a failed startup cannot leave its runtime lease behind"
    );
    assert_recovered(root.path(), &before);
}

#[test]
fn cli05_actual_idle_loop_stop_wakes_promptly_and_shutdown_releases_lease() {
    if child_probe() {
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("cli05-runtime-")
        .tempdir()
        .unwrap();
    let test = "cli05_actual_idle_loop_stop_wakes_promptly_and_shutdown_releases_lease";
    let mut probe = spawn_probe(root.path(), test, "webui", "first");
    let started = probe.wait_result();
    assert_eq!(started["event"], "started");
    let base = format!("http://{}", started["addr"].as_str().unwrap());
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for _ in 0..2 {
        let response: serde_json::Value = client
            .post(format!("{base}/api/worker/start"))
            .json(&json!({"force":true}))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(response["success"], true, "{response}");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status: serde_json::Value = client
                .get(format!("{base}/api/status"))
                .send()
                .unwrap()
                .json()
                .unwrap();
            assert_eq!(status["data"]["worker_loop_running"], true);
            if status["data"]["worker_status"] == "waiting" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real loop must reach its idle sleep"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let stop_at = Instant::now();
        let response: serde_json::Value = client
            .post(format!("{base}/api/worker/stop"))
            .json(&json!({}))
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(response["success"], true);
        assert!(
            stop_at.elapsed() < Duration::from_millis(1500),
            "idle stop must wake rather than burn the two-second grace"
        );
    }
    // The shutdown path must also stop a real sleeping loop, not merely change UI flags.
    let response: serde_json::Value = client
        .post(format!("{base}/api/worker/start"))
        .json(&json!({"force":true}))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(response["success"], true);
    // Keep a real accepted HTTP connection blocked mid-header through shutdown.
    // A detached handler would retain the DB lease after run_web_ui returned.
    let mut held_connection =
        std::net::TcpStream::connect(started["addr"].as_str().unwrap()).unwrap();
    use std::io::Write;
    held_connection
        .write_all(b"GET /api/jobs HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    assert!(client
        .get(format!("{base}/api/status"))
        .send()
        .unwrap()
        .status()
        .is_success());
    std::fs::write(probe.result.with_extension("shutdown"), b"stop owned probe").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = probe.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "shutdown must await and release its worker"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(probe.result.with_extension("stopped").exists());
    let mut restarted = spawn_probe(root.path(), test, "webui", "second");
    assert_eq!(
        restarted.wait_result()["event"],
        "started",
        "same DB must be reusable after a graceful shutdown"
    );
}
