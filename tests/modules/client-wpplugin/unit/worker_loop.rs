//! Integration tests: local worker loop (claim -> translate -> callback).
//!
//! `worker.rs` exposes only `pub(crate)` entries (`run_worker_cli`,
//! `build_worker_config`); the publicly drivable entry is the WebUI route
//! `POST /api/worker/run-once`, which executes one full local-worker
//! iteration: reload bindings -> discover site-relations -> rules ->
//! paginated content + claim lease -> component translation -> callback
//! write-back -> job/item status persistence. These tests drive that loop
//! through `WebUiTestHarness` against a self-contained mock WP backend.
//!
//! Run:    cargo test --manifest-path client-wpplugin/source/Cargo.toml --test worker_loop

use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, SignerMut};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wptsall_client::test_support::{
    decode_wp_transport_request, sign_wp_plaintext_response, WebUiJsonResponse, WebUiTestHarness,
};

const ROUTE_SECRET: &str = "route-worker";
const SESSION_TOKEN: &str = "sess-test";
const WP_CLIENT_TOKEN: &str = "wp-client-token-test";
const MOCK_SIGNING_KEY_ID: &str = "kid-mock";

#[derive(Default)]
struct BackendState {
    /// Decoded translation callback payloads received from the worker.
    callbacks: Vec<Value>,
    /// Idempotency-Key headers observed on each callback delivery.
    callback_keys: Vec<String>,
    /// Claim request bodies (decoded) in arrival order.
    claim_bodies: Vec<Value>,
    /// Raw content items served by /content.
    items: Vec<Value>,
}

struct BackendHandle {
    base_url: String,
    api_base_url: String,
    state: Arc<Mutex<BackendState>>,
    shutdown: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl BackendHandle {
    async fn start(items: Vec<Value>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{}", addr);
        let api_base_url = format!("{}/wp-json/wptsall/v2/client", base_url);
        let state = Arc::new(Mutex::new(BackendState {
            items,
            ..BackendState::default()
        }));
        let shutdown = CancellationToken::new();
        let join_state = Arc::clone(&state);
        let join_shutdown = shutdown.clone();
        let join_base_url = base_url.clone();
        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = join_shutdown.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let state = Arc::clone(&join_state);
                        let base_url = join_base_url.clone();
                        tokio::spawn(async move {
                            let _ = handle_backend_connection(socket, state, &base_url).await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            base_url,
            api_base_url,
            state,
            shutdown,
            join,
        })
    }

    async fn callbacks(&self) -> Vec<Value> {
        self.state.lock().await.callbacks.clone()
    }

    async fn callback_keys(&self) -> Vec<String> {
        self.state.lock().await.callback_keys.clone()
    }

    async fn claim_bodies(&self) -> Vec<Value> {
        self.state.lock().await.claim_bodies.clone()
    }

    async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.join.await;
    }
}

fn mock_signing_material() -> &'static (rsa::RsaPrivateKey, String) {
    static MATERIAL: OnceLock<(rsa::RsaPrivateKey, String)> = OnceLock::new();
    MATERIAL.get_or_init(|| {
        let mut rng = rand::thread_rng();
        let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("generate RSA keypair");
        let public_key = rsa::RsaPublicKey::from(&private_key);
        let pem =
            rsa::pkcs8::EncodePublicKey::to_public_key_pem(&public_key, rsa::pkcs8::LineEnding::LF)
                .expect("encode public key pem");
        (private_key, pem)
    })
}

fn sign_mock_component_payload(template_json: &Value) -> String {
    let (private_key, _) = mock_signing_material();
    let payload = serde_json::to_vec(template_json).expect("serialize mock component payload");
    let mut signing_key = SigningKey::<sha2::Sha256>::new_unprefixed(private_key.clone());
    let signature = signing_key.sign(&payload);
    BASE64_STANDARD.encode(signature.to_bytes())
}

fn signed_component_download(component_id: &str, template_json: Value) -> Value {
    json!({
        "component_id": component_id,
        "version": "1.0.0",
        "owner_type": "official",
        "template_json": template_json,
        "encrypted_payload": null,
        "nonce": null,
        "algorithm": null,
        "kdf_version": null,
        "signature": sign_mock_component_payload(&template_json),
        "signing_key_id": MOCK_SIGNING_KEY_ID
    })
}

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn assert_ok(response: &WebUiJsonResponse) -> anyhow::Result<()> {
    if !response.status_line.starts_with("HTTP/1.1 200") {
        return Err(anyhow!(
            "expected 200 response, got {} with body {}",
            response.status_line,
            response.raw_body
        ));
    }
    if response.body["success"] != json!(true) {
        return Err(anyhow!("request failed: {}", response.raw_body));
    }
    Ok(())
}

fn post_item(object_id: i64, title: &str) -> Value {
    json!({
        "object_type": "post",
        "subtype": "post",
        "object_id": object_id,
        "complete_data": {
            "post_title": title
        }
    })
}

async fn setup_harness(backend: &BackendHandle, harness: &WebUiTestHarness) -> anyhow::Result<()> {
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-text",
                "name": "Local Mock Text",
                "template_id": "official-mock-text-v1",
                "template_json": text_template(&backend.base_url),
                "vendor_id": "mock-text-vendor",
                "vendor_name": "Mock Text Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("text", "local-mock-text")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;
    Ok(())
}

/// Walk a run-once cycle and return the (single) discovery job's item rows.
async fn run_once_and_get_items(
    harness: &WebUiTestHarness,
    expected_processed: i64,
    expected_succeeded: i64,
) -> anyhow::Result<Vec<Value>> {
    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(
        run_once.body["data"]["tasks_processed"],
        json!(expected_processed),
        "unexpected tasks_processed: {}",
        run_once.raw_body
    );
    assert_eq!(
        run_once.body["data"]["tasks_succeeded"],
        json!(expected_succeeded)
    );
    assert_eq!(run_once.body["data"]["tasks_failed"], json!(0));

    let jobs = harness.list_jobs().await?;
    assert_ok(&jobs)?;
    let job_items = jobs.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("jobs.items should be an array"))?;
    assert_eq!(job_items.len(), 1, "expected exactly one discovery job");
    let job_id = job_items[0]["id"]
        .as_i64()
        .ok_or_else(|| anyhow!("job id missing"))?;

    let items = harness.list_job_items(job_id).await?;
    assert_ok(&items)?;
    Ok(items.body["data"]["items"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("job items should be an array"))?)
}

#[tokio::test]
async fn worker_single_run_executes_claim_translate_callback_cycle() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start(vec![post_item(501, "Hello from mock")]).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let item_rows = run_once_and_get_items(&harness, 1, 1).await?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    assert_eq!(item_rows[0]["status"], json!("done"));
    assert_eq!(
        item_rows[0]["wp_object_id"],
        json!(501),
        "job item should reference the discovered object: {}",
        item_rows[0]
    );
    assert_eq!(item_rows[0]["relation_id"], json!(101));

    // The worker must have claimed the item on WP before translating it.
    let claims = backend.claim_bodies().await;
    assert_eq!(claims.len(), 1, "expected exactly one claim request");
    let claim_items = claims[0]["items"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("claim request should carry items"))?;
    assert_eq!(claim_items.len(), 1);
    assert_eq!(claim_items[0]["object_id"], json!(501));
    assert_eq!(claim_items[0]["post_type"], json!("post"));

    // The translated result must be written back via callback with a marker
    // translation from the mock component and a non-empty Idempotency-Key.
    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_eq!(
        callbacks[0]["translated_fields"]["post_title"],
        json!("[zh_CN] Hello from mock"),
        "callback should carry the component translation: {}",
        callbacks[0]
    );
    assert_eq!(callbacks[0]["relation_id"], json!(101));
    assert_eq!(callbacks[0]["object_id"], json!(501));
    assert_eq!(
        callbacks[0]["source_revision"], "mock-source-revision",
        "callback must echo the WP-supplied source_revision: {}",
        callbacks[0]
    );
    let keys = backend.callback_keys().await;
    assert_eq!(keys.len(), 1);
    assert!(
        !keys[0].trim().is_empty(),
        "callback delivery must carry a non-empty Idempotency-Key header"
    );

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn worker_processes_all_claimed_items_in_one_run() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start(vec![
        post_item(501, "First post"),
        post_item(502, "Second post"),
        post_item(503, "Third post"),
    ])
    .await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let item_rows = run_once_and_get_items(&harness, 3, 3).await?;
    assert_eq!(item_rows.len(), 3, "expected three translated items");
    for row in &item_rows {
        assert_eq!(row["status"], json!("done"), "item not done: {}", row);
    }

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 3, "expected one callback per item");
    let titles: Vec<&str> = callbacks
        .iter()
        .map(|cb| cb["translated_fields"]["post_title"].as_str().unwrap_or(""))
        .collect();
    for expected in [
        "[zh_CN] First post",
        "[zh_CN] Second post",
        "[zh_CN] Third post",
    ] {
        assert!(
            titles.contains(&expected),
            "missing translated title {} in {:?}",
            expected,
            titles
        );
    }

    let keys = backend.callback_keys().await;
    assert_eq!(keys.len(), 3);
    let unique: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
    assert_eq!(unique.len(), 3, "each item needs its own Idempotency-Key");

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn worker_second_run_without_new_work_is_a_noop() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start(vec![post_item(501, "Hello from mock")]).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let first_rows = run_once_and_get_items(&harness, 1, 1).await?;
    assert_eq!(first_rows.len(), 1);
    let callbacks_after_first = backend.callbacks().await.len();
    let claims_after_first = backend.claim_bodies().await.len();

    // Second iteration: the item has a materialized success record and no
    // pending callback, so it is deduped — nothing succeeds, nothing fails,
    // the run reports the dedup/noop break reason, and nothing is
    // redelivered to WP. (The deduped item still counts as "processed"
    // because the loop did inspect and classify it.)
    let run_twice = harness.run_worker_once().await?;
    assert_ok(&run_twice)?;
    assert_eq!(
        run_twice.body["data"]["tasks_succeeded"],
        json!(0),
        "second run should not succeed any task: {}",
        run_twice.raw_body
    );
    assert_eq!(run_twice.body["data"]["tasks_failed"], json!(0));
    assert_eq!(
        run_twice.body["data"]["break_reason"],
        json!("dedup_or_noop"),
        "second run should end via the dedup/noop path: {}",
        run_twice.raw_body
    );

    assert_eq!(
        backend.callbacks().await.len(),
        callbacks_after_first,
        "second run must not redeliver callbacks"
    );
    // The claim endpoint is still hit during discovery, but every claimed item
    // is filtered out by the client-side dedup before translation; nothing
    // changes on the WP side.
    assert!(
        backend.claim_bodies().await.len() >= claims_after_first,
        "claim requests should never regress"
    );

    backend.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Mock WP backend (harness conventions copied from
// component_template_mock_task_flow.rs)
// ---------------------------------------------------------------------------

async fn handle_backend_connection(
    mut socket: TcpStream,
    state: Arc<Mutex<BackendState>>,
    base_url: &str,
) -> anyhow::Result<()> {
    let request = read_http_request(&mut socket).await?;
    let (path, query) = split_target(&request.target);

    match (request.method.as_str(), path.as_str()) {
        ("GET", "/api/v1/client/domains") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": {
                        "items": [{
                            "api_base_url": format!("{}/wp-json/wptsall/v2/client", base_url),
                            "license_status": "active",
                            "route_secret": ROUTE_SECRET,
                            "plan_tier": "pro",
                            "max_relations": 5
                        }]
                    }
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/signing-public-key") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": {
                        "public_key_pem": mock_signing_material().1,
                        "key_id": MOCK_SIGNING_KEY_ID
                    }
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/components") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": {
                        "items": [component_catalog_item()],
                        "page": 1,
                        "per_page": 200,
                        "total": 1,
                        "total_pages": 1
                    }
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/components/official-mock-text-v1") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": { "component": component_catalog_item() }
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-text-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download(
                        "official-mock-text-v1",
                        text_template(base_url)
                    )
                }),
            )
            .await?;
        }
        ("GET", "/wp-json/wptsall/v2/route-worker/client/site-relations") => {
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "relations": [{
                        "id": 101,
                        "source_lang": "en_US",
                        "target_lang": "zh_CN",
                        "sync_mode": "manual",
                        "target_site_type": "virtual",
                        "models": [{
                            "model_id": 1,
                            "plugin_slug": "",
                            "plugin_name": "",
                            "post_types": ["post"],
                            "taxonomies": []
                        }]
                    }]
                }),
            )
            .await?;
        }
        ("GET", "/wp-json/wptsall/v2/route-worker/client/rules") => {
            let params = parse_query(query.as_deref().unwrap_or_default());
            if params.get("relation_id").map(String::as_str) != Some("101") {
                return Err(anyhow!(
                    "unexpected relation_id in rules request: {:?}",
                    params
                ));
            }
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "rules": [{
                        "id": 201,
                        "model_id": 1,
                        "name": "Mock Worker Rule",
                        "data_type": "post_type",
                        "object_name": "post",
                        "translate_fields": ["post_title"],
                        "field_capabilities": {},
                        "related_taxonomies": [],
                        "field_content_formats": {
                            "post_title": "plain_text"
                        }
                    }]
                }),
            )
            .await?;
        }
        ("GET", "/wp-json/wptsall/v2/route-worker/client/content") => {
            let params = parse_query(query.as_deref().unwrap_or_default());
            let data_type = params.get("data_type").cloned().unwrap_or_default();
            let page = params
                .get("page")
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(1);
            let items = state.lock().await.items.clone();
            if data_type != "post" || page != 1 {
                write_wp_json_response(
                    &mut socket,
                    "200 OK",
                    WP_CLIENT_TOKEN,
                    &json!({ "items": [], "total": items.len() as i64, "page": page, "per_page": 20 }),
                )
                .await?;
                return Ok(());
            }
            let mut page_items = items;
            for item in &mut page_items {
                if let Some(complete_data) =
                    item.get_mut("complete_data").and_then(Value::as_object_mut)
                {
                    complete_data
                        .entry("__wptsall_job_snapshot")
                        .or_insert_with(|| {
                            json!({
                                "source_revision": "mock-source-revision",
                                "policy_version": "mock-policy-version"
                            })
                        });
                }
            }
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "items": page_items,
                    "total": page_items.len() as i64,
                    "page": 1,
                    "per_page": 20
                }),
            )
            .await?;
        }
        ("POST", "/wp-json/wptsall/v2/route-worker/client/content/claim") => {
            let body_json = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            let items = body_json["items"].as_array().cloned().unwrap_or_default();
            {
                let mut guard = state.lock().await;
                guard.claim_bodies.push(body_json);
            }
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "claimed_count": items.len(),
                    "claimed_items": items
                }),
            )
            .await?;
        }
        ("POST", "/wp-json/wptsall/v2/route-worker/client/translation-callback") => {
            let idempotency_key = request
                .header("idempotency-key")
                .unwrap_or_default()
                .to_string();
            let body_json = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            {
                let mut guard = state.lock().await;
                guard.callbacks.push(body_json);
                guard.callback_keys.push(idempotency_key);
            }
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "success": true,
                    "queued": false,
                    "result_id": 1,
                    "protocol": "v2",
                    "result_status": "synced"
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/text") => {
            let body_json = request.json_body()?;
            let text = body_json["text"].as_str().unwrap_or_default();
            let target_lang = body_json["target_lang"].as_str().unwrap_or("zh_CN");
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_text": format!("[{}] {}", target_lang, text)
                }),
            )
            .await?;
        }
        // Identity Contract v1.1 §5 (C-1): the domain loop fail-closes on
        // binding identity before dispatch, so run-once first pings the
        // mock WP backend. Serve the ATS ping envelope (signed like every
        // other WP response here).
        ("GET", "/wp-json/wptsall/v2/route-worker/client/ping") => {
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "success": true,
                    "data": {
                        "plugin_identity": "wpmmcc_ats",
                        "plugin_version": "1.0.0",
                        "site_platform": "wp"
                    }
                }),
            )
            .await?;
        }
        _ => {
            return Err(anyhow!(
                "unhandled backend request: {} {}",
                request.method,
                request.target
            ));
        }
    }

    Ok(())
}

fn component_catalog_item() -> Value {
    json!({
        "id": "official-mock-text-v1",
        "name": "Mock Text Template",
        "owner_type": "official",
        "version": "1.0.0",
        "type": "text_translation",
        "supported_types": ["text"],
        "supported_business_lines": ["post_content"],
        "supported_content_formats": ["plain_text", "rich_html"],
        "supported_formats": [],
        "vendor_id": "mock-text-vendor",
        "api_version": "1.0.0",
        "status": "active",
        "updated_at": "2026-03-20T00:00:00Z",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": "text",
            "input_mode": "text",
            "workflow_mode": "sync",
            "output_mode": "translated_text",
            "stages": ["request"],
            "request_body_type": "json",
            "submit_response_type": "json",
            "result_transport": "inline_json"
        }
    })
}

fn text_template(base_url: &str) -> Value {
    json!({
        "id": "official-mock-text-v1",
        "name": "Mock Text Template",
        "version": "1.0.0",
        "type": "text_translation",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": "text",
            "input_mode": "text",
            "workflow_mode": "sync",
            "output_mode": "translated_text",
            "stages": ["request"],
            "request_body_type": "json",
            "submit_response_type": "json",
            "result_transport": "inline_json"
        },
        "auth": { "fields": [] },
        "request": {
            "method": "POST",
            "url": format!("{}/mock/vendor/text", base_url),
            "headers": {
                "Content-Type": "application/json"
            },
            "body": {
                "text": "{{input.text}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            },
            "body_type": "json"
        },
        "response": {
            "translated_text_path": "translated_text"
        },
        "constraints": {
            "supported_content_formats": ["plain_text", "rich_html"],
            "max_input_chars": 5000
        }
    })
}

struct ParsedHttpRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl ParsedHttpRequest {
    fn json_body(&self) -> anyhow::Result<Value> {
        if self.body.is_empty() {
            Ok(json!({}))
        } else {
            serde_json::from_slice(&self.body)
                .with_context(|| format!("invalid json body for {}", self.target))
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.trim().to_ascii_lowercase())
            .map(String::as_str)
    }
}

async fn read_http_request(socket: &mut TcpStream) -> anyhow::Result<ParsedHttpRequest> {
    let mut buf = Vec::new();
    let mut header_end = None;
    let mut content_length = 0usize;

    loop {
        let mut chunk = [0u8; 4096];
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if header_end.is_none() {
            if let Some(idx) = find_bytes(&buf, b"\r\n\r\n") {
                header_end = Some(idx + 4);
                let header_text = String::from_utf8_lossy(&buf[..idx]);
                for line in header_text.lines().skip(1) {
                    if let Some((name, value)) = line.split_once(':') {
                        if name.trim().eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse::<usize>().unwrap_or(0);
                        }
                    }
                }
            }
        }
        if let Some(end) = header_end {
            if buf.len() >= end + content_length {
                break;
            }
        }
    }

    let header_end = header_end.ok_or_else(|| anyhow!("incomplete http headers"))?;
    let header_text = String::from_utf8_lossy(&buf[..header_end - 4]);
    let mut lines = header_text.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let body = buf[header_end..].to_vec();

    Ok(ParsedHttpRequest {
        method,
        target,
        headers,
        body,
    })
}

fn split_target(target: &str) -> (String, Option<String>) {
    target
        .split_once('?')
        .map(|(path, query)| (path.to_string(), Some(query.to_string())))
        .unwrap_or_else(|| (target.to_string(), None))
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect::<HashMap<String, String>>()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn write_json_response(
    socket: &mut TcpStream,
    status: &str,
    body: &Value,
) -> anyhow::Result<()> {
    let body_bytes = serde_json::to_vec(body)?;
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        body_bytes.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.write_all(&body_bytes).await?;
    Ok(())
}

async fn write_wp_json_response(
    socket: &mut TcpStream,
    status: &str,
    token: &str,
    body: &Value,
) -> anyhow::Result<()> {
    let body_bytes = serde_json::to_vec(body)?;
    let signature = sign_wp_plaintext_response(token, &body_bytes);
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        signature,
        body_bytes.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.write_all(&body_bytes).await?;
    Ok(())
}
