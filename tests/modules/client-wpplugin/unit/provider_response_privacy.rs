//! All request/response content below is synthetic, on owned ephemeral HTTP endpoints.

use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PRIVATE: &str = "owned-response-private-marker";
const QUERY: &str = "owned-query-private-marker";

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

async fn endpoint(status: u16, body: &str, close_without_response: bool) -> Endpoint {
    endpoint_framed(status, body, close_without_response, None).await
}

async fn endpoint_framed(
    status: u16,
    body: &str,
    close_without_response: bool,
    announced: Option<usize>,
) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/owned?api_key={QUERY}",
        listener.local_addr().unwrap()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let body = body.as_bytes().to_vec();
    let worker = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let observed = observed.clone();
            handlers.spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                let mut target = None;
                loop {
                    let count = stream.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if target.is_none() {
                        if let Some(start) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&bytes[..start]);
                            let length = header.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            }).unwrap_or(0);
                            target = Some(start + 4 + length);
                        }
                    }
                    if target.is_some_and(|length| bytes.len() >= length) {
                        break;
                    }
                    assert!(bytes.len() < 65536, "only bounded synthetic requests are used");
                }
                observed.fetch_add(1, Ordering::SeqCst);
                if close_without_response {
                    let _ = stream.shutdown().await;
                    return;
                }
                let header = format!(
                    "HTTP/1.1 {status} Owned\r\nContent-Type: application/json\r\nRetry-After: 2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    announced.unwrap_or(body.len())
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Endpoint { url, calls, worker }
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn runtime(url: &str) -> ComponentRuntime {
    ComponentRuntime {
        template: serde_json::from_value(json!({
            "id": "owned-response-privacy",
            "name": "Owned response privacy",
            "version": "1.0.0",
            "type": "text_translation",
            "request": { "method": "POST", "url": url, "body_type": "none" },
            "response": { "translated_text_path": "translated", "error_path": "error.message" }
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

fn private_error(error: &anyhow::Error) {
    let display = format!("{error:#}");
    let debug = format!("{error:?}");
    assert!(
        !display.contains(PRIVATE),
        "error display must not echo provider content"
    );
    assert!(
        !debug.contains(PRIVATE),
        "error debug must not echo provider content"
    );
    assert!(
        !display.contains(QUERY),
        "error source chain must not echo a query credential"
    );
    assert!(
        !debug.contains(QUERY),
        "error debug must not echo a query credential"
    );
}

async fn invoke_json_owned(status: u16, body: &str) -> (anyhow::Error, Endpoint) {
    let endpoint = endpoint(status, body, false).await;
    let runtime = runtime(&endpoint.url);
    let error = invoke_component_api(
        &client(),
        &runtime,
        &runtime.template.request,
        &endpoint.url,
        &HashMap::new(),
        SignResult::ContextOnly(HashMap::new()),
    )
    .await
    .expect_err("the owned provider response is an error");
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    (error, endpoint)
}

#[tokio::test]
async fn json_non_2xx_error_does_not_echo_mapped_message_or_credentials() {
    let (error, _endpoint) = invoke_json_owned(
        429,
        &json!({"error":{"message":PRIVATE},"access_token":QUERY}).to_string(),
    )
    .await;
    private_error(&error);
    assert!(error.to_string().contains("429"));
    assert!(error.to_string().contains("retry_after_ms=2000"));
}

#[tokio::test]
async fn json_non_2xx_missing_error_path_does_not_echo_response_preview() {
    let (error, _endpoint) = invoke_json_owned(
        403,
        &json!({"private_translation":PRIVATE,"refresh_token":QUERY}).to_string(),
    )
    .await;
    private_error(&error);
}

#[tokio::test]
async fn non_json_error_does_not_echo_provider_body() {
    let (error, _endpoint) = invoke_json_owned(500, PRIVATE).await;
    private_error(&error);
}

#[tokio::test]
async fn logical_200_error_does_not_echo_provider_message() {
    let (error, _endpoint) =
        invoke_json_owned(200, &json!({"error":{"message":PRIVATE}}).to_string()).await;
    private_error(&error);
    assert!(error.to_string().contains("logical error"));
}

#[tokio::test]
async fn binary_submit_error_does_not_echo_provider_body() {
    let endpoint = endpoint(500, PRIVATE, false).await;
    let runtime = runtime(&endpoint.url);
    let error = invoke_component_api_binary(
        &client(),
        &runtime,
        &runtime.template.request,
        &endpoint.url,
        &HashMap::new(),
        SignResult::ContextOnly(HashMap::new()),
    )
    .await
    .expect_err("owned binary submit must fail");
    private_error(&error);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn binary_download_error_does_not_echo_provider_body() {
    let endpoint = endpoint(500, PRIVATE, false).await;
    let runtime = runtime(&endpoint.url);
    let spec = serde_json::from_value(json!({
        "method":"GET", "url":endpoint.url, "headers":null, "body":null, "body_type":"none"
    }))
    .unwrap();
    let error = call_component_request_binary(&client(), &runtime, &spec, &mut HashMap::new())
        .await
        .expect_err("owned download must fail");
    private_error(&error);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

async fn source_upload_error(status: u16) -> (anyhow::Error, Endpoint) {
    let endpoint = endpoint(status, PRIVATE, false).await;
    let runtime = runtime(&endpoint.url);
    let spec = serde_json::from_value(json!({
        "method":"PUT", "url":endpoint.url, "body_type":"binary_source",
        "extract":{"id":"id"}
    }))
    .unwrap();
    let verified = SourceAsset {
        bytes: b"owned-source".to_vec(),
        content_type: "text/plain".into(),
        filename: "owned.txt".into(),
    };
    let error = upload_source_asset_to_vendor(
        &client(),
        &runtime,
        &spec,
        &mut HashMap::new(),
        "unused-owned-source",
        Some(&verified),
    )
    .await
    .expect_err("owned upload error must be retained");
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
    (error, endpoint)
}

#[tokio::test]
async fn source_upload_failure_does_not_echo_body() {
    let (error, _endpoint) = source_upload_error(500).await;
    private_error(&error);
}

#[tokio::test]
async fn source_upload_malformed_success_receipt_does_not_echo_body() {
    let (error, _endpoint) = source_upload_error(200).await;
    private_error(&error);
}

#[test]
fn prepare_extract_missing_mapping_does_not_echo_secret_response() {
    let mut ctx = HashMap::from([("computed.original".into(), "kept".into())]);
    let error = apply_json_extract_map(
        &HashMap::from([("id".into(), "missing".into())]),
        &json!({"private_translation":PRIVATE,"temporary_credentials":QUERY}),
        &mut ctx,
        "owned-response-privacy",
        "prepare.extract",
    )
    .expect_err("missing extraction must fail");
    private_error(&error);
    assert_eq!(ctx["computed.original"], "kept");
}

#[tokio::test]
async fn translated_path_failure_does_not_echo_paid_submit_response() {
    let endpoint = endpoint(
        200,
        &json!({"private_translation":PRIVATE,"access_token":QUERY}).to_string(),
        false,
    )
    .await;
    let runtime = runtime(&endpoint.url);
    let error = translate_text_via_component(&client(), &runtime, "owned-input", "en", "zh")
        .await
        .expect_err("missing translated path must fail");
    private_error(&error);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transport_failure_source_chain_does_not_echo_query_credential() {
    let endpoint = endpoint(200, "", true).await;
    let runtime = runtime(&endpoint.url);
    let error = invoke_component_api(
        &client(),
        &runtime,
        &runtime.template.request,
        &endpoint.url,
        &HashMap::new(),
        SignResult::ContextOnly(HashMap::new()),
    )
    .await
    .expect_err("closed owned connection must fail");
    private_error(&error);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn source_fetch_transport_chain_does_not_echo_query_credential() {
    let endpoint = endpoint(200, "", true).await;
    let error = fetch_source_asset(&client(), &endpoint.url, 32)
        .await
        .expect_err("closed owned source connection must fail");
    private_error(&error);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn source_status_diagnostic_omits_all_query_values_and_body() {
    let endpoint = endpoint(500, PRIVATE, false).await;
    let error = fetch_source_asset(&client(), &format!("{}&q={PRIVATE}", endpoint.url), 32)
        .await
        .expect_err("owned source status must fail");
    private_error(&error);
    assert!(error.to_string().contains("500"));
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn binary_and_upload_transport_chains_do_not_echo_query_credentials() {
    for stage in ["binary_submit", "binary_download", "source_upload"] {
        let endpoint = endpoint(200, "", true).await;
        let runtime = runtime(&endpoint.url);
        let client = client();
        let error = match stage {
            "binary_submit" => invoke_component_api_binary(
                &client,
                &runtime,
                &runtime.template.request,
                &endpoint.url,
                &HashMap::new(),
                SignResult::ContextOnly(HashMap::new()),
            )
            .await
            .unwrap_err(),
            "binary_download" => {
                let spec = serde_json::from_value(json!({
                    "method":"GET","url":endpoint.url,"body_type":"none"
                }))
                .unwrap();
                call_component_request_binary(&client, &runtime, &spec, &mut HashMap::new())
                    .await
                    .unwrap_err()
            }
            _ => {
                let spec = serde_json::from_value(json!({
                    "method":"PUT","url":endpoint.url,"body_type":"binary_source"
                }))
                .unwrap();
                let verified = SourceAsset {
                    bytes: b"owned-source".to_vec(),
                    content_type: "text/plain".into(),
                    filename: "owned.txt".into(),
                };
                upload_source_asset_to_vendor(
                    &client,
                    &runtime,
                    &spec,
                    &mut HashMap::new(),
                    "unused-owned-source",
                    Some(&verified),
                )
                .await
                .unwrap_err()
            }
        };
        private_error(&error);
        assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1, "{stage}");
    }
}

#[tokio::test]
async fn incomplete_success_bodies_fail_without_query_or_body_in_error_chains() {
    for stage in [
        "source",
        "json",
        "binary_submit",
        "binary_download",
        "source_upload",
    ] {
        let endpoint = endpoint_framed(200, PRIVATE, false, Some(PRIVATE.len() + 8)).await;
        let runtime = runtime(&endpoint.url);
        let client = client();
        let error = match stage {
            "source" => fetch_source_asset(&client, &endpoint.url, 128)
                .await
                .unwrap_err(),
            "json" => invoke_component_api(
                &client,
                &runtime,
                &runtime.template.request,
                &endpoint.url,
                &HashMap::new(),
                SignResult::ContextOnly(HashMap::new()),
            )
            .await
            .unwrap_err(),
            "binary_submit" => invoke_component_api_binary(
                &client,
                &runtime,
                &runtime.template.request,
                &endpoint.url,
                &HashMap::new(),
                SignResult::ContextOnly(HashMap::new()),
            )
            .await
            .unwrap_err(),
            "binary_download" => {
                let spec = serde_json::from_value(json!({
                    "method":"GET","url":endpoint.url,"body_type":"none"
                }))
                .unwrap();
                call_component_request_binary(&client, &runtime, &spec, &mut HashMap::new())
                    .await
                    .unwrap_err()
            }
            _ => {
                let spec = serde_json::from_value(json!({
                    "method":"PUT","url":endpoint.url,"body_type":"binary_source"
                }))
                .unwrap();
                let verified = SourceAsset {
                    bytes: b"owned-source".to_vec(),
                    content_type: "text/plain".into(),
                    filename: "owned.txt".into(),
                };
                upload_source_asset_to_vendor(
                    &client,
                    &runtime,
                    &spec,
                    &mut HashMap::new(),
                    "unused-owned-source",
                    Some(&verified),
                )
                .await
                .unwrap_err()
            }
        };
        private_error(&error);
        assert_eq!(endpoint.calls.load(Ordering::SeqCst), 1, "{stage}");
    }
}

#[tokio::test]
async fn successful_response_still_preserves_real_result_and_binary_bytes() {
    let endpoint = endpoint(200, r#"{"translated":"owned-success"}"#, false).await;
    let runtime = runtime(&endpoint.url);
    let result = translate_text_via_component(&client(), &runtime, "owned-input", "en", "zh")
        .await
        .unwrap();
    assert_eq!(result, "owned-success");
    let binary = invoke_component_api_binary(
        &client(),
        &runtime,
        &runtime.template.request,
        &endpoint.url,
        &HashMap::new(),
        SignResult::ContextOnly(HashMap::new()),
    )
    .await
    .unwrap();
    assert_eq!(binary.bytes, br#"{"translated":"owned-success"}"#);
    assert_eq!(endpoint.calls.load(Ordering::SeqCst), 2);
}
