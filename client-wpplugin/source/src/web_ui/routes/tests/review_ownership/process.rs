//! Owned shared-route subprocess proof, intentionally without the runtime-wide lease.
use super::*;
#[path = "../../../../../../../tests/modules/client-wpplugin/unit/encrypted_json_manual_process.rs"]
mod encrypted_json_manual_process;
use std::path::Path;
use std::process::{Child, Command, Stdio};

const WORKER: &str =
    "web_ui::routes::tests::review_ownership::process::owned_manual_process_worker";
const REQUEST: &str = "c1b05b00-4af3-4a5f-9d88-13b6dc674f11";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn(root: &Path, label: &str) -> OwnedChild {
    let executable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let log = std::fs::File::create(root.join(format!("{label}.log"))).unwrap();
    OwnedChild(
        Command::new(executable)
            .args([
                "--exact",
                WORKER,
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .current_dir(root)
            .env("WPTSALL_OWNED_MANUAL_PROCESS", "owned-manual-process-v1")
            .env("WPTSALL_OWNED_MANUAL_LABEL", label)
            .env(
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                std::env::var("WPTSALL_COMPONENT_BINDINGS_SECRET").unwrap(),
            )
            .env("WPTSALL_DATA_DIR", root)
            .env("WPTSALL_LOG_FILE", root.join("owned-runtime.log"))
            .env("WPTSALL_USE_SERVER_CONTROL_PLANE", "0")
            .env("WPTSALL_RETRY_MAX", "0")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

async fn wait(child: &mut OwnedChild) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "owned manual child failed");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "owned subprocess harness only; no saved credentials or live provider"]
async fn owned_manual_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_MANUAL_PROCESS").unwrap(),
        "owned-manual-process-v1"
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let label = std::env::var("WPTSALL_OWNED_MANUAL_LABEL").unwrap();
    assert!(label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
    let state = build_test_web_ui_state("", None);
    state.lock().await.db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let response = action(&state, 1, "retranslate", b"").await;
    crate::bindings::atomic_file::install_new(
        &root.join(format!("{label}.response")),
        response.as_bytes(),
    )
    .unwrap();
}

async fn seed(root: &Path, provider: &str) -> Arc<Mutex<rusqlite::Connection>> {
    let state = build_test_web_ui_state("", None);
    let db = Arc::new(Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    state.lock().await.db = db.clone();
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:9",
        "",
        "pending_review",
        "owned",
    )
    .await;
    assert_eq!(id, 1);
    let plan = json!({
        "format":"manual-plan-v1","wp_base":"http://127.0.0.1:9/wp-json/wptsall/v2/owned/client",
        "content":{"post_title":"owned frozen source","__wptsall_job_snapshot":{"source_revision":"owned-revision","policy_version":"owned-policy"}},
        "relation":{"id":1,"source_lang":"en_US","target_lang":"zh_CN","target_site_type":"virtual","sync_mode":"one_way"},
        "rules":[{"id":1,"model_id":1,"data_type":"post","object_name":"post","translate_fields":["post_title"],
            "field_content_formats":{"post_title":"plain_text"},"field_storage_map":{"post_title":"post_column"}}],
        "runtimes":{"comp-test":{
            "template":{"id":"comp-test","name":"Owned","version":"1","type":"text",
                "request":{"method":"POST","url":provider,"body_type":"json","body":{"text":"{{input.text}}"}},
                "response":{"translated_text_path":"text"}},
            "auth_values":{},"language_map":{},"supported_content_formats":["plain_text"],
            "supported_formats":[],"supported_business_lines":[],"key_pool":null,"oauth_pool":null,"oauth_configs":null,
            "proxy_profile_id":null,"max_concurrent":1,"min_interval_ms":0
        }},
        "ordered_ids":["comp-test"],"rule_bindings":{"version":1,"bindings":{}},
        "task_type_bindings":{"version":1,"bindings":{}},"proxy_profiles":{}
    });
    let _: crate::db::review_attempts::plan::ManualPlan =
        serde_json::from_value(plan.clone()).unwrap();
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, 1)
        .unwrap()
        .unwrap();
    let lease = crate::db::unit_lock::UnitLease::item(&db, 1).await.unwrap();
    crate::db::review_attempts::scope(&*db.lock().await, &db, &lease, &item, &plan, REQUEST)
        .unwrap();
    drop(lease);
    db
}

#[tokio::test]
async fn remaining_manual_real_kill_after_paid_submit_preserves_unknown_and_refuses_competitor() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let submits = Arc::new(AtomicUsize::new(0));
    let count = submits.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            let n = socket.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..n]).starts_with("POST "));
            count.fetch_add(1, Ordering::SeqCst);
            signal.notify_one();
            // Provider has accepted the paid request, but its result is unknown.
            std::future::pending::<()>().await;
        }
    });
    let db = seed(root.path(), &base).await;
    let mut original = spawn(root.path(), "original");
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    let mut competitor = spawn(root.path(), "competitor");
    wait(&mut competitor).await;
    assert!(
        std::fs::read_to_string(root.path().join("competitor.response"))
            .unwrap()
            .contains("REVIEW_ITEM_BUSY")
    );
    original.0.kill().unwrap();
    let status = original.0.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9), "exercise a real SIGKILL");
    }
    let mut resumed = spawn(root.path(), "resumed");
    wait(&mut resumed).await;
    let response = std::fs::read_to_string(root.path().join("resumed.response")).unwrap();
    assert!(response.contains("RETRANSLATE_FAILED"), "{response}");
    assert_eq!(
        submits.load(Ordering::SeqCst),
        1,
        "unknown paid submission must never automatically submit again"
    );
    assert_eq!(
        crate::db::review_attempts::pending_request(&*db.lock().await, 1)
            .unwrap()
            .as_deref(),
        Some(REQUEST)
    );
    server.abort();
    eprintln!("owned manual SIGKILL: original UUID retained, competitor refused, fresh process made zero new submits");
}

#[tokio::test]
async fn remaining_manual_complete_orphan_recovers_in_a_fresh_process_without_source_or_provider() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let db = seed(root.path(), "http://127.0.0.1:9").await;
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, 1)
        .unwrap()
        .unwrap();
    let plan = crate::db::review_attempts::saved_plan(&*db.lock().await, 1, REQUEST)
        .unwrap()
        .unwrap();
    let lease = crate::db::unit_lock::UnitLease::item(&db, 1).await.unwrap();
    let scope =
        crate::db::review_attempts::scope(&*db.lock().await, &db, &lease, &item, &plan, REQUEST)
            .unwrap();
    let payload: TranslationCallbackPayload = serde_json::from_value(json!({
        "relation_id":1,"business_line":"post_content","object_type":"post_type","post_type":"post","object_id":item.wp_object_id,
        "translated_fields":{"post_title":"owned complete result"},"translated_meta":{},"media_mappings":[],
        "client_task_id":format!("manual-{REQUEST}"),"worker_id":"owned","source_lang":"en_US","target_lang":"zh_CN",
        "execution_time_ms":1,"source_revision":"owned-revision"
    })).unwrap();
    db.lock().await.execute_batch("CREATE TRIGGER deny_manual_result BEFORE UPDATE OF translated_path ON translation_items
        BEGIN SELECT RAISE(ABORT,'owned complete result projection refusal'); END;").unwrap();
    let path = root.path().join("owned-complete-result.json");
    assert!(crate::task_engine::pipeline::persist_translated_claimed(
        &lease,
        &db,
        1,
        &payload,
        &format!("manual-{REQUEST}"),
        None,
        path.to_str().unwrap(),
        "/dev/null",
        Some(&scope)
    )
    .await
    .is_err());
    assert!(path.is_file());
    db.lock()
        .await
        .execute_batch("DROP TRIGGER deny_manual_result;")
        .unwrap();
    drop(lease);
    let mut recovered = spawn(root.path(), "orphan-recovered");
    wait(&mut recovered).await;
    let response = std::fs::read_to_string(root.path().join("orphan-recovered.response")).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(parse_http_json_body(&response)["data"]["replayed"], true);
    assert_eq!(
        parse_http_json_body(&response)["data"]["status"],
        "pending_review"
    );
    assert_eq!(
        crate::db::jobs::get_item_checked(&*db.lock().await, 1)
            .unwrap()
            .unwrap()
            .translated_path,
        path.to_str().unwrap()
    );
    eprintln!("owned manual fresh process: complete orphan restored original UUID and result without source/provider");
}
