//! Existing async_poll.timeout_seconds is exercised through the durable runner.
//! Endpoints, bytes, SQLite and credentials are synthetic and owned.
use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Copy)]
enum Reply {
    LateHeaders,
    LateBody,
    LateNextPoll,
    LongInterval,
    LateResultRequest,
    LateDownload,
    Immediate,
}

struct Endpoint {
    base: String,
    submits: Arc<AtomicUsize>,
    polls: Arc<AtomicUsize>,
    results: Arc<AtomicUsize>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

impl Endpoint {
    async fn start(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let submits = Arc::new(AtomicUsize::new(0));
        let polls = Arc::new(AtomicUsize::new(0));
        let results = Arc::new(AtomicUsize::new(0));
        let (submitted, polled, retrieved) = (submits.clone(), polls.clone(), results.clone());
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut socket, _) = accepted.unwrap();
                let (submitted, polled, retrieved) =
                    (submitted.clone(), polled.clone(), retrieved.clone());
                handlers.spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let mut needed = None;
                    loop {
                        let n = socket.read(&mut buffer).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                        assert!(bytes.len() < 65536);
                        if needed.is_none() {
                            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                                let header = String::from_utf8_lossy(&bytes[..end]);
                                let length = header.lines().find_map(|line| {
                                    let (key, value) = line.split_once(':')?;
                                    key.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok()).flatten()
                                }).unwrap_or(0);
                                needed = Some(end + 4 + length);
                            }
                        }
                        if needed.is_some_and(|n| bytes.len() >= n) {
                            break;
                        }
                    }
                    let header = String::from_utf8_lossy(&bytes);
                    let is_submit = header.starts_with("POST /submit ");
                    let is_result = header.starts_with("GET /result/owned-job ");
                    let ordinal = if is_submit {
                        submitted.fetch_add(1, Ordering::SeqCst);
                        0
                    } else if is_result {
                        retrieved.fetch_add(1, Ordering::SeqCst);
                        0
                    } else {
                        assert!(header.starts_with("GET /poll/owned-job "));
                        polled.fetch_add(1, Ordering::SeqCst)
                    };
                    let pending = !is_submit && matches!(reply, Reply::LateNextPoll | Reply::LongInterval) && ordinal == 0;
                    let body = if is_submit {
                        json!({"job_id":"owned-job"}).to_string()
                    } else if pending {
                        json!({"status":"processing"}).to_string()
                    } else {
                        json!({"status":"done","text":"Owned ready",
                            "result":{"video_url":"https://owned.invalid/result.mp4"}}).to_string()
                    };
                    if !is_submit
                        && (matches!(reply, Reply::LateHeaders) || pending
                            || (is_result && matches!(reply, Reply::LateResultRequest | Reply::LateDownload)))
                    {
                        tokio::time::sleep(Duration::from_millis(if pending { 350 } else { 1600 })).await;
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    if !is_submit && matches!(reply, Reply::LateBody) {
                        let _ = socket.write_all(&body.as_bytes()[..1]).await;
                        tokio::time::sleep(Duration::from_millis(1600)).await;
                        let _ = socket.write_all(&body.as_bytes()[1..]).await;
                    } else {
                        let _ = socket.write_all(body.as_bytes()).await;
                    }
                    let _ = socket.shutdown().await;
                });
            }
            handlers.shutdown().await;
        });
        Self {
            base,
            submits,
            polls,
            results,
            stop: Some(stop),
            worker: Some(worker),
        }
    }

    async fn finish(&mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.worker.take().unwrap().await.unwrap();
    }
}

fn runtime(base: &str, media: bool) -> ComponentRuntime {
    let mut template: ComponentTemplate = serde_json::from_value(json!({
        "id":"owned-poll-deadline","name":"Owned deadline","version":"1.0.0",
        "type":"text_translation",
        "request":{"method":"POST","url":format!("{base}/submit"),"body":{}},
        "response":{"translated_text_path":"text"},
        "async_poll":{
            "job_id_path":"job_id",
            "request":{"method":"GET","url":format!("{base}/poll/{{{{computed.job_id}}}}"),"body_type":"none"},
            "status_path":"status","done_values":["done"],"failed_values":["failed"],
            "interval_seconds":1,"timeout_seconds":1,"result_text_path":"text"
        }
    })).unwrap();
    if media {
        template.kind = "video_translation".into();
        template.response.translated_text_path = None;
        let poll = template.async_poll.as_mut().unwrap();
        poll.result_ref_path = Some("result.video_url".into());
        poll.result_text_path = None;
    }
    ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: Vec::new(),
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    }
}

fn env(root: &Path, media: bool) -> crate::db::async_jobs::AsyncJobEnv {
    crate::db::async_jobs::AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.join("owned.sqlite").to_str().unwrap()).unwrap(),
        )),
        domain: "http://127.0.0.1/owned".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 81,
        field_name: "owned-deadline".into(),
        chunk_index: 0,
        lane: if media { "non_text" } else { "text" },
        source_snapshot: None,
        resume_binding: None,
    }
}

async fn run(
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
            "data:video/mp4;base64,QUJD",
            "video",
            "owned-deadline",
            "en",
            "zh",
            Some(env),
        )
        .await?;
        Ok(json!({"translated_ref":result.translated_ref,"translated_text":result.translated_text}))
    } else {
        translate_text_via_component_with_env(client, runtime, "Hello", "en", "zh", Some(env))
            .await
            .map(Value::String)
    }
}

async fn deadline_case(reply: Reply, media: bool, must_time_out: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let mut endpoint = Endpoint::start(reply).await;
    let mut runtime = runtime(&endpoint.base, media);
    runtime.runtime_concurrency_sem = Some(Arc::new(tokio::sync::Semaphore::new(1)));
    if matches!(reply, Reply::LongInterval) {
        runtime
            .template
            .async_poll
            .as_mut()
            .unwrap()
            .interval_seconds = Some(5);
    }
    if matches!(reply, Reply::LateResultRequest | Reply::LateDownload) {
        let poll = runtime.template.async_poll.as_mut().unwrap();
        if matches!(reply, Reply::LateResultRequest) {
            poll.result_request = Some(serde_json::from_value(json!({
                "method":"GET","url":format!("{}/result/{{{{computed.job_id}}}}", endpoint.base),
                "body_type":"none"
            })).unwrap());
        } else {
            poll.result_download = Some(serde_json::from_value(json!({
                "method":"GET","url":format!("{}/result/{{{{computed.job_id}}}}", endpoint.base),
                "body_type":"none","filename":"owned.bin"
            })).unwrap());
        }
    }
    let env = env(root.path(), media);
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let started = Instant::now();
    let result = run(&client, &runtime, env.clone(), media).await;
    let elapsed = started.elapsed();
    let counts = (
        endpoint.submits.load(Ordering::SeqCst),
        endpoint.polls.load(Ordering::SeqCst),
    );
    assert_eq!(
        runtime
            .runtime_concurrency_sem
            .as_ref()
            .unwrap()
            .available_permits(),
        1
    );
    eprintln!(
        "owned deadline media={media} result={result:?} elapsed={elapsed:?} counts={counts:?}"
    );
    if must_time_out {
        assert!(
            result.is_err(),
            "existing one-second poll budget must not accept a late result"
        );
        assert!(
            elapsed < Duration::from_millis(1400),
            "poll await/sleep must obey the budget"
        );
        assert_eq!(counts, (1, 1), "no fresh submit or poll after expiry");
        let expected_results = usize::from(matches!(
            reply,
            Reply::LateResultRequest | Reply::LateDownload
        ));
        assert_eq!(endpoint.results.load(Ordering::SeqCst), expected_results);
        assert_eq!(
            crate::db::async_jobs::async_jobs_inventory(&*env.db.lock().await).unwrap(),
            (1, 0)
        );
        let assets = root.path().join("provider-assets");
        assert!(!assets.exists() || std::fs::read_dir(&assets).unwrap().next().is_none());
        let original: String = env
            .db
            .lock()
            .await
            .query_row(
                "SELECT value FROM system_config WHERE key LIKE 'provider-operation-v1:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(original.starts_with("V1BUQw"));
        let retry = run(&client, &runtime, env.clone(), media).await;
        assert!(
            retry.is_err(),
            "expired paid job stays retained, never fresh submission"
        );
        let retained: String = env
            .db
            .lock()
            .await
            .query_row(
                "SELECT value FROM system_config WHERE key LIKE 'provider-operation-v1:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, original);
        assert_eq!(
            (
                endpoint.submits.load(Ordering::SeqCst),
                endpoint.polls.load(Ordering::SeqCst)
            ),
            counts
        );
        assert_eq!(endpoint.results.load(Ordering::SeqCst), expected_results);
    } else {
        assert!(result.is_ok());
        assert_eq!(counts, (1, 1));
        let replay = run(&client, &runtime, env.clone(), media).await.unwrap();
        assert_eq!(replay, result.unwrap());
        assert_eq!(
            (
                endpoint.submits.load(Ordering::SeqCst),
                endpoint.polls.load(Ordering::SeqCst)
            ),
            counts
        );
    }
    endpoint.finish().await;
}

#[tokio::test]
async fn poll_deadline_text_late_headers_refuse() {
    deadline_case(Reply::LateHeaders, false, true).await;
}

#[tokio::test]
async fn poll_deadline_nontext_late_headers_refuse() {
    deadline_case(Reply::LateHeaders, true, true).await;
}

#[tokio::test]
async fn poll_deadline_text_incomplete_body_refuses_at_deadline() {
    deadline_case(Reply::LateBody, false, true).await;
}

#[tokio::test]
async fn poll_deadline_sleep_cannot_launch_a_late_poll() {
    deadline_case(Reply::LateNextPoll, false, true).await;
}

#[tokio::test]
async fn poll_deadline_text_immediate_result_still_works() {
    deadline_case(Reply::Immediate, false, false).await;
}

#[tokio::test]
async fn poll_deadline_nontext_immediate_result_still_works() {
    deadline_case(Reply::Immediate, true, false).await;
}

#[tokio::test]
async fn poll_deadline_nontext_result_request_uses_remaining_budget() {
    deadline_case(Reply::LateResultRequest, true, true).await;
}

#[tokio::test]
async fn poll_deadline_nontext_download_uses_remaining_budget() {
    deadline_case(Reply::LateDownload, true, true).await;
}

#[tokio::test]
async fn poll_deadline_long_interval_must_not_extend_timeout() {
    deadline_case(Reply::LongInterval, false, true).await;
}

#[tokio::test]
async fn poll_deadline_nontext_long_interval_must_not_extend_timeout() {
    deadline_case(Reply::LongInterval, true, true).await;
}

async fn known_job_waiting_for_permit(media: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let mut endpoint = Endpoint::start(Reply::Immediate).await;
    let mut runtime = runtime(&endpoint.base, media);
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    runtime.runtime_concurrency_sem = Some(semaphore.clone());
    let held = semaphore.clone().acquire_owned().await.unwrap();
    let input = if media {
        json!({
            "source_text":"","source_payload":null,
            "source_ref":"data:video/mp4;base64,QUJD",
            "task_type":"video","key":"owned-deadline",
        })
    } else {
        json!("Hello")
    };
    let env = env(root.path(), media)
        .for_runtime(&runtime, &input, "en", "zh")
        .unwrap();
    let context = HashMap::from([
        ("computed.job_id".into(), "owned-job".into()),
        ("computed.async_job_id".into(), "owned-job".into()),
    ]);
    crate::db::async_jobs::test_writes::upsert_polling_job(
        &env,
        &runtime.template.id,
        "owned-job",
        &context,
        "en",
        "zh",
    )
    .await
    .unwrap();
    crate::db::async_jobs::test_writes::save_provider_job(
        &env,
        &runtime.template.id,
        "owned-job",
        &context,
        "en",
        "zh",
    )
    .await
    .unwrap();
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let started = Instant::now();
    let result = run(&client, &runtime, env.clone(), media).await;
    let elapsed = started.elapsed();
    drop(held);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("timed out"), "{error}");
    assert!(elapsed < Duration::from_millis(1400));
    assert_eq!(semaphore.available_permits(), 1);
    assert_eq!(endpoint.submits.load(Ordering::SeqCst), 0);
    assert_eq!(endpoint.polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        crate::db::async_jobs::async_jobs_inventory(&*env.db.lock().await).unwrap(),
        (1, 0)
    );
    assert!(run(&client, &runtime, env, media).await.is_err());
    assert_eq!(endpoint.submits.load(Ordering::SeqCst), 0);
    assert_eq!(endpoint.polls.load(Ordering::SeqCst), 0);
    endpoint.finish().await;
}

#[tokio::test]
async fn poll_deadline_known_job_waiting_for_permit_has_no_fresh_submit() {
    known_job_waiting_for_permit(false).await;
}

#[tokio::test]
async fn poll_deadline_known_nontext_job_waiting_for_permit_has_no_fresh_submit() {
    known_job_waiting_for_permit(true).await;
}

async fn failure_save_error(media: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let mut endpoint = Endpoint::start(Reply::LateHeaders).await;
    let runtime = runtime(&endpoint.base, media);
    let env = env(root.path(), media);
    env.db
        .lock()
        .await
        .execute_batch(
            "CREATE TRIGGER deny_deadline_failure BEFORE UPDATE ON async_jobs
         WHEN NEW.status='failed' BEGIN SELECT RAISE(ABORT,'owned failure-save fault'); END;",
        )
        .unwrap();
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let error = format!(
        "{:#}",
        run(&client, &runtime, env.clone(), media)
            .await
            .unwrap_err()
    );
    assert!(
        error.contains("persist async terminal failure failed"),
        "{error}"
    );
    let saved = crate::db::async_jobs::find_polling_job(&env)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.job_id, "owned-job");
    assert_eq!(
        (
            endpoint.submits.load(Ordering::SeqCst),
            endpoint.polls.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    assert_eq!(
        crate::db::async_jobs::async_jobs_inventory(&*env.db.lock().await).unwrap(),
        (0, 1)
    );
    env.db
        .lock()
        .await
        .execute_batch("DROP TRIGGER deny_deadline_failure")
        .unwrap();
    let error = format!(
        "{:#}",
        run(&client, &runtime, env.clone(), media)
            .await
            .unwrap_err()
    );
    assert!(error.contains("timed out"), "{error}");
    assert_eq!(
        (
            endpoint.submits.load(Ordering::SeqCst),
            endpoint.polls.load(Ordering::SeqCst)
        ),
        (1, 2)
    );
    assert_eq!(
        crate::db::async_jobs::async_jobs_inventory(&*env.db.lock().await).unwrap(),
        (1, 0)
    );
    assert!(run(&client, &runtime, env, media).await.is_err());
    assert_eq!(
        (
            endpoint.submits.load(Ordering::SeqCst),
            endpoint.polls.load(Ordering::SeqCst)
        ),
        (1, 2)
    );
    endpoint.finish().await;
}

#[tokio::test]
async fn poll_deadline_text_failure_save_error_retains_original_job() {
    failure_save_error(false).await;
}

#[tokio::test]
async fn poll_deadline_nontext_failure_save_error_retains_original_job() {
    failure_save_error(true).await;
}
