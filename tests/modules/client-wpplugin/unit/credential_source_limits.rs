use super::*;
use crate::component_rt::key_pool::KeyPool;
use crate::component_rt::oauth::{OAuthHttpClient, OAuthPool, OAuthPoolEntry, OAuthTokenManager};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn credential_source_limits_byte_conversion_rejects_damage_and_rounds_up() {
    use crate::component_rt::file_limits::bytes_from_megabytes;
    assert_eq!(bytes_from_megabytes(0.0).unwrap(), 0);
    assert_eq!(bytes_from_megabytes(32.0 / 1_048_576.0).unwrap(), 32);
    assert_eq!(bytes_from_megabytes(0.5 / 1_048_576.0).unwrap(), 1);
    for bad in [
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::MAX,
        u64::MAX as f64 / 1_048_576.0,
    ] {
        assert!(bytes_from_megabytes(bad).is_err());
    }
}

#[test]
fn credential_source_limits_pool_budget_does_not_mask_damage_with_unlimited() {
    use crate::component_rt::file_limits::pool_source_budget;
    assert!(pool_source_budget(std::iter::empty()).is_err());
    assert_eq!(
        pool_source_budget([16.0 / 1_048_576.0, 32.0 / 1_048_576.0].into_iter()).unwrap(),
        32
    );
    assert_eq!(pool_source_budget([0.0, 32.0].into_iter()).unwrap(), 0);
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(pool_source_budget([0.0, bad].into_iter()).is_err());
        assert!(pool_source_budget([bad, 0.0].into_iter()).is_err());
    }
}

#[test]
fn credential_source_limits_combined_budget_keeps_strictest_positive_bound() {
    use crate::component_rt::file_limits::strictest;
    assert_eq!(strictest(0, 0), 0);
    assert_eq!(strictest(0, 32), 32);
    assert_eq!(strictest(32, 0), 32);
    assert_eq!(strictest(32, 16), 16);
    assert_eq!(strictest(strictest(64, 32), 16), 16);
}

struct OwnedEndpoints {
    base: String,
    task: tokio::task::JoinHandle<()>,
    gets: Arc<AtomicUsize>,
    paid: Arc<AtomicUsize>,
    tokens: Arc<AtomicUsize>,
    requests: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl Drop for OwnedEndpoints {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl OwnedEndpoints {
    async fn start(head_size: Option<usize>, first: usize, later: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let gets = Arc::new(AtomicUsize::new(0));
        let paid = Arc::new(AtomicUsize::new(0));
        let tokens = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (g, p, t, r, url) = (
            gets.clone(),
            paid.clone(),
            tokens.clone(),
            requests.clone(),
            base.clone(),
        );
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let (g, p, t, r, url) = (g.clone(), p.clone(), t.clone(), r.clone(), url.clone());
                handlers.spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let body_offset;
                    loop {
                        let count = socket.read(&mut buffer).await.unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(offset) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                            body_offset = offset + 4;
                            break;
                        }
                    }
                    let header = String::from_utf8_lossy(&bytes[..body_offset]).to_string();
                    let length = header.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    while bytes.len() < body_offset + length {
                        let count = socket.read(&mut buffer).await.unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    if header.starts_with("HEAD ") {
                        let size = head_size.map(|n| format!("Content-Length: {n}\r\n")).unwrap_or_default();
                        socket.write_all(format!("HTTP/1.1 200 OK\r\n{size}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
                    } else if header.starts_with("GET /source") {
                        let size = if g.fetch_add(1, Ordering::SeqCst) == 0 { first } else { later };
                        socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=\"owned.png\"\r\nConnection: close\r\n\r\n").await.unwrap();
                        socket.write_all(format!("{size:x}\r\n").as_bytes()).await.unwrap();
                        socket.write_all(&vec![7; size]).await.unwrap();
                        socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
                    } else {
                        let token = header.starts_with("POST /token");
                        if token { t.fetch_add(1, Ordering::SeqCst); } else { p.fetch_add(1, Ordering::SeqCst); }
                        r.lock().await.push(String::from_utf8_lossy(&bytes).to_string());
                        let body = if token { json!({"access_token":"owned-token","expires_in":3600}) }
                            else { json!({"output":format!("{url}/output.png")}) }.to_string();
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    }
                });
            }
        });
        Self {
            base,
            task,
            gets,
            paid,
            tokens,
            requests,
        }
    }

    fn runtime(&self, limits: &[usize]) -> ComponentRuntime {
        let template = serde_json::from_value(json!({
            "id":"owned-credential-size", "name":"Owned credential size", "version":"1", "type":"translation",
            "request":{"method":"POST", "url":format!("{}/paid",self.base),
                "headers":{"Authorization":"{{auth.api_key}}"},
                "body":{"asset":"{{input.source_base64}}"}},
            "response":{"translated_image_ref_path":"output"}
        })).unwrap();
        ComponentRuntime {
            template,
            auth_values: HashMap::new(),
            supported_business_lines: Vec::new(),
            language_map: HashMap::new(),
            supported_content_formats: Vec::new(),
            supported_formats: Vec::new(),
            key_pool: Some(Arc::new(KeyPool::new_with_ext(
                limits
                    .iter()
                    .enumerate()
                    .map(|(i, size)| {
                        (
                            format!("owned-key-{i}"),
                            HashMap::from([("api_key".into(), format!("owned-key-{i}"))]),
                            1,
                            1,
                            0,
                            *size as f64 / (1024.0 * 1024.0),
                        )
                    })
                    .collect(),
                KeySelectionStrategy::RoundRobin,
            ))),
            oauth_pool: None,
            oauth_manager: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
            proxy_profile_id: None,
        }
    }

    async fn translate(
        &self,
        runtime: &ComponentRuntime,
    ) -> anyhow::Result<NonTextComponentOutcome> {
        translate_non_text_via_component(
            &Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            runtime,
            "",
            None,
            &format!("{}/source", self.base),
            "image",
            "image_src",
            "en",
            "fr",
        )
        .await
    }
}

#[tokio::test]
async fn credential_source_limits_unknown_head_actual_over_limit_never_submits() {
    let endpoints = OwnedEndpoints::start(None, 33, 33).await;
    assert!(endpoints
        .translate(&endpoints.runtime(&[32]))
        .await
        .is_err());
    assert_eq!(endpoints.paid.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn credential_source_limits_lied_head_actual_over_limit_never_submits() {
    let endpoints = OwnedEndpoints::start(Some(16), 33, 33).await;
    assert!(endpoints
        .translate(&endpoints.runtime(&[32]))
        .await
        .is_err());
    assert_eq!(endpoints.paid.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn credential_source_limits_actual_size_selects_larger_member() {
    let endpoints = OwnedEndpoints::start(None, 24, 24).await;
    endpoints
        .translate(&endpoints.runtime(&[16, 32]))
        .await
        .unwrap();
    let requests = endpoints.requests.lock().await;
    assert!(
        requests[0].contains("owned-key-1"),
        "actual bytes must exclude smaller first member"
    );
}

#[tokio::test]
async fn credential_source_limits_verified_source_is_not_refetched_for_base64() {
    let endpoints = OwnedEndpoints::start(Some(16), 32, 33).await;
    endpoints
        .translate(&endpoints.runtime(&[32]))
        .await
        .unwrap();
    assert_eq!(
        endpoints.gets.load(Ordering::SeqCst),
        1,
        "metadata and base64 must use the measured original bytes"
    );
    assert!(endpoints.requests.lock().await[0].contains(&BASE64_STANDARD.encode(vec![7; 32])));
}

#[tokio::test]
async fn credential_source_limits_multipart_reuses_verified_original() {
    let endpoints = OwnedEndpoints::start(Some(16), 32, 33).await;
    let mut runtime = endpoints.runtime(&[32]);
    runtime.template.request.body_type = Some("multipart".into());
    runtime.template.request.body = Some(json!({"file":"@file:{{input.source_ref}}"}));
    endpoints.translate(&runtime).await.unwrap();
    assert_eq!(
        endpoints.gets.load(Ordering::SeqCst),
        1,
        "multipart must not fetch an unverified replacement"
    );
}

#[tokio::test]
async fn credential_source_limits_source_upload_reuses_verified_original() {
    let endpoints = OwnedEndpoints::start(Some(16), 32, 33).await;
    let mut runtime = endpoints.runtime(&[32]);
    runtime.template.source_upload = Some(ComponentSourceUpload {
        http_limits: None,
        method: "PUT".into(),
        url: format!("{}/upload", endpoints.base),
        headers: None,
        body_type: Some("binary_source".into()),
        success_statuses: Vec::new(),
        extract: HashMap::new(),
    });
    endpoints.translate(&runtime).await.unwrap();
    assert_eq!(
        endpoints.gets.load(Ordering::SeqCst),
        1,
        "upload must not fetch an unverified replacement"
    );
}

#[tokio::test]
async fn credential_source_limits_bad_member_budget_refuses_before_source_or_submit() {
    let endpoints = OwnedEndpoints::start(None, 16, 16).await;
    for invalid in [-1.0, f64::NAN, f64::INFINITY] {
        let mut runtime = endpoints.runtime(&[0]);
        runtime.key_pool = Some(Arc::new(KeyPool::new_with_ext(
            vec![("owned-invalid".into(), HashMap::new(), 1, 1, 0, invalid)],
            KeySelectionStrategy::RoundRobin,
        )));
        assert!(endpoints.translate(&runtime).await.is_err());
    }
    assert_eq!(endpoints.gets.load(Ordering::SeqCst), 0);
    assert_eq!(endpoints.paid.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn credential_source_limits_exact_and_zero_unlimited_still_submit() {
    for limit in [32, 0] {
        let endpoints = OwnedEndpoints::start(None, 32, 32).await;
        endpoints
            .translate(&endpoints.runtime(&[limit]))
            .await
            .unwrap();
        assert_eq!(endpoints.paid.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn credential_source_limits_oauth_over_limit_never_issues_token_or_submits() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let endpoints = OwnedEndpoints::start(None, 33, 33).await;
    let mut runtime = endpoints.runtime(&[32]);
    runtime.key_pool = None;
    runtime.template.request.headers = Some(HashMap::from([(
        "Authorization".into(),
        "{{auth.access_token}}".into(),
    )]));
    let config = serde_json::from_value(json!({
        "vendor_id":"owned-vendor","label":"Owned OAuth","grant_type":"client_credentials",
        "token_url":format!("{}/token",endpoints.base), "client_id":"owned-client",
        "client_secret":"owned-secret","token_field":"access_token"
    }))
    .unwrap();
    let manager = OAuthTokenManager::new(
        HashMap::from([("owned".into(), config)]),
        OAuthHttpClient::direct().unwrap(),
        root.path().join("oauth.json").display().to_string(),
    )
    .with_recovery_db(Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").unwrap(),
    )));
    runtime.oauth_manager = Some(Arc::new(manager));
    runtime.oauth_pool = Some(Arc::new(OAuthPool::new(
        vec![OAuthPoolEntry {
            config_id: "owned".into(),
            token_field: "access_token".into(),
            max_concurrent: 1,
            max_input_chars: 0,
            max_file_size_mb: 32.0 / (1024.0 * 1024.0),
            weight: 1,
            active_count: Arc::new(AtomicUsize::new(0)),
        }],
        KeySelectionStrategy::RoundRobin,
    )));
    assert!(endpoints.translate(&runtime).await.is_err());
    assert_eq!(endpoints.tokens.load(Ordering::SeqCst), 0);
    assert_eq!(endpoints.paid.load(Ordering::SeqCst), 0);
}
