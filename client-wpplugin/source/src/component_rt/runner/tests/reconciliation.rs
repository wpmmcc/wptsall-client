use super::*;

async fn read_owned_request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let (end, size) = loop {
        let mut buffer = [0; 4096];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let size = String::from_utf8_lossy(&bytes[..end])
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (end + 4, size);
        }
    };
    while bytes.len() < end + size {
        let mut buffer = [0; 4096];
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
    }
    (
        String::from_utf8(bytes[..end].to_vec()).unwrap(),
        if size > 0 {
            serde_json::from_slice(&bytes[end..end + size]).unwrap()
        } else {
            Value::Null
        },
    )
}

async fn saved_operation(env: &crate::db::async_jobs::AsyncJobEnv) -> Value {
    let conn = env.db.lock().await;
    let raw: String = conn
        .query_row(
            "SELECT value FROM system_config WHERE key LIKE 'provider-operation-v1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&crate::db::system::decrypt_config_value(&raw).unwrap()).unwrap()
}

#[tokio::test]
async fn provider_reconcile_identity_is_sent_before_unknown_submit() {
    let _key = crate::db::owned_mock_bindings_key();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut runtime = gap04_text_runtime(&base, "poll-text", 2);
    runtime.template.request.body = Some(json!({
        "client_reference":"{{operation.attempt_id}}",
        "binding":"{{operation.binding}}",
    }));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (_, body) = read_owned_request(&mut socket).await;
        drop(socket); // Upstream accepts the job, but its reply is lost.
        body
    });
    let env = gap04_test_env("text", "owned-reference");
    assert!(translate_text_via_component_with_env(
        &Client::new(),
        &runtime,
        "owned source",
        "en",
        "zh",
        Some(env.clone()),
    )
    .await
    .is_err());
    let body = server.await.unwrap();
    let operation = saved_operation(&env).await;
    assert_eq!(
        body["client_reference"], operation["attempt_id"],
        "the provider must receive the already committed operation identity"
    );
    assert_eq!(body["binding"], operation["binding"]);
    assert!(translate_text_via_component_with_env(
        &Client::new(),
        &runtime,
        "owned source",
        "en",
        "zh",
        Some(env.clone()),
    )
    .await
    .is_err());
    assert_eq!(
        saved_operation(&env).await,
        operation,
        "no blind resubmit or changed intent"
    );
}

#[tokio::test]
async fn provider_reconcile_freezes_prepare_context_before_main_submit() {
    let _key = crate::db::owned_mock_bindings_key();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut runtime = gap04_text_runtime(&base, "poll", 2);
    runtime.template.kind = "video_translation".into();
    runtime.template.response.translated_video_ref_path = Some("video_url".into());
    runtime.template.prepare = Some(ComponentPrepare {
        request: ComponentRequest {
            http_limits: None,
            method: "POST".into(),
            url: format!("{base}/prepare"),
            headers: None,
            body: Some(json!({})),
            body_type: None,
            response_type: None,
        },
        extract: HashMap::from([("upload_ref".into(), "upload_ref".into())]),
    });
    runtime.template.request.body = Some(json!({"upload_ref":"{{computed.upload_ref}}"}));
    let server = tokio::spawn(async move {
        let (mut prepare, _) = listener.accept().await.unwrap();
        let (header, _) = read_owned_request(&mut prepare).await;
        assert!(header.starts_with("POST /prepare "));
        let body = "{\"upload_ref\":\"owned-staged-source\"}";
        prepare
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        drop(prepare);
        let (mut submit, _) = listener.accept().await.unwrap();
        let (_, body) = read_owned_request(&mut submit).await;
        assert_eq!(body["upload_ref"], "owned-staged-source");
    });
    let env = gap04_test_env("non_text", "owned-prepare");
    assert!(translate_non_text_via_component_with_env(
        &Client::new(),
        &runtime,
        "",
        None,
        "",
        "video",
        "owned-prepare",
        "en",
        "zh",
        Some(env.clone()),
    )
    .await
    .is_err());
    server.await.unwrap();
    assert_eq!(
        saved_operation(&env).await["ctx"]["computed.upload_ref"],
        "owned-staged-source",
        "reconciliation must restore the context actually used for the lost submit"
    );
}

fn evidence_runtime(base: &str, media: bool) -> ComponentRuntime {
    let mut runtime = gap04_text_runtime(base, "poll-text", 2);
    runtime
        .auth_values
        .insert("auth.token".into(), "owned-frozen-token".into());
    runtime.template.request.body = Some(json!({
        "client_reference":"{{operation.attempt_id}}", "binding":"{{operation.binding}}",
    }));
    let poll = runtime.template.async_poll.as_mut().unwrap();
    poll.submit_extract
        .insert("poll_token".into(), "poll_token".into());
    poll.request.headers = Some(HashMap::from([
        ("Authorization".into(), "Bearer {{auth.token}}".into()),
        (
            "X-Owned-Poll-Token".into(),
            "{{computed.poll_token}}".into(),
        ),
    ]));
    poll.reconcile = Some(ComponentAsyncReconcile {
        request: ComponentRequest {
            http_limits: None,
            method: "GET".into(),
            url: format!("{base}/evidence/{{{{operation.attempt_id}}}}"),
            headers: Some(HashMap::from([(
                "Authorization".into(),
                "Bearer {{auth.token}}".into(),
            )])),
            body: None,
            body_type: Some("none".into()),
            response_type: Some("json".into()),
        },
        matches_path: "matches".into(),
        attempt_id_path: "client_reference".into(),
        binding_path: "binding".into(),
        job_id_path: "job_id".into(),
        status_path: "status".into(),
        resumable_values: vec!["processing".into(), "completed".into()],
    });
    if media {
        runtime.template.kind = "video_translation".into();
        runtime.template.response.translated_video_ref_path = Some("result.video_url".into());
        poll.result_ref_path = Some("result.video_url".into());
    }
    runtime
}

struct OwnedEvidenceMock {
    requests: Arc<tokio::sync::Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for OwnedEvidenceMock {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn evidence_mock(mode: &str, media: bool) -> (ComponentRuntime, OwnedEvidenceMock) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let runtime = evidence_runtime(&base, media);
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let mode = mode.to_string();
    let server = tokio::spawn(async move {
        let mut submitted = Value::Null;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (header, body) = read_owned_request(&mut socket).await;
            seen.lock().await.push(header.clone());
            if header.starts_with("POST /submit ") {
                submitted = body;
                continue; // Accepted once. Lose the response.
            }
            let (status, body) = if header.starts_with("GET /evidence/") {
                assert!(header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer owned-frozen-token"));
                let mut record = json!({
                    "client_reference":submitted["client_reference"], "binding":submitted["binding"],
                    "job_id":"job789", "status":"completed", "poll_token":"owned-poll-token",
                });
                match mode.as_str() {
                    "wrong_attempt" => {
                        record["client_reference"] = json!(uuid::Uuid::new_v4().to_string())
                    }
                    "wrong_binding" => record["binding"] = json!("different-runtime"),
                    "wrong_job_shape" => record["job_id"] = json!({"id":"job789"}),
                    "unsafe_job" => record["job_id"] = json!("../other?token=bad"),
                    "zero_job" => record["job_id"] = json!(0),
                    "failed" => record["status"] = json!("failed"),
                    "no_extract" => {
                        record.as_object_mut().unwrap().remove("poll_token");
                    }
                    _ => {}
                }
                let matches = match mode.as_str() {
                    "ambiguous" => json!([record.clone(), record]),
                    "absent" => json!([]),
                    _ => json!([record]),
                };
                let body = if mode == "logical_error" {
                    json!({"matches":matches, "error":{"message":"owned refusal"}})
                } else {
                    json!({"matches":matches})
                };
                let status = if mode == "http_error" {
                    "403 Forbidden"
                } else if mode == "redirect" {
                    "302 Found"
                } else {
                    "200 OK"
                };
                (status, body)
            } else {
                assert!(header.starts_with("GET /poll-text/job789 "));
                assert!(header
                    .to_ascii_lowercase()
                    .contains("x-owned-poll-token: owned-poll-token"));
                assert!(header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer owned-frozen-token"));
                (
                    "200 OK",
                    json!({
                        "data":{"job789":{"status":"completed","translated_text":"owned-recovered"}},
                        "result":{"video_url":"https://owned.example/recovered.mp4"},
                    }),
                )
            };
            let body = serde_json::to_vec(&body).unwrap();
            socket.write_all(format!("HTTP/1.1 {status}\r\nLocation: /submit\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    (runtime, OwnedEvidenceMock { requests, server })
}

async fn invoke_owned(
    runtime: &ComponentRuntime,
    env: &crate::db::async_jobs::AsyncJobEnv,
    media: bool,
) -> anyhow::Result<Value> {
    if media {
        translate_non_text_via_component_with_env(&Client::new(), runtime, "", None, "", "video", "owned-recovery", "en", "zh", Some(env.clone()))
            .await.map(|outcome| json!({"translated_ref":outcome.translated_ref, "translated_text":outcome.translated_text}))
    } else {
        translate_text_via_component_with_env(
            &Client::new(),
            runtime,
            "owned source",
            "en",
            "zh",
            Some(env.clone()),
        )
        .await
        .map(|result| json!(result))
    }
}

async fn owned_unknown(
    runtime: &ComponentRuntime,
    media: bool,
) -> crate::db::async_jobs::AsyncJobEnv {
    let env = gap04_test_env(if media { "non_text" } else { "text" }, "owned-recovery");
    assert!(invoke_owned(runtime, &env, media).await.is_err());
    env
}

async fn successful_reconciliation(media: bool) {
    let _key = crate::db::owned_mock_bindings_key();
    let (mut runtime, mock) = evidence_mock("valid", media).await;
    let env = owned_unknown(&runtime, media).await;
    let op = saved_operation(&env).await;
    let review = crate::db::async_jobs::review_provider_operation(
        &env.db,
        op["attempt_id"].as_str().unwrap(),
    )
    .await
    .unwrap();
    // Changing the selected credentials must not retarget the already paid job.
    runtime
        .auth_values
        .insert("auth.token".into(), "different-selected-token".into());
    assert_eq!(
        provider_recovery::reconcile(&review).await.unwrap(),
        "job789"
    );
    assert_eq!(saved_operation(&env).await["state"], "polling");
    let output = invoke_owned(&runtime, &env, media).await.unwrap();
    if media {
        assert_eq!(
            output["translated_ref"],
            "https://owned.example/recovered.mp4"
        );
    } else {
        assert_eq!(output, json!("owned-recovered"));
    }
    let ready = invoke_owned(&runtime, &env, media).await.unwrap();
    assert_eq!(output, ready);
    let requests = mock.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests.iter().filter(|r| r.starts_with("POST ")).count(),
        1,
        "reconciliation and restart must not make a second paid submission"
    );
}

#[tokio::test]
async fn provider_reconcile_verified_text_resumes_original_job_without_resubmit() {
    successful_reconciliation(false).await;
}

#[tokio::test]
async fn provider_reconcile_verified_media_resumes_original_job_without_resubmit() {
    successful_reconciliation(true).await;
}

#[tokio::test]
async fn provider_reconcile_missing_ambiguous_foreign_failed_and_malformed_proofs_retain_intent() {
    let _key = crate::db::owned_mock_bindings_key();
    for mode in [
        "wrong_attempt",
        "wrong_binding",
        "wrong_job_shape",
        "unsafe_job",
        "zero_job",
        "failed",
        "no_extract",
        "ambiguous",
        "absent",
        "http_error",
        "redirect",
        "logical_error",
    ] {
        let (mut runtime, mock) = evidence_mock(mode, false).await;
        runtime.template.response.error_path = Some("error.message".into());
        let env = owned_unknown(&runtime, false).await;
        let before = saved_operation(&env).await;
        let review = crate::db::async_jobs::review_provider_operation(
            &env.db,
            before["attempt_id"].as_str().unwrap(),
        )
        .await
        .unwrap();
        assert!(
            provider_recovery::reconcile(&review).await.is_err(),
            "{mode}"
        );
        assert_eq!(
            saved_operation(&env).await,
            before,
            "{mode}: evidence must be retained"
        );
        assert!(invoke_owned(&runtime, &env, false).await.is_err());
        assert_eq!(
            mock.requests.lock().await.len(),
            2,
            "{mode}: only submit and read-only query"
        );
    }
}

#[tokio::test]
async fn provider_reconcile_stale_reviews_cannot_overwrite_a_newer_saved_job() {
    let _key = crate::db::owned_mock_bindings_key();
    let (runtime, mock) = evidence_mock("valid", false).await;
    let env = owned_unknown(&runtime, false).await;
    let before = saved_operation(&env).await;
    let id = before["attempt_id"].as_str().unwrap();
    let first = crate::db::async_jobs::review_provider_operation(&env.db, id)
        .await
        .unwrap();
    let stale = crate::db::async_jobs::review_provider_operation(&env.db, id)
        .await
        .unwrap();
    provider_recovery::reconcile(&first).await.unwrap();
    let committed = saved_operation(&env).await;
    assert!(provider_recovery::reconcile(&stale).await.is_err());
    assert_eq!(saved_operation(&env).await, committed);
    assert_eq!(
        mock.requests
            .lock()
            .await
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_reconcile_write_refusal_retains_original_unknown_ciphertext() {
    let _key = crate::db::owned_mock_bindings_key();
    let (runtime, _mock) = evidence_mock("valid", false).await;
    let env = owned_unknown(&runtime, false).await;
    let before = saved_operation(&env).await;
    let review = crate::db::async_jobs::review_provider_operation(
        &env.db,
        before["attempt_id"].as_str().unwrap(),
    )
    .await
    .unwrap();
    env.db.lock().await.execute_batch("CREATE TRIGGER owned_deny_update BEFORE UPDATE ON system_config BEGIN SELECT RAISE(IGNORE); END;").unwrap();
    assert!(provider_recovery::reconcile(&review).await.is_err());
    assert_eq!(saved_operation(&env).await, before);
}

#[tokio::test]
async fn provider_reconcile_unsupported_pre_submit_and_changed_snapshot_are_refused() {
    let _key = crate::db::owned_mock_bindings_key();
    let env = gap04_test_env("text", "owned-legacy");
    let bound = env
        .for_runtime(
            &gap04_text_runtime("http://127.0.0.1:1", "poll-text", 2),
            &json!("owned source"),
            "en",
            "zh",
        )
        .unwrap();
    crate::db::async_jobs::test_writes::begin_provider_intent(
        &bound,
        "owned",
        &HashMap::new(),
        "en",
        "zh",
    )
    .await
    .unwrap();
    let op = saved_operation(&bound).await;
    assert!(
        !crate::db::async_jobs::list_provider_operations(&bound.db)
            .await
            .unwrap()[0]
            .can_reconcile
    );
    assert!(crate::db::async_jobs::review_provider_operation(
        &bound.db,
        op["attempt_id"].as_str().unwrap()
    )
    .await
    .is_err());

    let runtime = evidence_runtime("http://127.0.0.1:1", false);
    let env = gap04_test_env("text", "owned-prepared");
    let bound = env
        .for_runtime(&runtime, &json!("owned source"), "en", "zh")
        .unwrap();
    let mut ctx = HashMap::new();
    crate::db::async_jobs::test_writes::begin_provider_runtime_intent(
        &bound,
        &runtime,
        &json!("owned source"),
        &mut ctx,
        "en",
        "zh",
    )
    .await
    .unwrap();
    let op = saved_operation(&bound).await;
    assert!(crate::db::async_jobs::review_provider_operation(
        &bound.db,
        op["attempt_id"].as_str().unwrap()
    )
    .await
    .is_err());
    crate::db::async_jobs::test_writes::commit_provider_submit_context(&bound, &ctx)
        .await
        .unwrap();
    let mut changed = saved_operation(&bound).await;
    changed["recovery"]["template"]["request"]["url"] = json!("https://different.example/submit");
    let raw = crate::db::system::encrypt_config_value(&changed.to_string()).unwrap();
    bound
        .db
        .lock()
        .await
        .execute(
            "UPDATE system_config SET value=?1 WHERE key LIKE 'provider-operation-v1:%'",
            [raw],
        )
        .unwrap();
    assert!(crate::db::async_jobs::list_provider_operations(&bound.db)
        .await
        .is_err());
    assert!(crate::db::async_jobs::review_provider_operation(
        &bound.db,
        op["attempt_id"].as_str().unwrap()
    )
    .await
    .is_err());
}

#[test]
fn provider_reconcile_contract_rejects_submit_queries_and_unsent_identity_markers() {
    let mut runtime = evidence_runtime("https://owned.example", false);
    assert!(provider_recovery::validate_contract(&runtime.template).is_ok());
    runtime
        .template
        .async_poll
        .as_mut()
        .unwrap()
        .reconcile
        .as_mut()
        .unwrap()
        .request
        .method = "POST".into();
    assert!(provider_recovery::validate_contract(&runtime.template).is_err());
    runtime
        .template
        .async_poll
        .as_mut()
        .unwrap()
        .reconcile
        .as_mut()
        .unwrap()
        .request
        .method = "GET".into();
    runtime.template.request.body_type = Some("none".into());
    assert!(provider_recovery::validate_contract(&runtime.template).is_err());
}

#[tokio::test]
async fn provider_reconcile_unknown_and_adopted_job_survive_sqlite_reopen() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-provider.sqlite");
    let (runtime, mock) = evidence_mock("valid", false).await;
    let mut env = gap04_test_env("text", "owned-recovery");
    let connection = rusqlite::Connection::open(&path).unwrap();
    crate::db::schema::create_tables(&connection).unwrap();
    env.db = Arc::new(tokio::sync::Mutex::new(connection));
    assert!(invoke_owned(&runtime, &env, false).await.is_err());
    let before = saved_operation(&env).await;
    let id = before["attempt_id"].as_str().unwrap();
    let ciphertext: String = env
        .db
        .lock()
        .await
        .query_row(
            "SELECT value FROM system_config WHERE key LIKE 'provider-operation-v1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    for private in ["owned-frozen-token", "owned source", "/submit", id] {
        assert!(!ciphertext.contains(private));
    }
    env.db = Arc::new(tokio::sync::Mutex::new(
        rusqlite::Connection::open(&path).unwrap(),
    ));
    assert!(invoke_owned(&runtime, &env, false).await.is_err());
    let review = crate::db::async_jobs::review_provider_operation(&env.db, id)
        .await
        .unwrap();
    provider_recovery::reconcile(&review).await.unwrap();
    drop(review);
    env.db = Arc::new(tokio::sync::Mutex::new(
        rusqlite::Connection::open(&path).unwrap(),
    ));
    assert_eq!(
        invoke_owned(&runtime, &env, false).await.unwrap(),
        json!("owned-recovered")
    );
    assert_eq!(
        mock.requests
            .lock()
            .await
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_reconcile_competing_reviews_commit_only_once() {
    let _key = crate::db::owned_mock_bindings_key();
    let (runtime, mock) = evidence_mock("valid", false).await;
    let env = owned_unknown(&runtime, false).await;
    let op = saved_operation(&env).await;
    let mut reviews = Vec::new();
    for _ in 0..16 {
        reviews.push(
            crate::db::async_jobs::review_provider_operation(
                &env.db,
                op["attempt_id"].as_str().unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = Vec::new();
    for review in reviews {
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            provider_recovery::reconcile(&review).await.is_ok()
        }));
    }
    let mut committed = 0;
    for task in tasks {
        committed += usize::from(task.await.unwrap());
    }
    assert_eq!(committed, 1);
    assert_eq!(saved_operation(&env).await["state"], "polling");
    assert_eq!(
        mock.requests
            .lock()
            .await
            .iter()
            .filter(|r| r.starts_with("POST "))
            .count(),
        1
    );
}

const PROCESS_WORKER: &str =
    "component_rt::runner::tests::reconciliation::provider_recovery_process_worker";

fn owned_worker_command(
    base: &str,
    root: &std::path::Path,
    recover: bool,
) -> std::process::Command {
    let executable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let mut command = std::process::Command::new(executable);
    command
        .args(["--exact", PROCESS_WORKER, "--ignored", "--test-threads=1"])
        .env(
            "WPTSALL_OWNED_PROVIDER_PROCESS",
            "owned-provider-process-v1",
        )
        .env("WPTSALL_OWNED_PROVIDER_BASE", base)
        .env(
            "WPTSALL_OWNED_PROVIDER_RECOVER",
            if recover { "1" } else { "0" },
        )
        .env("WPTSALL_DATA_DIR", root)
        .env(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-private-fixture-key",
        )
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
}

#[tokio::test]
#[ignore = "owned subprocess harness only; no live provider or saved credentials"]
async fn provider_recovery_process_worker() {
    assert_eq!(
        std::env::var("WPTSALL_OWNED_PROVIDER_PROCESS").unwrap(),
        "owned-provider-process-v1"
    );
    let base = std::env::var("WPTSALL_OWNED_PROVIDER_BASE").unwrap();
    assert_eq!(
        reqwest::Url::parse(&base).unwrap().host_str(),
        Some("127.0.0.1")
    );
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let connection = rusqlite::Connection::open(root.join("owned-provider.sqlite")).unwrap();
    crate::db::schema::create_tables(&connection).unwrap();
    let mut env = gap04_test_env("text", "owned-recovery");
    env.db = Arc::new(tokio::sync::Mutex::new(connection));
    let runtime = evidence_runtime(&base, false);
    if std::env::var("WPTSALL_OWNED_PROVIDER_RECOVER").unwrap() == "1" {
        assert!(invoke_owned(&runtime, &env, false).await.is_err());
        let op = saved_operation(&env).await;
        let review = crate::db::async_jobs::review_provider_operation(
            &env.db,
            op["attempt_id"].as_str().unwrap(),
        )
        .await
        .unwrap();
        provider_recovery::reconcile(&review).await.unwrap();
    }
    assert_eq!(
        invoke_owned(&runtime, &env, false).await.unwrap(),
        json!("owned-recovered")
    );
}

#[tokio::test]
async fn provider_reconcile_fresh_process_after_accepted_submit_keeps_same_operation_and_one_submit(
) {
    let _key = crate::db::TestEnvVarGuard::set(
        "WPTSALL_COMPONENT_BINDINGS_SECRET",
        "owned-private-fixture-key",
    );
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(tokio::sync::Notify::new());
    let signal = Arc::clone(&accepted);
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        let mut submitted = Value::Null;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (header, body) = read_owned_request(&mut socket).await;
            seen.lock().await.push(header.clone());
            if header.starts_with("POST /submit ") {
                assert!(
                    submitted.is_null(),
                    "fresh process resubmitted a paid operation"
                );
                submitted = body;
                signal.notify_one();
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
                continue;
            }
            let body = if header.starts_with("GET /evidence/") {
                assert!(header.starts_with(&format!(
                    "GET /evidence/{} ",
                    submitted["client_reference"].as_str().unwrap()
                )));
                json!({"matches":[{"client_reference":submitted["client_reference"],"binding":submitted["binding"],
                    "job_id":"job789","status":"completed","poll_token":"owned-poll-token"}]})
            } else {
                assert!(header.starts_with("GET /poll-text/job789 "));
                json!({"data":{"job789":{"status":"completed","translated_text":"owned-recovered"}}})
            };
            let body = body.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let mut first = owned_worker_command(&base, root.path(), false)
        .spawn()
        .unwrap();
    if tokio::time::timeout(std::time::Duration::from_secs(30), accepted.notified())
        .await
        .is_err()
    {
        let _ = first.kill();
        let _ = first.wait();
        server.abort();
        panic!("owned provider process did not reach accepted submit");
    }
    first.kill().unwrap();
    assert!(!first.wait().unwrap().success());
    let connection = rusqlite::Connection::open(root.path().join("owned-provider.sqlite")).unwrap();
    let mut env = gap04_test_env("text", "owned-recovery");
    env.db = Arc::new(tokio::sync::Mutex::new(connection));
    let before = saved_operation(&env).await;
    assert_eq!(before["state"], "submit_unknown");
    let mut second = owned_worker_command(&base, root.path(), true)
        .spawn()
        .unwrap();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(status) = second.try_wait().unwrap() {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    if outcome.is_err() {
        let _ = second.kill();
        let _ = second.wait();
        server.abort();
        panic!("owned provider recovery process did not finish");
    }
    assert!(
        outcome.unwrap().success(),
        "fresh process did not recover original provider job"
    );
    let after = saved_operation(&env).await;
    assert_eq!(after["attempt_id"], before["attempt_id"]);
    assert_eq!(after["state"], "result_ready");
    assert!(after["recovery"]["evidence"]["record"].is_object());
    let requests = requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests.iter().filter(|r| r.starts_with("POST ")).count(),
        1
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn provider_reconcile_templates_require_persistence_before_any_egress() {
    let _key = crate::db::owned_mock_bindings_key();
    let (runtime, mock) = evidence_mock("valid", false).await;
    assert!(translate_text_via_component_with_env(
        &Client::new(),
        &runtime,
        "owned source",
        "en",
        "zh",
        None
    )
    .await
    .is_err());
    assert!(translate_non_text_via_component_with_env(
        &Client::new(),
        &runtime,
        "",
        None,
        "",
        "video",
        "owned-recovery",
        "en",
        "zh",
        None
    )
    .await
    .is_err());
    assert!(mock.requests.lock().await.is_empty());
}

#[tokio::test]
async fn provider_reconcile_configured_proxy_does_not_fall_back_to_direct_query() {
    let _key = crate::db::owned_mock_bindings_key();
    let (mut runtime, mock) = evidence_mock("valid", false).await;
    runtime.proxy_profile_id = Some("owned-selected-proxy".into());
    let env = owned_unknown(&runtime, false).await;
    let op = saved_operation(&env).await;
    assert!(
        !crate::db::async_jobs::list_provider_operations(&env.db)
            .await
            .unwrap()[0]
            .can_reconcile
    );
    assert!(crate::db::async_jobs::review_provider_operation(
        &env.db,
        op["attempt_id"].as_str().unwrap()
    )
    .await
    .is_err());
    assert_eq!(mock.requests.lock().await.len(), 1);
}
