//! Integration tests: discoverer wptsall/v2 endpoint interactions.
//!
//! Covers the discovery surface of `src/task_engine/discoverer.rs`:
//!   1. `/client/site-relations` + `/client/rules` — route-secret scoped
//!      requests with relation_id-scoped rule fetches.
//!   2. `/client/content` — paginated fetching until `total`/`per_page`
//!      tightens the page bound.
//!   3. `/client/content/claim` — the 30-minute WP claim lease: only items
//!      granted by WP may be translated and written back.
//!
//! Note: the discoverer's parsing/normalization helpers
//! (`normalize_rule_data_type`, `discover_content_data_types`,
//! `build_content_claim_items`, `retain_claimed_content_items`,
//! `tighten_total_pages`, ...) are `pub(crate)`, so an external integration
//! test cannot call them directly. They are exercised here end-to-end through
//! the real worker loop (`WebUiTestHarness::run_worker_once`) against a mock
//! WP backend that records and asserts the wire interactions.
//!
//! Run:    cargo test --manifest-path client-wpplugin/source/Cargo.toml --test discoverer_endpoints

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

const ROUTE_SECRET: &str = "route-disc";
const SESSION_TOKEN: &str = "sess-test";
const WP_CLIENT_TOKEN: &str = "wp-client-token-test";
const MOCK_SIGNING_KEY_ID: &str = "kid-mock";

#[derive(Default)]
struct BackendState {
    /// Decoded translation callback payloads received from the worker.
    callbacks: Vec<Value>,
    /// Every `/rules` request's relation_id query value, in order.
    rules_relation_ids: Vec<String>,
    /// Every `/content` request's page number, in order.
    content_pages: Vec<i64>,
    /// Whether /content must serve items at all (off => empty pages).
    content_enabled: bool,
    /// per_page reported by /content responses.
    per_page: i64,
    /// Decoded claim request bodies in arrival order.
    claim_bodies: Vec<Value>,
    /// Object ids the claim endpoint grants (None => grant everything).
    claim_grant: Option<Vec<i64>>,
}

struct BackendHandle {
    base_url: String,
    api_base_url: String,
    state: Arc<Mutex<BackendState>>,
    shutdown: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl BackendHandle {
    async fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{}", addr);
        let api_base_url = format!("{}/wp-json/wptsall/v2/client", base_url);
        let state = Arc::new(Mutex::new(BackendState {
            content_enabled: true,
            per_page: 20,
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

    async fn set_content(&self, enabled: bool, per_page: i64) {
        let mut guard = self.state.lock().await;
        guard.content_enabled = enabled;
        guard.per_page = per_page;
    }

    async fn set_claim_grant(&self, grant: Option<Vec<i64>>) {
        self.state.lock().await.claim_grant = grant;
    }

    async fn callbacks(&self) -> Vec<Value> {
        self.state.lock().await.callbacks.clone()
    }

    async fn rules_relation_ids(&self) -> Vec<String> {
        self.state.lock().await.rules_relation_ids.clone()
    }

    async fn content_pages(&self) -> Vec<i64> {
        self.state.lock().await.content_pages.clone()
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

fn post_item_with_snapshot(object_id: i64, title: &str) -> Value {
    let mut item = post_item(object_id, title);
    item["complete_data"]["__wptsall_job_snapshot"] = json!({
        "source_revision": "mock-source-revision",
        "policy_version": "mock-policy-version"
    });
    item
}

#[tokio::test]
async fn site_relations_and_rules_are_scoped_to_route_secret_and_relation() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start().await?;
    backend.set_content(true, 20).await;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;

    // The mock only registers the rule / content handlers under
    // `/wp-json/wptsall/v2/{ROUTE_SECRET}/client/...`; a wrong route secret or
    // a missing relation_id would have failed the run above.
    let rules_ids = backend.rules_relation_ids().await;
    assert!(
        !rules_ids.is_empty(),
        "worker must fetch rules for each discovered relation"
    );
    for relation_id in &rules_ids {
        assert_eq!(
            relation_id, "101",
            "rules request must be scoped to the fetched relation"
        );
    }

    // The relation payload was parsed into the job/callback wire format.
    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 3, "one callback per served item");
    for callback in &callbacks {
        assert_eq!(callback["relation_id"], json!(101));
        assert_eq!(callback["source_lang"], json!("en_US"));
        assert_eq!(callback["target_lang"], json!("zh_CN"));
    }

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn content_pagination_walks_all_pages_until_total_tightens() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start().await?;
    // 3 items, but served one per page: the client must keep fetching pages
    // until its own total_pages bound (total/per_page = 3) is reached.
    backend.set_content(true, 1).await;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(3));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(3));

    let pages = backend.content_pages().await;
    assert_eq!(
        pages,
        vec![1, 2, 3],
        "client must request every page exactly once: {:?}",
        pages
    );

    let callbacks = backend.callbacks().await;
    assert_eq!(
        callbacks.len(),
        3,
        "every page's items must be written back"
    );
    let ids: Vec<i64> = callbacks
        .iter()
        .map(|cb| cb["object_id"].as_i64().unwrap_or_default())
        .collect();
    for expected in [501, 502, 503] {
        assert!(
            ids.contains(&expected),
            "object {} missing from callbacks: {:?}",
            expected,
            ids
        );
    }

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn claim_lease_only_allows_granted_items_to_be_written_back() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start().await?;
    backend.set_content(true, 20).await;
    // WP holds object 502 under a 30-minute lease by another worker; only 501
    // and 503 are granted to this client.
    backend.set_claim_grant(Some(vec![501, 503])).await;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(2));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(2));
    assert_eq!(run_once.body["data"]["tasks_failed"], json!(0));

    // The claim request covered every discovered item; the grant is WP's call.
    let claims = backend.claim_bodies().await;
    assert_eq!(claims.len(), 1, "expected one claim request per page");
    let claimed = claims[0]["items"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("claim request should carry items"))?;
    assert_eq!(
        claimed.len(),
        3,
        "all discovered items are offered for claim"
    );
    let offered: Vec<i64> = claimed
        .iter()
        .map(|item| item["object_id"].as_i64().unwrap_or_default())
        .collect();
    assert_eq!(offered, vec![501, 502, 503]);

    // Only the granted items were translated and written back.
    let callbacks = backend.callbacks().await;
    assert_eq!(
        callbacks.len(),
        2,
        "the leased item must not be written back: {:?}",
        callbacks
    );
    let ids: Vec<i64> = callbacks
        .iter()
        .map(|cb| cb["object_id"].as_i64().unwrap_or_default())
        .collect();
    assert!(ids.contains(&501));
    assert!(ids.contains(&503));
    assert!(
        !ids.contains(&502),
        "object 502 is held by another worker's 30-minute lease"
    );

    backend.shutdown().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Mock WP backend (harness conventions copied from
// component_template_mock_task_flow.rs)
// ---------------------------------------------------------------------------

/// Content items served by the mock (page slicing handled per request).
fn served_items() -> Vec<Value> {
    vec![
        post_item_with_snapshot(501, "First post"),
        post_item_with_snapshot(502, "Second post"),
        post_item_with_snapshot(503, "Third post"),
    ]
}

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
        ("GET", "/wp-json/wptsall/v2/route-disc/client/site-relations") => {
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
        ("GET", "/wp-json/wptsall/v2/route-disc/client/rules") => {
            let params = parse_query(query.as_deref().unwrap_or_default());
            let relation_id = params.get("relation_id").cloned().unwrap_or_default();
            {
                let mut guard = state.lock().await;
                guard.rules_relation_ids.push(relation_id);
            }
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "rules": [{
                        "id": 201,
                        "model_id": 1,
                        "name": "Mock Discoverer Rule",
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
        ("GET", "/wp-json/wptsall/v2/route-disc/client/content") => {
            let params = parse_query(query.as_deref().unwrap_or_default());
            let data_type = params.get("data_type").cloned().unwrap_or_default();
            let page = params
                .get("page")
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(1);
            let (enabled, per_page) = {
                let mut guard = state.lock().await;
                guard.content_pages.push(page);
                (guard.content_enabled, guard.per_page)
            };
            if data_type != "post" || !enabled {
                write_wp_json_response(
                    &mut socket,
                    "200 OK",
                    WP_CLIENT_TOKEN,
                    &json!({ "items": [], "total": 0, "page": page, "per_page": per_page }),
                )
                .await?;
                return Ok(());
            }
            let items = served_items();
            let total = items.len() as i64;
            let start = ((page - 1) * per_page).max(0) as usize;
            let end = (start + per_page as usize).min(items.len());
            let page_items: Vec<Value> = if start < items.len() {
                items[start..end].to_vec()
            } else {
                Vec::new()
            };
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "items": page_items,
                    "total": total,
                    "page": page,
                    "per_page": per_page
                }),
            )
            .await?;
        }
        ("POST", "/wp-json/wptsall/v2/route-disc/client/content/claim") => {
            let body_json = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            let items = body_json["items"].as_array().cloned().unwrap_or_default();
            let grant = state.lock().await.claim_grant.clone();
            {
                let mut guard = state.lock().await;
                guard.claim_bodies.push(body_json);
            }
            let (claimed_count, claimed_items) = match grant {
                None => (items.len(), items),
                Some(allowed) => {
                    let granted: Vec<Value> = items
                        .iter()
                        .filter(|item| {
                            allowed.contains(&item["object_id"].as_i64().unwrap_or_default())
                        })
                        .cloned()
                        .collect();
                    let count = granted.len();
                    // WP returns only the granted subset: everything else is
                    // still held under a 30-minute lease by another worker.
                    (count, granted)
                }
            };
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "claimed_count": claimed_count,
                    "claimed_items": claimed_items
                }),
            )
            .await?;
        }
        ("POST", "/wp-json/wptsall/v2/route-disc/client/translation-callback") => {
            let body_json = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            {
                let mut guard = state.lock().await;
                guard.callbacks.push(body_json);
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
        ("GET", "/wp-json/wptsall/v2/route-disc/client/ping") => {
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
