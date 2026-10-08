//! New independent HTTP response/time contracts, using only owned synthetic endpoints.
use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PRIVATE: &str = "owned-http-budget-private";
const QUERY: &str = "owned-http-budget-query";

#[derive(Clone, Copy)]
enum Reply {
    Normal,
    Chunked,
    SlowHeaders,
    SlowBody,
}

struct Endpoint {
    url: String,
    calls: Arc<AtomicUsize>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn endpoint(body: &[u8], reply: Reply) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/owned?key={QUERY}",
        listener.local_addr().unwrap()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let body = body.to_vec();
    let worker = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let observed = observed.clone();
            handlers.spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8;4096];
                let mut expected = None;
                loop {
                    let count = stream.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 { return; }
                    request.extend_from_slice(&buffer[..count]);
                    assert!(request.len()<65536);
                    if expected.is_none() {
                        if let Some(end) = request.windows(4).position(|w| w==b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&request[..end]);
                            let size = head.lines().find_map(|line| {
                                let (key,value)=line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(||value.trim().parse::<usize>().ok()).flatten()
                            }).unwrap_or(0);
                            expected=Some(end+4+size);
                        }
                    }
                    if expected.is_some_and(|size|request.len()>=size) { break; }
                }
                observed.fetch_add(1,Ordering::SeqCst);
                if matches!(reply,Reply::SlowHeaders) {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                }
                let framing=if matches!(reply,Reply::Chunked) {
                    "Transfer-Encoding: chunked\r\n".into()
                } else { format!("Content-Length: {}\r\n",body.len()) };
                let head=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{framing}Connection: close\r\n\r\n");
                if stream.write_all(head.as_bytes()).await.is_err() { return; }
                if matches!(reply,Reply::SlowBody) {
                    let _=stream.write_all(&body[..1]).await;
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    let _=stream.write_all(&body[1..]).await;
                } else if matches!(reply,Reply::Chunked) {
                    let _=stream.write_all(format!("{:x}\r\n",body.len()).as_bytes()).await;
                    let _=stream.write_all(&body).await;
                    let _=stream.write_all(b"\r\n").await;
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    let _=stream.write_all(b"0\r\n\r\n").await;
                } else { let _=stream.write_all(&body).await; }
                let _=stream.shutdown().await;
            });
        }
    });
    Endpoint { url, calls, worker }
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

fn runtime(url: &str, bytes: u64, millis: u64) -> ComponentRuntime {
    ComponentRuntime {
        template: serde_json::from_value(json!({
            "id":"owned-http-budget","name":"Owned HTTP budget","version":"1.0.0",
            "type":"text_translation",
            "request":{"method":"POST","url":url,"body_type":"none",
                "http_limits":{"max_response_bytes":bytes,"timeout_ms":millis}},
            "response":{"translated_text_path":"translated"}
        }))
        .unwrap(),
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

fn safe_error(error: &anyhow::Error) {
    for formatted in [format!("{error:#}"), format!("{error:?}")] {
        assert!(
            !formatted.contains(PRIVATE),
            "no response content in errors"
        );
        assert!(!formatted.contains(QUERY), "no URL cause in errors");
    }
}

async fn json_refusal(reply: Reply, bytes: u64, millis: u64, expected: &str) {
    let body = json!({"translated":PRIVATE.repeat(4)}).to_string();
    let server = endpoint(body.as_bytes(), reply).await;
    let runtime = runtime(&server.url, bytes, millis);
    let started = Instant::now();
    let result = call_component_request_json(
        &client(),
        &runtime,
        &runtime.template.request,
        &mut HashMap::new(),
    )
    .await;
    let elapsed = started.elapsed();
    let error = result.expect_err("configured independent response/time limit must refuse");
    safe_error(&error);
    assert!(error.to_string().contains(expected), "{error:#}");
    assert!(
        elapsed < Duration::from_millis(500),
        "refuse before delayed EOF/headers/body: {elapsed:?}"
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_json_content_length_refuses() {
    json_refusal(Reply::Normal, 32, 2000, "response byte limit").await;
}

#[tokio::test]
async fn http_limit_json_chunked_refuses_before_eof() {
    json_refusal(Reply::Chunked, 32, 2000, "response byte limit").await;
}

#[tokio::test]
async fn http_limit_request_late_headers_refuse() {
    json_refusal(Reply::SlowHeaders, 4096, 100, "timed out").await;
}

#[tokio::test]
async fn http_limit_request_late_body_refuses() {
    json_refusal(Reply::SlowBody, 4096, 100, "timed out").await;
}

#[tokio::test]
async fn http_limit_binary_submit_refuses() {
    let server = endpoint(PRIVATE.repeat(4).as_bytes(), Reply::Normal).await;
    let runtime = runtime(&server.url, 32, 2000);
    let error = invoke_component_api_binary(
        &client(),
        &runtime,
        &runtime.template.request,
        &server.url,
        &HashMap::new(),
        SignResult::ContextOnly(HashMap::new()),
    )
    .await
    .unwrap_err();
    safe_error(&error);
    assert!(error.to_string().contains("response byte limit"));
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_download_refuses_before_eof() {
    let server = endpoint(PRIVATE.repeat(4).as_bytes(), Reply::Chunked).await;
    let runtime = runtime(&server.url, 4096, 2000);
    let download = serde_json::from_value(json!({
        "method":"GET","url":server.url,"body_type":"none",
        "http_limits":{"max_response_bytes":32,"timeout_ms":2000}
    }))
    .unwrap();
    let started = Instant::now();
    let error = call_component_request_binary(&client(), &runtime, &download, &mut HashMap::new())
        .await
        .unwrap_err();
    safe_error(&error);
    assert!(error.to_string().contains("response byte limit"));
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_source_upload_ack_refuses() {
    let server = endpoint(PRIVATE.repeat(4).as_bytes(), Reply::Normal).await;
    let runtime = runtime(&server.url, 4096, 2000);
    let upload = serde_json::from_value(json!({
        "method":"PUT","url":server.url,"body_type":"binary_source",
        "http_limits":{"max_response_bytes":32,"timeout_ms":2000}
    }))
    .unwrap();
    let error = upload_source_asset_to_vendor(
        &client(),
        &runtime,
        &upload,
        &mut HashMap::new(),
        "data:application/octet-stream;base64,QUJD",
        None,
    )
    .await
    .unwrap_err();
    safe_error(&error);
    assert!(error.to_string().contains("response byte limit"));
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_exact_json_and_independent_request_succeed() {
    let body = br#"{"translated":"ok"}"#;
    let server = endpoint(body, Reply::Normal).await;
    let runtime = runtime(&server.url, body.len() as u64, 2000);
    let result = call_component_request_json(
        &client(),
        &runtime,
        &runtime.template.request,
        &mut HashMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(result["translated"], "ok");
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn http_limit_absent_request_serialization_stays_legacy_compatible() {
    let before = json!({"method":"POST","url":"https://owned.invalid","headers":null,
        "body":null,"body_type":null,"response_type":null});
    let request: ComponentRequest = serde_json::from_value(before.clone()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), before);
}

#[test]
fn http_limit_wrong_types_and_unknown_fields_are_not_silent_defaults() {
    for value in [
        json!(null),
        json!(-1),
        json!(1.5),
        json!("owned-http-budget-private"),
        json!(true),
    ] {
        for field in ["max_response_bytes", "timeout_ms"] {
            let mut limits = json!({});
            limits[field] = value.clone();
            let error = serde_json::from_value::<ComponentHttpLimits>(limits).unwrap_err();
            assert!(!error.to_string().contains(PRIVATE));
        }
    }
    assert!(serde_json::from_value::<ComponentHttpLimits>(json!({"typo":32})).is_err());
}

#[tokio::test]
async fn http_limit_invalid_future_download_stops_before_fresh_submit() {
    let server = endpoint(br#"{"translated":"ok"}"#, Reply::Normal).await;
    let mut runtime = runtime(&server.url, 4096, 2000);
    runtime.template.async_poll=Some(serde_json::from_value(json!({
        "job_id_path":"job_id","request":{"method":"GET","url":server.url},
        "result_download":{"method":"GET","url":server.url,"http_limits":{"max_response_bytes":0}}
    })).unwrap());
    let error = translate_text_via_component(&client(), &runtime, "Hello", "en", "zh")
        .await
        .unwrap_err();
    assert!(error
        .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
        .is_some());
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn http_limit_queue_wait_obeys_phase_timeout_and_releases_permit() {
    let server = endpoint(br#"{"translated":"ok"}"#, Reply::Normal).await;
    let mut runtime = runtime(&server.url, 4096, 100);
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    runtime.runtime_concurrency_sem = Some(sem.clone());
    let held = sem.clone().acquire_owned().await.unwrap();
    let started = Instant::now();
    let error = call_component_request_json(
        &client(),
        &runtime,
        &runtime.template.request,
        &mut HashMap::new(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_millis(500));
    drop(held);
    assert_eq!(sem.available_permits(), 1);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn http_limit_incomplete_body_remains_an_error_below_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = socket.read(&mut [0u8; 4096]).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 32\r\nConnection: close\r\n\r\n{")
            .await
            .unwrap();
    });
    let runtime = runtime(&url, 4096, 2000);
    assert!(call_component_request_json(
        &client(),
        &runtime,
        &runtime.template.request,
        &mut HashMap::new()
    )
    .await
    .is_err());
    worker.await.unwrap();
}

#[tokio::test]
async fn http_limit_private_mock_s3_ack_is_bounded_by_parent() {
    let server = endpoint(PRIVATE.repeat(4).as_bytes(), Reply::Normal).await;
    let runtime = runtime(&server.url, 4096, 2000);
    let origin = reqwest::Url::parse(&server.url)
        .unwrap()
        .origin()
        .ascii_serialization();
    let upload = serde_json::from_value(json!({
        "method":"PUT","url":"s3://owned-bucket/owned-key","body_type":"aws_s3_put_object",
        "headers":{"x-wptsall-s3-upload-profile":"private_mock","x-amz-endpoint-url":origin},
        "http_limits":{"max_response_bytes":32,"timeout_ms":2000}
    }))
    .unwrap();
    let error = upload_source_asset_to_vendor(
        &client(),
        &runtime,
        &upload,
        &mut HashMap::new(),
        "data:application/octet-stream;base64,QUJD",
        None,
    )
    .await
    .unwrap_err();
    safe_error(&error);
    assert!(error.to_string().contains("response byte limit"));
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_signed_s3_ack_is_bounded_and_not_retried() {
    let server = endpoint(PRIVATE.repeat(4).as_bytes(), Reply::Normal).await;
    let runtime = runtime(&server.url, 4096, 2000);
    let origin = reqwest::Url::parse(&server.url)
        .unwrap()
        .origin()
        .ascii_serialization();
    let upload = serde_json::from_value(json!({
        "method":"PUT","url":"s3://owned-bucket/owned-key","body_type":"aws_s3_put_object",
        "headers":{"x-amz-access-key-id":"owned-access","x-amz-secret-access-key":"owned-secret",
            "x-amz-endpoint-url":origin},
        "http_limits":{"max_response_bytes":32,"timeout_ms":2000}
    }))
    .unwrap();
    let error = upload_source_asset_to_vendor(
        &client(),
        &runtime,
        &upload,
        &mut HashMap::new(),
        "data:application/octet-stream;base64,QUJD",
        None,
    )
    .await
    .unwrap_err();
    safe_error(&error);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_durable_unknown_submit_does_not_rebill_on_retry() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let server = endpoint(
        json!({"translated":PRIVATE.repeat(4)})
            .to_string()
            .as_bytes(),
        Reply::Normal,
    )
    .await;
    let runtime = runtime(&server.url, 32, 2000);
    let env = crate::db::async_jobs::AsyncJobEnv {
        db: Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        )),
        domain: "http://127.0.0.1/owned".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 81,
        field_name: "http-budget".into(),
        chunk_index: 0,
        lane: "text",
        source_snapshot: None,
        resume_binding: None,
    };
    let error = translate_text_via_component_with_env(
        &client(),
        &runtime,
        "Hello",
        "en",
        "zh",
        Some(env.clone()),
    )
    .await
    .unwrap_err();
    safe_error(&error);
    assert!(error.to_string().contains("response byte limit"));
    assert!(translate_text_via_component_with_env(
        &client(),
        &runtime,
        "Hello",
        "en",
        "zh",
        Some(env.clone())
    )
    .await
    .is_err());
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        crate::db::capacity::inventory(&*env.db.lock().await)
            .unwrap()
            .retained_units,
        1
    );
}

#[test]
fn http_limit_legacy_upload_download_serialization_remains_unchanged() {
    let download = json!({"method":"GET","url":"https://owned.invalid",
        "headers":null,"body":null,"body_type":null,"filename":null,"content_type":null});
    let parsed: ComponentAsyncDownload = serde_json::from_value(download.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), download);
    let upload = json!({"method":"PUT","url":"https://owned.invalid","headers":null,
        "body_type":null,"success_statuses":[],"extract":{}});
    let parsed: ComponentSourceUpload = serde_json::from_value(upload.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), upload);
}

#[tokio::test]
async fn http_limit_lossy_text_behavior_matches_existing_client() {
    for bytes in [
        b"\xef\xbb\xbfowned".as_slice(),
        b"owned\xff".as_slice(),
        "【zh】已保存".as_bytes(),
    ] {
        let server = endpoint(bytes, Reply::Normal).await;
        let client = client();
        let expected = client
            .get(&server.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let runtime = runtime(&server.url, bytes.len() as u64, 2000);
        let value = call_component_request_json(
            &client,
            &runtime,
            &runtime.template.request,
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(value["body"], expected);
    }
}

#[tokio::test]
async fn http_limit_oversized_default_json_refuses_without_explicit_limit() {
    let bytes = vec![b'x'; 16 * 1024 * 1024 + 1];
    let server = endpoint(&bytes, Reply::Normal).await;
    let mut runtime = runtime(&server.url, 4096, 2000);
    runtime.template.request.http_limits = None;
    let error = call_component_request_json(
        &client(),
        &runtime,
        &runtime.template.request,
        &mut HashMap::new(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("response byte limit"));
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_limit_binary_queue_wait_obeys_request_budget() {
    let server = endpoint(b"owned", Reply::Normal).await;
    let mut runtime = runtime(&server.url, 4096, 100);
    let sem = Arc::new(tokio::sync::Semaphore::new(1));
    runtime.runtime_concurrency_sem = Some(sem.clone());
    let held = sem.clone().acquire_owned().await.unwrap();
    let started = Instant::now();
    let error = call_component_request_binary_submit(
        &client(),
        &runtime,
        &runtime.template.request,
        &mut HashMap::new(),
        None,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_millis(500));
    drop(held);
    assert_eq!(sem.available_permits(), 1);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn http_limit_all_nonpositive_or_out_of_range_limits_stop_before_submit() {
    let server = endpoint(br#"{"translated":"ok"}"#, Reply::Normal).await;
    for (bytes, millis) in [
        (0, 2000),
        (4096, 0),
        (MAX_RESPONSE_BYTES + 1, 2000),
        (4096, MAX_TIMEOUT_MS + 1),
        (u64::MAX, 2000),
        (4096, u64::MAX),
    ] {
        let runtime = runtime(&server.url, bytes, millis);
        let error = translate_text_via_component(&client(), &runtime, "Hello", "en", "zh")
            .await
            .unwrap_err();
        assert!(error
            .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
            .is_some());
        assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    }
}
