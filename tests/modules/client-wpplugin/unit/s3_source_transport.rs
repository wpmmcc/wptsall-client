//! Owned S3-compatible origin/proxy contracts, no shared mock port or AWS endpoint.

use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

struct Endpoint {
    url: String,
    calls: Arc<AtomicUsize>,
    requests: Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>,
    worker: JoinHandle<()>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn endpoint() -> Endpoint {
    endpoint_response(200, "").await
}

async fn endpoint_response(status: u16, response_body: &'static str) -> Endpoint {
    endpoint_response_after(status, response_body, Duration::ZERO).await
}

async fn endpoint_response_after(
    status: u16,
    response_body: &'static str,
    delay: Duration,
) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let observed_calls = calls.clone();
    let observed_requests = requests.clone();
    let worker = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let calls = observed_calls.clone();
            let requests = observed_requests.clone();
            handlers.spawn(async move {
                let mut raw = Vec::new();
                let mut buffer = [0u8; 2048];
                let mut needed = None;
                while raw.len() < 65536 {
                    let Ok(count) = socket.read(&mut buffer).await else {
                        return;
                    };
                    if count == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buffer[..count]);
                    if needed.is_none() {
                        if let Some(start) = raw.windows(4).position(|v| v == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&raw[..start]);
                            let size = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            needed = Some(start + 4 + size);
                        }
                    }
                    if needed.is_some_and(|length| raw.len() >= length) {
                        break;
                    }
                }
                calls.fetch_add(1, Ordering::SeqCst);
                requests.lock().await.push(raw);
                tokio::time::sleep(delay).await;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} Owned\r\nContent-Length: {}\r\nETag: \"owned\"\r\nConnection: close\r\n\r\n{response_body}",
                            response_body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });
    Endpoint {
        url,
        calls,
        requests,
        worker,
    }
}

fn runtime() -> ComponentRuntime {
    let template = serde_json::from_value(json!({
        "id": "owned-s3-transport",
        "name": "Owned S3 transport",
        "version": "1.0.0",
        "type": "document_translation",
        "request": { "method": "POST", "url": "http://127.0.0.1:1/not-used" },
        "response": {}
    }))
    .unwrap();
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
        proxy_profile_id: Some("owned-selected-proxy".into()),
    }
}

fn headers(endpoint: &str) -> Vec<(String, String)> {
    vec![
        ("x-amz-access-key-id".into(), "owned-access".into()),
        ("x-amz-secret-access-key".into(), "owned-secret".into()),
        ("x-amz-endpoint-url".into(), endpoint.into()),
    ]
}

fn asset() -> SourceAsset {
    SourceAsset {
        bytes: b"owned-s3-source".to_vec(),
        filename: "owned.txt".into(),
        content_type: "text/plain".into(),
    }
}

#[tokio::test]
async fn s3_put_uses_selected_proxy_and_never_contacts_owned_direct_origin() {
    let origin = endpoint().await;
    let proxy = endpoint().await;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(&proxy.url).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://owned-bucket/object.txt",
        &headers(&origin.url),
        &asset(),
    )
    .await
    .unwrap();
    assert_eq!(
        origin.calls.load(Ordering::SeqCst),
        0,
        "the selected S3 proxy must not fall back to the owned direct origin"
    );
    assert_eq!(proxy.calls.load(Ordering::SeqCst), 1);
    let raw = proxy.requests.lock().await;
    let request = String::from_utf8_lossy(&raw[0]);
    assert!(request.starts_with(&format!(
        "PUT {}/owned-bucket/object.txt?x-id=PutObject HTTP/1.1",
        origin.url
    )));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: aws4-hmac-sha256"));
    assert!(request.contains("owned-s3-source"));
}

#[tokio::test]
async fn s3_unknown_upload_does_not_blindly_retry_or_expose_remote_error_body() {
    let origin = endpoint_response(
        500,
        "<Error><Code>InternalError</Code><Message>owned-private-response-marker</Message></Error>",
    )
    .await;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let error = upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://owned-bucket/owned-private-object-marker",
        &headers(&origin.url),
        &asset(),
    )
    .await
    .expect_err("the owned unknown upload must fail");
    assert_eq!(
        origin.calls.load(Ordering::SeqCst),
        1,
        "an unknown remote upload effect must not trigger SDK automatic retries"
    );
    let detail = format!("{error:#}");
    assert!(!detail.contains("owned-private-response-marker"));
    assert!(!detail.contains("owned-private-object-marker"));
    assert!(!detail.contains("owned-secret"));
}

#[tokio::test]
async fn s3_terminal_error_does_not_expose_object_or_response_secrets() {
    let origin = endpoint_response(
        403,
        "<Error><Code>AccessDenied</Code><Message>owned-private-response-marker</Message></Error>",
    )
    .await;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let error = upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://owned-bucket/owned-private-object-marker",
        &headers(&origin.url),
        &asset(),
    )
    .await
    .expect_err("the owned forbidden upload must fail");
    assert_eq!(origin.calls.load(Ordering::SeqCst), 1);
    let detail = format!("{error:#}");
    assert!(!detail.contains("owned-private-object-marker"));
    assert!(!detail.contains("owned-private-response-marker"));
    assert!(!detail.contains("owned-secret"));
}

#[tokio::test]
async fn explicit_private_mock_uses_selected_proxy_without_aws_headers() {
    let origin = endpoint().await;
    let proxy = endpoint().await;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(&proxy.url).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let headers = vec![
        ("x-wptsall-s3-upload-profile".into(), "private_mock".into()),
        ("x-amz-endpoint-url".into(), origin.url.clone()),
        ("x-amz-access-key-id".into(), "unused-owned-access".into()),
        (
            "x-amz-secret-access-key".into(),
            "unused-owned-secret".into(),
        ),
        ("x-amz-session-token".into(), "unused-owned-session".into()),
    ];
    upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://owned-bucket/object.txt",
        &headers,
        &asset(),
    )
    .await
    .unwrap();
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
    assert_eq!(proxy.calls.load(Ordering::SeqCst), 1);
    let raw = proxy.requests.lock().await;
    let request = String::from_utf8_lossy(&raw[0]);
    assert!(request.starts_with(&format!(
        "PUT {}/mock-upload/owned-bucket/object.txt HTTP/1.1",
        origin.url
    )));
    assert!(request.contains("owned-s3-source"));
    for private in [
        "authorization:",
        "x-amz-",
        "unused-owned-access",
        "unused-owned-secret",
        "unused-owned-session",
        "x-wptsall-s3-upload-profile",
    ] {
        assert!(!request.to_ascii_lowercase().contains(private));
    }
}

#[tokio::test]
async fn private_mock_missing_endpoint_refuses_before_any_network() {
    let proxy = endpoint().await;
    let client = Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(&proxy.url).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let error = upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://mock-bucket/object.txt",
        &[("x-wptsall-s3-upload-profile".into(), "private_mock".into())],
        &asset(),
    )
    .await
    .expect_err("no implicit shared mock port or AWS endpoint is permitted");
    assert!(error.to_string().contains("explicit loopback endpoint"));
    assert_eq!(proxy.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_or_ambiguous_profile_and_empty_endpoint_refuse_before_network() {
    let origin = endpoint().await;
    let client = Client::builder().no_proxy().build().unwrap();
    for profile in ["", "unknown", "private-mock"] {
        let mut headers = headers(&origin.url);
        headers.push(("x-wptsall-s3-upload-profile".into(), profile.into()));
        assert!(upload_source_asset_to_s3(
            &client,
            &runtime(),
            "s3://owned-bucket/object.txt",
            &headers,
            &asset()
        )
        .await
        .is_err());
    }
    let mut duplicate = headers(&origin.url);
    duplicate.extend([
        ("x-wptsall-s3-upload-profile".into(), "private_mock".into()),
        ("X-WPTSALL-S3-Upload-Profile".into(), "aws_s3".into()),
    ]);
    assert!(upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://owned-bucket/object.txt",
        &duplicate,
        &asset()
    )
    .await
    .is_err());
    let mut empty = headers("");
    empty.push(("x-wptsall-s3-upload-profile".into(), "aws_s3".into()));
    assert!(upload_source_asset_to_s3(
        &client,
        &runtime(),
        "s3://owned-bucket/object.txt",
        &empty,
        &asset()
    )
    .await
    .is_err());
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn private_mock_endpoint_is_literal_loopback_and_url_segments_are_encoded() {
    for endpoint in [
        "http://example.invalid",
        "http://192.168.1.1",
        "http://0.0.0.0",
        "http://169.254.169.254",
        "http://user:pass@127.0.0.1:1234",
        "http://127.0.0.1:1234?token=owned",
        "http://127.0.0.1:1234#fragment",
        "file:///tmp/owned",
    ] {
        assert!(private_mock_s3_url(endpoint, "owned-bucket", "object").is_err());
    }
    let built = private_mock_s3_url(
        "http://127.0.0.1:1234/base/",
        "owned-bucket",
        "subdir/owned file?#.txt",
    )
    .unwrap();
    let parsed = url::Url::parse(&built).unwrap();
    assert_eq!(parsed.query(), None);
    assert_eq!(parsed.fragment(), None);
    assert_eq!(
        parsed.path(),
        "/base/mock-upload/owned-bucket/subdir/owned%20file%3F%23.txt"
    );
    assert!(private_mock_s3_url("http://[::1]:1234", "owned-bucket", "object").is_ok());
}

#[tokio::test]
async fn mock_named_bucket_and_keys_do_not_select_unsigned_profile() {
    let origin = endpoint().await;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    for access_key in ["mock-owned-access", "AKIA_TEST_OWNED"] {
        let mut headers = headers(&origin.url);
        headers[0].1 = access_key.into();
        assert_eq!(s3_upload_profile(&headers).unwrap(), S3UploadProfile::AwsS3);
        upload_source_asset_to_s3(
            &client,
            &runtime(),
            "s3://mock-owned-bucket/object.txt",
            &headers,
            &asset(),
        )
        .await
        .unwrap();
    }
    assert_eq!(origin.calls.load(Ordering::SeqCst), 2);
    let raw = origin.requests.lock().await;
    for raw in raw.iter() {
        let request = String::from_utf8_lossy(raw).to_ascii_lowercase();
        assert!(request.starts_with("put /mock-owned-bucket/object.txt?x-id=putobject http/1.1"));
        assert!(request.contains("authorization: aws4-hmac-sha256"));
        assert!(!request.contains("/mock-upload/"));
    }
}

#[tokio::test]
async fn selected_proxy_timeout_has_one_attempt_and_no_direct_fallback() {
    let origin = endpoint().await;
    let proxy = endpoint_response_after(200, "", Duration::from_secs(2)).await;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .proxy(reqwest::Proxy::all(&proxy.url).unwrap())
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        upload_source_asset_to_s3(
            &client,
            &runtime(),
            "s3://owned-bucket/object.txt",
            &headers(&origin.url),
            &asset(),
        ),
    )
    .await
    .expect("selected transport timeout must remain bounded");
    assert!(outcome.is_err());
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
    assert_eq!(proxy.calls.load(Ordering::SeqCst), 1);
}

fn upload_spec(headers: Vec<(String, String)>) -> ComponentSourceUpload {
    ComponentSourceUpload {
        http_limits: None,
        method: "PUT".into(),
        url: "s3://owned-bucket/object.txt".into(),
        headers: Some(headers.into_iter().collect()),
        body_type: Some("aws_s3_put_object".into()),
        success_statuses: Vec::new(),
        extract: HashMap::new(),
    }
}

#[tokio::test]
async fn parent_upload_does_not_drop_empty_profile_before_s3_validation() {
    let origin = endpoint().await;
    let mut headers = headers(&origin.url);
    headers.push(("x-wptsall-s3-upload-profile".into(), "".into()));
    let spec = upload_spec(headers);
    let client = Client::builder().no_proxy().build().unwrap();
    let error = upload_source_asset_to_vendor(
        &client,
        &runtime(),
        &spec,
        &mut HashMap::new(),
        "unused-owned-source",
        Some(&asset()),
    )
    .await
    .expect_err("an explicit empty profile must not silently become signed S3");
    assert!(error
        .to_string()
        .contains("invalid S3 source upload profile"));
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn parent_upload_does_not_drop_an_empty_case_duplicate_profile() {
    let origin = endpoint().await;
    let mut headers = headers(&origin.url);
    headers.extend([
        ("x-wptsall-s3-upload-profile".into(), "".into()),
        ("X-WPTSALL-S3-Upload-Profile".into(), "aws_s3".into()),
    ]);
    let spec = upload_spec(headers);
    let client = Client::builder().no_proxy().build().unwrap();
    let error = upload_source_asset_to_vendor(
        &client,
        &runtime(),
        &spec,
        &mut HashMap::new(),
        "unused-owned-source",
        Some(&asset()),
    )
    .await
    .expect_err("an empty case duplicate must not disappear before routing validation");
    assert!(error
        .to_string()
        .contains("duplicate S3 source upload profile"));
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn parent_upload_retains_empty_and_duplicate_endpoint_for_rejection() {
    let origin = endpoint().await;
    let client = Client::builder().no_proxy().build().unwrap();
    for duplicate in [false, true] {
        let mut values = headers("");
        values.push(("x-wptsall-s3-upload-profile".into(), "private_mock".into()));
        if duplicate {
            values.push(("X-AMZ-Endpoint-URL".into(), origin.url.clone()));
        }
        let spec = upload_spec(values);
        let error = upload_source_asset_to_vendor(
            &client,
            &runtime(),
            &spec,
            &mut HashMap::new(),
            "unused-owned-source",
            Some(&asset()),
        )
        .await
        .expect_err("explicit routing values must not vanish before validation");
        let expected = if duplicate {
            "duplicate S3 source upload endpoint"
        } else {
            "S3 source upload endpoint is empty"
        };
        assert!(error.to_string().contains(expected));
    }
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn parent_upload_private_profile_uses_proxy_and_verified_original_bytes() {
    let origin = endpoint().await;
    let proxy = endpoint().await;
    let client = Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::all(&proxy.url).unwrap())
        .build()
        .unwrap();
    let spec = upload_spec(vec![
        ("x-wptsall-s3-upload-profile".into(), "private_mock".into()),
        ("x-amz-endpoint-url".into(), origin.url.clone()),
    ]);
    upload_source_asset_to_vendor(
        &client,
        &runtime(),
        &spec,
        &mut HashMap::new(),
        "unused-owned-source",
        Some(&asset()),
    )
    .await
    .unwrap();
    assert_eq!(origin.calls.load(Ordering::SeqCst), 0);
    assert_eq!(proxy.calls.load(Ordering::SeqCst), 1);
    let raw = proxy.requests.lock().await;
    let request = String::from_utf8_lossy(&raw[0]);
    assert!(request.contains("owned-s3-source"));
    assert!(!request.to_ascii_lowercase().contains("authorization:"));
}
