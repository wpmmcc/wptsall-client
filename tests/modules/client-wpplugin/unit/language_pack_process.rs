//! Private Linux subprocess contracts. Only owned SQLite and loopback mocks.
use super::*;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const WORKER: &str = "task_engine::discoverer::tests::language_pack_recovery::process::language_pack_recovery_process_worker";
const REQUEST: &str = "550e8400-e29b-41d4-a716-446655440057";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn(root: &Path, base: &str, mode: &str, expected: &str, label: &str) -> OwnedChild {
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
            .env("WPTSALL_OWNED_PACK_PROCESS", "owned-pack-process-v1")
            .env("WPTSALL_OWNED_PACK_BASE", base)
            .env("WPTSALL_OWNED_PACK_MODE", mode)
            .env("WPTSALL_OWNED_PACK_EXPECT", expected)
            .env(
                "WPTSALL_COMPONENT_BINDINGS_SECRET",
                std::env::var("WPTSALL_COMPONENT_BINDINGS_SECRET").unwrap(),
            )
            .env("WPTSALL_DATA_DIR", root)
            .env("WPTSALL_LOG_FILE", root.join("owned-runtime.log"))
            .env("WPTSALL_USE_SERVER_CONTROL_PLANE", "0")
            .env("WPTSALL_RETRY_MAX", "0")
            .env("WPTSALL_WP_TRANSPORT_ENCRYPT", "always")
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

async fn wait(child: &mut OwnedChild) {
    let status = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned pack child timed out; its guard will kill it");
    assert!(status.success(), "owned pack child failed");
}

fn runtime(base: &str, polling: bool) -> ComponentRuntime {
    let mut runtime = sample_registry().runtimes.remove("comp-selected").unwrap();
    runtime.template.request.url = format!("{base}/submit");
    if polling {
        runtime.template.async_poll = Some(
            serde_json::from_value(json!({
                "job_id_path":"job_id","request":{"method":"GET",
                    "url":format!("{base}/poll/{{{{computed.job_id}}}}"),"body_type":"none"},
                "status_path":"state","pending_values":["pending"],"done_values":["done"],
                "failed_values":["failed"],"interval_seconds":1,"timeout_seconds":15,
                "result_text_path":"text"
            }))
            .unwrap(),
        );
    }
    runtime
}

#[tokio::test]
#[ignore = "owned parent subprocess only; never uses live credentials"]
async fn language_pack_recovery_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_PACK_PROCESS").unwrap(),
        "owned-pack-process-v1"
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let base = std::env::var("WPTSALL_OWNED_PACK_BASE").unwrap();
    assert_eq!(
        reqwest::Url::parse(&base).unwrap().host_str(),
        Some("127.0.0.1")
    );
    let mode = std::env::var("WPTSALL_OWNED_PACK_MODE").unwrap();
    let expected = std::env::var("WPTSALL_OWNED_PACK_EXPECT").unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    if mode == "delivery" {
        let item = jobs::get_item_checked(&*db.lock().await, 1)
            .unwrap()
            .unwrap();
        let result = crate::task_engine::pipeline::sync_i18n_item_to_wp(
            &db,
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            1,
            &item.translated_path,
            &base,
            "owned-pack-token",
            &discovery_test_worker_config(),
            "/dev/null",
            &Arc::new(tokio::sync::Semaphore::new(1)),
        )
        .await;
        if expected == "busy" {
            assert!(format!("{:#}", result.unwrap_err()).contains("REVIEW_ITEM_BUSY"));
        } else {
            assert_eq!(result.unwrap(), if expected == "noop" { 0 } else { 1 });
        }
        return;
    }
    if mode == "manual" {
        let harness = crate::web_ui::test_support::WebUiTestHarness::new("", None)
            .await
            .unwrap();
        harness.state.lock().await.db = db;
        let response = harness
            .post_json(
                "/api/items/1/retranslate",
                json!({"request_id":REQUEST,"resume_only":true}),
            )
            .await
            .unwrap();
        match expected.as_str() {
            "busy" => assert_eq!(response.body["error"]["code"], "REVIEW_ITEM_BUSY"),
            "unknown" => assert_eq!(response.body["error"]["code"], "MANUAL_FIELDS_UNRESOLVED"),
            _ => assert!(response.status_line.contains("200"), "{}", response.body),
        }
        return;
    }
    let source = LanguagePackItem {
        object_id: 81,
        text_domain: "owned-pack".into(),
        complete_data: LanguagePackCompleteData {
            entry_id: 101,
            msgid: "Owned source".into(),
            msgctxt: "Owned context".into(),
            msgid_plural: "Owned sources".into(),
            plural_index: Some(1),
            text_domain: "owned-pack".into(),
        },
    };
    let result = translate_language_pack_entry(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &runtime(&base, mode == "poll"),
        Some(&db),
        "http://127.0.0.1/site-a/wp-json/wptsall/v2/owned/client",
        &sample_relation(),
        "plugin_i18n",
        "plugin",
        &source,
        &DiscoveryTaskParams::default(),
        &EffectiveConstraints::resolve(None, None, None, 10_000, "none"),
    )
    .await;
    match expected.as_str() {
        "busy" => {
            assert!(format!("{:#}", result.unwrap_err()).contains("LANGUAGE_PACK_ENTRY_BUSY"))
        }
        "unknown" => assert!(format!("{:#}", result.unwrap_err()).contains("submit")),
        _ => assert_eq!(result.unwrap(), "Owned provider translation"),
    }
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 4096];
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n > 0, "owned request truncated");
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                return (headers, bytes[end + 4..end + 4 + length].to_vec());
            }
        }
    }
}

async fn crash_case(mode: &'static str) {
    let f = PackFixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    if mode == "manual" {
        let saved = review_pack(&f).await;
        drop(pack_manual_plan(&f, &saved, runtime(&base, false), REQUEST).await);
    }
    let accepted = Arc::new(tokio::sync::Notify::new());
    let signal = accepted.clone();
    let submits = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let submit_count = submits.clone();
    let poll_count = polls.clone();
    let server = tokio::spawn(async move {
        let mut interrupted = false;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, request) = read_request(&mut socket).await;
            let target = headers.lines().next().unwrap();
            let poll = target.starts_with("GET ");
            if poll {
                assert!(target.contains("/poll/owned-original-job"));
                poll_count.fetch_add(1, Ordering::SeqCst);
            } else {
                assert!(target.starts_with("POST /submit "));
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&request).unwrap()["text"],
                    "Owned sources"
                );
                submit_count.fetch_add(1, Ordering::SeqCst);
            }
            if !interrupted && (mode != "poll" || poll) {
                interrupted = true;
                signal.notify_one();
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
                continue;
            }
            let body = if poll {
                r#"{"state":"done","text":"Owned provider translation"}"#
            } else {
                r#"{"job_id":"owned-original-job"}"#
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut first = spawn(&f.root, &base, mode, "success", "original");
    tokio::time::timeout(Duration::from_secs(15), accepted.notified())
        .await
        .expect("original pack process failed before the paid boundary");
    let mut competitor = spawn(&f.root, &base, mode, "busy", "competitor");
    wait(&mut competitor).await;
    first.0.kill().unwrap();
    let status = first.0.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "exercise real SIGKILL, not cancellation"
        );
    }
    let mut resumed = spawn(
        &f.root,
        &base,
        mode,
        if mode == "poll" { "success" } else { "unknown" },
        "resumed",
    );
    wait(&mut resumed).await;
    assert_eq!(
        submits.load(Ordering::SeqCst),
        1,
        "never repeat an original paid pack submit"
    );
    assert_eq!(
        polls.load(Ordering::SeqCst),
        if mode == "poll" { 2 } else { 0 }
    );
    if mode == "manual" {
        assert_eq!(
            crate::db::review_attempts::pending_request(&*f.db.lock().await, 1)
                .unwrap()
                .as_deref(),
            Some(REQUEST)
        );
    }
    server.abort();
    eprintln!(
        "owned pack {mode} SIGKILL: competitor refused, one submit, {} polls, retained root {}",
        polls.load(Ordering::SeqCst),
        f.root.display()
    );
}

#[tokio::test]
async fn language_pack_recovery_process_unknown_submit_retains_original_without_rebilling() {
    crash_case("unknown").await;
}

#[tokio::test]
async fn language_pack_recovery_process_poll_recovers_original_job_without_rebilling() {
    crash_case("poll").await;
}

#[tokio::test]
async fn language_pack_recovery_process_manual_unknown_retains_original_request_uuid() {
    crash_case("manual").await;
}

#[tokio::test]
async fn language_pack_recovery_process_lost_callback_resumes_original_and_fences_replacement() {
    let f = PackFixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/site-a/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let saved = f.persist_for(&base).await.unwrap();
    let original = std::fs::read(&saved.translated_path).unwrap();
    let accepted = Arc::new(tokio::sync::Notify::new());
    let signal = accepted.clone();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = requests.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let applied = effects.clone();
    let server = tokio::spawn(async move {
        let mut completed = false;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut socket).await;
            assert!(headers
                .lines()
                .next()
                .unwrap()
                .contains("/translation-callback "));
            let payload =
                crate::web_ui::test_support::decode_wp_transport_request("owned-pack-token", &body)
                    .unwrap();
            observed.lock().unwrap().push(payload);
            if !completed {
                completed = true;
                applied.fetch_add(1, Ordering::SeqCst);
                signal.notify_one();
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
                continue;
            }
            let body = r#"{"success":true,"result_id":81,"protocol":"v2","result_status":"synced","entries_updated":1,"entries_rejected":0}"#;
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-pack-token",
                body.as_bytes(),
            );
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let mut first = spawn(&f.root, &base, "delivery", "success", "original-delivery");
    tokio::time::timeout(Duration::from_secs(15), accepted.notified())
        .await
        .unwrap();
    let mut competitor = spawn(&f.root, &base, "delivery", "busy", "competing-delivery");
    wait(&mut competitor).await;
    first.0.kill().unwrap();
    let status = first.0.wait().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
    }
    let harness = crate::web_ui::test_support::WebUiTestHarness::new("", None)
        .await
        .unwrap();
    harness.state.lock().await.db = f.db.clone();
    let refusal = harness
        .post_json(
            "/api/items/1/retranslate",
            json!({"request_id":REQUEST,"resume_only":true}),
        )
        .await
        .unwrap();
    assert_eq!(refusal.body["error"]["code"], "REVIEW_DELIVERY_UNRESOLVED");
    let mut resumed = spawn(&f.root, &base, "delivery", "success", "resumed-delivery");
    wait(&mut resumed).await;
    let mut noop = spawn(&f.root, &base, "delivery", "noop", "completed-delivery");
    wait(&mut noop).await;
    server.abort();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0], requests[1],
        "restart may send only the original frozen callback"
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "owned remote keeps one idempotent effect"
    );
    assert_eq!(std::fs::read(&saved.translated_path).unwrap(), original);
    assert_eq!(
        jobs::get_item_checked(&*f.db.lock().await, saved.item_id)
            .unwrap()
            .unwrap()
            .status,
        "done"
    );
    eprintln!("owned pack callback SIGKILL: same callback body, one remote effect, replacement refused, saved receipt noop");
}
