// catalog: WEBUI-MOD-component-rt-runner-rs
// oracle: L2
use super::*;

struct PollBarrier {
    base: String,
    submits: Arc<AtomicUsize>,
    polls: Arc<AtomicUsize>,
    downloads: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for PollBarrier {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn poll_barrier() -> PollBarrier {
    poll_barrier_at(false).await
}

async fn poll_barrier_at(download_stage: bool) -> PollBarrier {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let submits = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let downloads = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let state = (
        submits.clone(),
        polls.clone(),
        downloads.clone(),
        entered.clone(),
        release.clone(),
    );
    let server = tokio::spawn(async move {
        // Aborting the server also aborts its owned connection handlers.
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (submits, polls, downloads, entered, release) = state.clone();
            connections.spawn(async move {
                let mut bytes = [0u8; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                let path = request.lines().next().unwrap().split_whitespace().nth(1).unwrap();
                let body = if path == "/download/job789" {
                    downloads.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.notified().await;
                    json!("owned binary fixture").to_string()
                } else if path == "/submit" {
                    submits.fetch_add(1, Ordering::SeqCst);
                    json!({"job_id": "job789"}).to_string()
                } else {
                    assert_eq!(path, "/poll/job789");
                    let count = polls.fetch_add(1, Ordering::SeqCst);
                    if count == 0 && !download_stage {
                        entered.notify_one();
                        release.notified().await;
                    }
                    // Different outcomes expose a competing terminal writer,
                    // rather than letting identical mock output hide the race.
                    let text = if count == 0 { "owned winner" } else { "unowned contender" };
                    json!({"status": "done", "translated_text": text,
                        "result": {"video_url": format!("https://owned.invalid/{count}.mp4")}}).to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    PollBarrier {
        base,
        submits,
        polls,
        downloads,
        entered,
        release,
        server,
    }
}

fn claims_runtime(base: &str, media: bool) -> ComponentRuntime {
    let mut runtime = gap04_text_runtime(base, "poll", 10);
    runtime.template.id = "owned-poller-claims".into();
    let poll = runtime.template.async_poll.as_mut().unwrap();
    poll.status_path = Some("status".into());
    poll.done_values = vec!["done".into()];
    poll.result_text_path = Some("translated_text".into());
    if media {
        runtime.template.kind = "video_translation".into();
        runtime.template.response.translated_text_path = None;
        poll.result_ref_path = Some("result.video_url".into());
        poll.result_text_path = None;
    }
    runtime
}

fn claims_input(media: bool) -> Value {
    if media {
        json!({"source_text": "", "source_payload": null,
            "source_ref": "data:video/mp4;base64,QUJDREVGRw==",
            "task_type": "video", "key": "owned-poller"})
    } else {
        json!("Hello")
    }
}

async fn run_claims_unit(
    client: &Client,
    runtime: &ComponentRuntime,
    env: crate::db::async_jobs::AsyncJobEnv,
    media: bool,
) -> anyhow::Result<Value> {
    if media {
        let result = translate_non_text_via_component_with_env(
            client,
            runtime,
            "",
            None,
            "data:video/mp4;base64,QUJDREVGRw==",
            "video",
            "owned-poller",
            "en",
            "zh",
            Some(env),
        )
        .await?;
        Ok(
            json!({"translated_ref": result.translated_ref, "translated_text": result.translated_text}),
        )
    } else {
        translate_text_via_component_with_env(client, runtime, "Hello", "en", "zh", Some(env))
            .await
            .map(Value::String)
    }
}

async fn seed_known_claims_job(
    runtime: &ComponentRuntime,
    media: bool,
    connection: rusqlite::Connection,
) -> crate::db::async_jobs::AsyncJobEnv {
    let mut env = gap04_test_env(if media { "non_text" } else { "text" }, "owned-poller")
        .for_runtime(runtime, &claims_input(media), "en", "zh")
        .unwrap();
    env.db = Arc::new(tokio::sync::Mutex::new(connection));
    let ctx = gap04_crash_ctx_snapshot();
    crate::db::async_jobs::test_writes::upsert_polling_job(
        &env,
        &runtime.template.id,
        "job789",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    crate::db::async_jobs::test_writes::save_provider_job(
        &env,
        &runtime.template.id,
        "job789",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    env
}

async fn concurrent_known_job(media: bool, separate_connections: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let mock = poll_barrier().await;
    let runtime = Arc::new(claims_runtime(&mock.base, media));
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.sqlite");
    let _runtime_lease = crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let mut env = gap04_test_env(if media { "non_text" } else { "text" }, "owned-poller")
        .for_runtime(&runtime, &claims_input(media), "en", "zh")
        .unwrap();
    env.db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(path.to_str().unwrap()).unwrap(),
    ));
    let ctx = gap04_crash_ctx_snapshot();
    crate::db::async_jobs::test_writes::upsert_polling_job(
        &env,
        &runtime.template.id,
        "job789",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    crate::db::async_jobs::test_writes::save_provider_job(
        &env,
        &runtime.template.id,
        "job789",
        &ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    let mut contender_env = env.clone();
    if separate_connections {
        contender_env.db = Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(path.to_str().unwrap()).unwrap(),
        ));
    }
    let client = Client::builder().no_proxy().build().unwrap();
    let winner = {
        let (client, runtime, env) = (client.clone(), runtime.clone(), env.clone());
        tokio::spawn(async move { run_claims_unit(&client, &runtime, env, media).await })
    };
    tokio::time::timeout(Duration::from_secs(5), mock.entered.notified())
        .await
        .expect("the winner reached the real loopback poll");
    let contender = tokio::time::timeout(
        Duration::from_secs(5),
        run_claims_unit(&client, &runtime, contender_env, media),
    )
    .await
    .expect("a contender must refuse promptly, not wait for the owner");
    mock.release.notify_one();
    let winner = tokio::time::timeout(Duration::from_secs(5), winner)
        .await
        .unwrap()
        .unwrap();
    let counts = (
        mock.submits.load(Ordering::SeqCst),
        mock.polls.load(Ordering::SeqCst),
    );
    eprintln!("owned poller counts={counts:?}; contender={contender:?}; winner={winner:?}");
    assert!(
        contender.is_err(),
        "a live unit owner must exclude a second poller"
    );
    assert_eq!(
        counts,
        (0, 1),
        "only the existing paid job's owner may poll"
    );
    let expected = if media {
        json!({"translated_ref": "https://owned.invalid/0.mp4", "translated_text": ""})
    } else {
        json!("owned winner")
    };
    assert_eq!(winner.unwrap(), expected);
    assert!(crate::db::async_jobs::find_polling_job(&env)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        run_claims_unit(&client, &runtime, env, media)
            .await
            .unwrap(),
        expected,
        "the winner's result must replay without another provider request"
    );
    assert_eq!(mock.polls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn poller_claim_text_same_connection_excludes_competing_resume() {
    concurrent_known_job(false, false).await;
}

#[tokio::test]
async fn poller_claim_media_same_connection_excludes_competing_resume() {
    concurrent_known_job(true, false).await;
}

#[tokio::test]
async fn poller_claim_text_separate_connections_excludes_competing_resume() {
    concurrent_known_job(false, true).await;
}

#[tokio::test]
async fn poller_claim_media_separate_connections_excludes_competing_resume() {
    concurrent_known_job(true, true).await;
}

async fn cancel_known_poller(media: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let mock = poll_barrier().await;
    let runtime = Arc::new(claims_runtime(&mock.base, media));
    let env = seed_known_claims_job(&runtime, media, crate::db::open_db(":memory:").unwrap()).await;
    let client = Client::builder().no_proxy().build().unwrap();
    let first = {
        let (client, runtime, env) = (client.clone(), runtime.clone(), env.clone());
        tokio::spawn(async move { run_claims_unit(&client, &runtime, env, media).await })
    };
    tokio::time::timeout(Duration::from_secs(5), mock.entered.notified())
        .await
        .unwrap();
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let row = crate::db::async_jobs::find_polling_job(&env)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((row.job_id.as_str(), row.attempts), ("job789", 1));
    let result = run_claims_unit(&client, &runtime, env.clone(), media)
        .await
        .unwrap();
    mock.release.notify_one();
    // The aborted request may still have reached the provider. Its late reply
    // has no local future/authority left to overwrite the replacement result.
    tokio::task::yield_now().await;
    assert_eq!(
        run_claims_unit(&client, &runtime, env, media)
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        (
            mock.submits.load(Ordering::SeqCst),
            mock.polls.load(Ordering::SeqCst)
        ),
        (0, 2)
    );
}

#[tokio::test]
async fn poller_claim_text_cancellation_releases_only_local_owner_and_resumes_original_job() {
    cancel_known_poller(false).await;
}

#[tokio::test]
async fn poller_claim_media_cancellation_releases_only_local_owner_and_resumes_original_job() {
    cancel_known_poller(true).await;
}

async fn claim_storage_fault(media: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    for known in [false, true] {
        for action in ["ABORT,'owned claim write fault'", "IGNORE"] {
            let mock = poll_barrier().await;
            let runtime = claims_runtime(&mock.base, media);
            let env = if known {
                seed_known_claims_job(&runtime, media, crate::db::open_db(":memory:").unwrap())
                    .await
            } else {
                gap04_test_env(if media { "non_text" } else { "text" }, "owned-poller")
            };
            let sql = format!(
                "CREATE TRIGGER deny_claim BEFORE {} ON system_config
                 WHEN NEW.key LIKE 'provider-execution-claim-v1:%'
                 BEGIN SELECT RAISE({action}); END;",
                if known { "UPDATE" } else { "INSERT" }
            );
            env.db.lock().await.execute_batch(&sql).unwrap();
            let result = run_claims_unit(
                &Client::builder().no_proxy().build().unwrap(),
                &runtime,
                env.clone(),
                media,
            )
            .await;
            assert!(result.is_err());
            assert_eq!(
                (
                    mock.submits.load(Ordering::SeqCst),
                    mock.polls.load(Ordering::SeqCst)
                ),
                (0, 0)
            );
            assert_eq!(
                crate::db::async_jobs::find_polling_job(&env)
                    .await
                    .unwrap()
                    .is_some(),
                known
            );
        }
    }
}

#[tokio::test]
async fn poller_claim_text_storage_failure_means_zero_provider_egress() {
    claim_storage_fault(false).await;
}

#[tokio::test]
async fn poller_claim_media_storage_failure_means_zero_provider_egress() {
    claim_storage_fault(true).await;
}

async fn ready_replay_without_claim_write(media: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let mock = poll_barrier().await;
    let runtime = claims_runtime(&mock.base, media);
    let env = seed_known_claims_job(&runtime, media, crate::db::open_db(":memory:").unwrap()).await;
    mock.release.notify_one();
    let client = Client::builder().no_proxy().build().unwrap();
    let first = run_claims_unit(&client, &runtime, env.clone(), media)
        .await
        .unwrap();
    env.db.lock().await.execute_batch(
        "CREATE TRIGGER deny_any_update BEFORE UPDATE ON system_config BEGIN SELECT RAISE(ABORT,'owned read-only replay'); END;
         CREATE TRIGGER deny_any_insert BEFORE INSERT ON system_config BEGIN SELECT RAISE(ABORT,'owned read-only replay'); END;"
    ).unwrap();
    let replay = run_claims_unit(&client, &runtime, env, media)
        .await
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(
        (
            mock.submits.load(Ordering::SeqCst),
            mock.polls.load(Ordering::SeqCst)
        ),
        (0, 1)
    );
}

#[tokio::test]
async fn poller_claim_text_ready_replay_is_read_only_before_new_claims_or_egress() {
    ready_replay_without_claim_write(false).await;
}

#[tokio::test]
async fn poller_claim_media_ready_replay_is_read_only_before_new_claims_or_egress() {
    ready_replay_without_claim_write(true).await;
}

#[tokio::test]
async fn poller_claim_media_owner_spans_result_download_and_durable_asset_save() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let mock = poll_barrier_at(true).await;
    let mut runtime = claims_runtime(&mock.base, true);
    runtime
        .template
        .async_poll
        .as_mut()
        .unwrap()
        .result_download = Some(
        serde_json::from_value(json!({
            "method": "GET", "url": format!("{}/download/{{{{computed.job_id}}}}", mock.base),
            "body_type": "none", "filename": "owned-result.mp4"
        }))
        .unwrap(),
    );
    let runtime = Arc::new(runtime);
    let env = seed_known_claims_job(&runtime, true, crate::db::open_db(":memory:").unwrap()).await;
    let client = Client::builder().no_proxy().build().unwrap();
    let owner = {
        let (client, runtime, env) = (client.clone(), runtime.clone(), env.clone());
        tokio::spawn(async move { run_claims_unit(&client, &runtime, env, true).await })
    };
    tokio::time::timeout(Duration::from_secs(5), mock.entered.notified())
        .await
        .unwrap();
    let contender = run_claims_unit(&client, &runtime, env.clone(), true).await;
    mock.release.notify_one();
    let result = owner.await.unwrap().unwrap();
    assert!(format!("{:#}", contender.unwrap_err()).contains("PROVIDER_UNIT_ALREADY_RUNNING"));
    let path = result["translated_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("file://")
        .unwrap();
    assert_eq!(
        crate::retained_assets::read(std::path::Path::new(path), 128).unwrap(),
        b"\"owned binary fixture\""
    );
    assert_ne!(std::fs::read(path).unwrap(), b"\"owned binary fixture\"");
    assert_eq!(
        run_claims_unit(&client, &runtime, env, true).await.unwrap(),
        result
    );
    assert_eq!(
        (
            mock.polls.load(Ordering::SeqCst),
            mock.downloads.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
}

const PROCESS_POLLER: &str =
    "component_rt::runner::tests::poller_claims::poller_claim_owned_process_worker";

struct OwnedChild(std::process::Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn process_poller(
    mock: &PollBarrier,
    root: &std::path::Path,
    media: bool,
    contender: bool,
) -> OwnedChild {
    let executable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let mut command = std::process::Command::new(executable);
    command
        .args([
            "--exact",
            PROCESS_POLLER,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("WPTSALL_OWNED_POLLER_PROCESS", "owned-poller-process-v1")
        .env("WPTSALL_OWNED_POLLER_BASE", &mock.base)
        .env("WPTSALL_OWNED_POLLER_MEDIA", if media { "1" } else { "0" })
        .env(
            "WPTSALL_OWNED_POLLER_CONTENDER",
            if contender { "1" } else { "0" },
        )
        .env(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-private-fixture-key",
        )
        .env("WPTSALL_DATA_DIR", root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    OwnedChild(command.spawn().unwrap())
}

async fn wait_owned_child(child: &mut OwnedChild) -> std::process::ExitStatus {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owned process must finish or be cleaned by its guard")
}

#[tokio::test]
#[ignore = "owned subprocess harness only; no saved credentials or live provider"]
async fn poller_claim_owned_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_POLLER_PROCESS").unwrap(),
        "owned-poller-process-v1"
    );
    let base = std::env::var("WPTSALL_OWNED_POLLER_BASE").unwrap();
    assert_eq!(
        reqwest::Url::parse(&base).unwrap().host_str(),
        Some("127.0.0.1")
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let media = std::env::var("WPTSALL_OWNED_POLLER_MEDIA").unwrap() == "1";
    let runtime = claims_runtime(&base, media);
    // Deliberately exercise the unit native lock, without letting the separate
    // whole-runtime lock make a broken unit lock look correct.
    let mut env = gap04_test_env(if media { "non_text" } else { "text" }, "owned-poller");
    env.db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let result = run_claims_unit(
        &Client::builder().no_proxy().build().unwrap(),
        &runtime,
        env,
        media,
    )
    .await;
    if std::env::var("WPTSALL_OWNED_POLLER_CONTENDER").unwrap() == "1" {
        assert!(format!("{:#}", result.unwrap_err()).contains("PROVIDER_UNIT_ALREADY_RUNNING"));
    } else {
        let result = result.unwrap();
        let path = root.join("owned-result.json");
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        std::io::Write::write_all(&mut output, result.to_string().as_bytes()).unwrap();
        output.sync_all().unwrap();
    }
}

async fn killed_known_poller(media: bool) {
    let _key = crate::db::TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-private-fixture-key",
    );
    let root = tempfile::tempdir().unwrap();
    let mock = poll_barrier().await;
    let runtime = claims_runtime(&mock.base, media);
    let env = seed_known_claims_job(
        &runtime,
        media,
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    )
    .await;
    let read_operation = || async {
        let conn = env.db.lock().await;
        let raw: String = conn
            .query_row(
                "SELECT value FROM system_config WHERE key LIKE 'provider-operation-v1:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<Value>(&crate::db::system::decrypt_config_value(&raw).unwrap())
            .unwrap()
    };
    let before = read_operation().await;
    let mut first = process_poller(&mock, root.path(), media, false);
    tokio::time::timeout(Duration::from_secs(30), mock.entered.notified())
        .await
        .unwrap();
    let mut contender = process_poller(&mock, root.path(), media, true);
    assert!(
        wait_owned_child(&mut contender).await.success(),
        "the second process must prove a live-owner refusal"
    );
    assert_eq!(mock.polls.load(Ordering::SeqCst), 1);
    first.0.kill().unwrap();
    let killed = first.0.wait().unwrap();
    assert!(!killed.success());
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(killed.signal(), Some(9));
    }
    assert_eq!(
        read_operation().await,
        before,
        "SIGKILL must retain the same paid job and attempt"
    );
    let mut replacement = process_poller(&mock, root.path(), media, false);
    assert!(
        wait_owned_child(&mut replacement).await.success(),
        "a fresh process must recover the original job"
    );
    let after = read_operation().await;
    assert_eq!(after["attempt_id"], before["attempt_id"]);
    assert_eq!(after["job_id"], before["job_id"]);
    assert_eq!(after["state"], "result_ready");
    mock.release.notify_one();
    let result: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("owned-result.json")).unwrap())
            .unwrap();
    assert_eq!(
        run_claims_unit(
            &Client::builder().no_proxy().build().unwrap(),
            &runtime,
            env,
            media
        )
        .await
        .unwrap(),
        result
    );
    assert_eq!(
        (
            mock.submits.load(Ordering::SeqCst),
            mock.polls.load(Ordering::SeqCst)
        ),
        (0, 2)
    );
    eprintln!("owned SIGKILL + fresh-process resume: original attempt/job, 0 submits, 2 polls, competing process refused");
}

#[tokio::test]
async fn poller_claim_text_sigkill_and_fresh_process_resume_keep_original_job() {
    killed_known_poller(false).await;
}

#[tokio::test]
async fn poller_claim_media_sigkill_and_fresh_process_resume_keep_original_job() {
    killed_known_poller(true).await;
}
