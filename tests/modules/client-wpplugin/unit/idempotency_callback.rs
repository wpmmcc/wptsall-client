//! Integration tests: translation callback Idempotency-Key semantics.
//!
//! The client derives a deterministic idempotency key per content item
//! (`discovery-{domain}-{relation}-{object_type}-{object_id}-{snapshot}`),
//! upserts a pending-callback row keyed on it, and sends the key as the
//! `Idempotency-Key` header on every `/translation-callback` delivery.
//! The WP side replays the cached ack for a duplicate key with the same
//! semantic payload and refuses to double-write.
//!
//! These tests drive the real worker loop through `WebUiTestHarness` against
//! a self-contained mock WP backend that models the plugin's idempotency
//! behavior (`trait-client-tasks-rest-controller-idempotency.php`).
//!
//! Run:    cargo test --manifest-path client-wpplugin/source/Cargo.toml --test idempotency_callback

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

const ROUTE_SECRET: &str = "route-idem";
const SESSION_TOKEN: &str = "sess-test";
const WP_CLIENT_TOKEN: &str = "wp-client-token-test";
const MOCK_SIGNING_KEY_ID: &str = "kid-mock";

/// One delivered callback attempt, as observed by the mock WP.
#[derive(Clone)]
struct CallbackDelivery {
    idempotency_key: String,
    fingerprint: String,
    #[allow(dead_code)]
    payload: Value,
}

#[derive(Default)]
struct BackendState {
    /// Every synthetic vendor request, to distinguish replay from retranslation.
    vendor_requests: Vec<String>,
    /// Every delivery attempt (success or failure) in order.
    deliveries: Vec<CallbackDelivery>,
    /// Unique keys that actually caused a write on the WP side.
    writes: Vec<String>,
    /// key -> (fingerprint, cached ack) — the WP idempotency record pool.
    ack_cache: HashMap<String, (String, Value)>,
    /// How many duplicate deliveries were answered from the cache.
    idempotent_replays: usize,
    /// Injected failure knob: fail every callback delivery while true.
    fail_callbacks: bool,
    /// Serve content items with needs_resync=true (forces client reprocessing).
    serve_needs_resync: bool,
    /// Raw content items served by /content (without the job snapshot).
    items: Vec<Value>,
    /// per_page reported by /content (controls client pagination).
    per_page: i64,
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

    async fn set_fail_callbacks(&self, value: bool) {
        self.state.lock().await.fail_callbacks = value;
    }

    async fn set_serve_needs_resync(&self, value: bool) {
        self.state.lock().await.serve_needs_resync = value;
    }

    async fn deliveries(&self) -> Vec<CallbackDelivery> {
        self.state.lock().await.deliveries.clone()
    }

    async fn writes(&self) -> Vec<String> {
        self.state.lock().await.writes.clone()
    }

    async fn idempotent_replays(&self) -> usize {
        self.state.lock().await.idempotent_replays
    }

    async fn vendor_requests(&self) -> Vec<String> {
        self.state.lock().await.vendor_requests.clone()
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

/// Mirrors the WP plugin fingerprint scope (`task_id`/`status`/`result`/`meta`):
/// only the semantic result fields, never transport metadata like
/// `attempt_id` or `execution_time_ms`.
fn callback_fingerprint(payload: &Value) -> String {
    use sha2::{Digest, Sha256};
    let semantic = json!({
        "client_task_id": payload["client_task_id"],
        "relation_id": payload["relation_id"],
        "object_type": payload["object_type"],
        "subtype": payload["post_type"],
        "object_id": payload["object_id"],
        "translated_fields": payload["translated_fields"],
        "translated_meta": payload["translated_meta"],
        "media_mappings": payload["media_mappings"],
    });
    let bytes = serde_json::to_vec(&semantic).expect("serialize semantic fingerprint");
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
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

/// Configure the harness with a local text component bound to the text lane,
/// plus the domain binding against the mock WP.
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

#[tokio::test]
async fn distinct_items_use_distinct_idempotency_keys_and_write_once() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start(vec![
        post_item(501, "Hello from mock"),
        post_item(502, "Second post"),
    ])
    .await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(2));

    let deliveries = backend.deliveries().await;
    assert_eq!(deliveries.len(), 2, "expected one callback per item");
    let keys: Vec<&str> = deliveries
        .iter()
        .map(|d| d.idempotency_key.as_str())
        .collect();
    assert!(
        keys[0] != keys[1],
        "distinct content items must use distinct Idempotency-Keys: {:?}",
        keys
    );
    for key in &keys {
        assert!(
            key.starts_with("discovery-"),
            "idempotency key should carry the discovery prefix: {}",
            key
        );
    }
    assert_eq!(backend.writes().await.len(), 2);

    // A second worker run over the same content is a client-side no-op: the
    // items already have materialized success records with no pending
    // callbacks, so nothing is redelivered and WP does not see duplicates.
    let run_twice = harness.run_worker_once().await?;
    assert_ok(&run_twice)?;
    assert_eq!(run_twice.body["data"]["tasks_succeeded"], json!(0));
    assert_eq!(
        backend.deliveries().await.len(),
        2,
        "second run must not redeliver already-synced callbacks"
    );
    assert_eq!(
        backend.writes().await.len(),
        2,
        "second run must not produce duplicate writes"
    );

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn failed_callback_redelivers_same_key_and_writes_exactly_once() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start(vec![post_item(501, "Hello from mock")]).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    // Run 1: WP rejects every delivery. The client retries in-run, then leaves
    // the callback pending; nothing is written on the WP side.
    backend.set_fail_callbacks(true).await;
    let run_one = harness.run_worker_once().await?;
    assert_ok(&run_one)?;
    let failed_deliveries = backend.deliveries().await;
    assert!(
        !failed_deliveries.is_empty(),
        "run one should have attempted at least one callback delivery"
    );
    assert_eq!(backend.writes().await.len(), 0);
    let key = failed_deliveries[0].idempotency_key.clone();
    let stored_task_id: String = rusqlite::Connection::open_with_flags(
        &harness.db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?
    .query_row(
        "SELECT client_task_id FROM translation_items WHERE wp_object_id=501",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        stored_task_id,
        failed_deliveries[0].payload["client_task_id"]
            .as_str()
            .unwrap(),
        "discovery must persist the saved callback's task identity, not an empty placeholder"
    );
    for delivery in &failed_deliveries {
        assert_eq!(
            delivery.idempotency_key, key,
            "every retry attempt must reuse the same Idempotency-Key"
        );
    }

    // Run 2: WP recovers. The client redelivers with the same deterministic
    // key; the WP-side write happens exactly once despite N delivery attempts.
    backend.set_fail_callbacks(false).await;
    let run_two = harness.run_worker_once().await?;
    assert_ok(&run_two)?;
    assert_eq!(
        backend.writes().await,
        vec![key.clone()],
        "WP must materialize one write, not two"
    );
    assert_eq!(
        backend.vendor_requests().await,
        vec!["Hello from mock".to_string()],
        "resuming an acknowledged saved callback must not translate it again"
    );

    let deliveries = backend.deliveries().await;
    assert_eq!(
        deliveries.len(),
        failed_deliveries.len() + 1,
        "the recovered item must have one delivery, not resume plus scan"
    );
    assert_eq!(run_two.body["data"]["tasks_succeeded"], json!(1));
    for delivery in &deliveries {
        assert_eq!(
            delivery.idempotency_key, key,
            "all deliveries (across runs) must share one Idempotency-Key"
        );
    }
    let writes = backend.writes().await;
    assert_eq!(
        writes,
        vec![key.clone()],
        "exactly one WP write expected after redelivery"
    );
    let run_three = harness.run_worker_once().await?;
    assert_ok(&run_three)?;
    assert_eq!(run_three.body["data"]["tasks_succeeded"], json!(0));
    assert_eq!(backend.deliveries().await.len(), deliveries.len());
    assert_eq!(backend.vendor_requests().await.len(), 1);

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn needs_resync_redelivery_same_key_is_deduped_by_wp() -> anyhow::Result<()> {
    let _test_lock = test_lock().lock().await;
    let backend = BackendHandle::start(vec![post_item(501, "Hello from mock")]).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    setup_harness(&backend, &harness).await?;

    // Run 1: normal delivery, one write, key K.
    let run_one = harness.run_worker_once().await?;
    assert_ok(&run_one)?;
    assert_eq!(run_one.body["data"]["tasks_succeeded"], json!(1));
    let first = backend
        .deliveries()
        .await
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("expected a first callback delivery"))?;
    assert_eq!(backend.writes().await.len(), 1);

    // Run 2: WP re-lists the same content with needs_resync=true (e.g. an
    // editor forced a resync). The client bypasses its own dedup and
    // re-delivers, but the content is identical, so the deterministic key and
    // the semantic fingerprint match run one. WP replays the cached ack and
    // does NOT write a second time.
    backend.set_serve_needs_resync(true).await;
    let run_two = harness.run_worker_once().await?;
    assert_ok(&run_two)?;
    assert_eq!(run_two.body["data"]["tasks_succeeded"], json!(1));

    let deliveries = backend.deliveries().await;
    assert_eq!(deliveries.len(), 2, "needs_resync forces a redelivery");
    assert_eq!(
        deliveries[1].idempotency_key, first.idempotency_key,
        "identical content must regenerate the identical Idempotency-Key"
    );
    assert_eq!(
        deliveries[1].fingerprint, first.fingerprint,
        "identical content must produce an identical semantic fingerprint"
    );
    assert_eq!(
        backend.writes().await.len(),
        1,
        "duplicate delivery with the same key must not double-write on WP"
    );
    assert_eq!(
        backend.idempotent_replays().await,
        1,
        "WP should have answered the duplicate from the idempotency cache"
    );
    assert_eq!(backend.vendor_requests().await,vec!["Hello from mock".to_string()],
        "an unchanged explicit resync must redeliver the original paid result without another provider request");

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
        ("GET", "/wp-json/wptsall/v2/route-idem/client/site-relations") => {
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
        ("GET", "/wp-json/wptsall/v2/route-idem/client/rules") => {
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
                        "name": "Mock Idempotency Rule",
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
        ("GET", "/wp-json/wptsall/v2/route-idem/client/content") => {
            let params = parse_query(query.as_deref().unwrap_or_default());
            let data_type = params.get("data_type").cloned().unwrap_or_default();
            let page = params
                .get("page")
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(1);
            let (items, per_page, needs_resync) = {
                let guard = state.lock().await;
                (
                    guard.items.clone(),
                    guard.per_page,
                    guard.serve_needs_resync,
                )
            };
            if data_type != "post" {
                write_wp_json_response(
                    &mut socket,
                    "200 OK",
                    WP_CLIENT_TOKEN,
                    &json!({ "items": [], "total": 0, "page": page, "per_page": per_page }),
                )
                .await?;
                return Ok(());
            }
            let total = items.len() as i64;
            let start = ((page - 1) * per_page).max(0) as usize;
            let end = (start + per_page as usize).min(items.len());
            let mut page_items: Vec<Value> = if start < items.len() {
                items[start..end].to_vec()
            } else {
                Vec::new()
            };
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
                if needs_resync {
                    item["needs_resync"] = json!(true);
                }
            }
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
        ("POST", "/wp-json/wptsall/v2/route-idem/client/content/claim") => {
            let body_json = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            let items = body_json["items"].as_array().cloned().unwrap_or_default();
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
        ("POST", "/wp-json/wptsall/v2/route-idem/client/translation-callback") => {
            let idempotency_key = request
                .header("idempotency-key")
                .unwrap_or_default()
                .to_string();
            let payload = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            let fingerprint = callback_fingerprint(&payload);

            let mut guard = state.lock().await;
            guard.deliveries.push(CallbackDelivery {
                idempotency_key: idempotency_key.clone(),
                fingerprint: fingerprint.clone(),
                payload: payload.clone(),
            });

            if guard.fail_callbacks {
                drop(guard);
                write_wp_json_response(
                    &mut socket,
                    "500 Internal Server Error",
                    WP_CLIENT_TOKEN,
                    &json!({
                        "success": false,
                        "error": { "code": "callback_injected_failure" }
                    }),
                )
                .await?;
                return Ok(());
            }

            let cached = guard.ack_cache.get(&idempotency_key).cloned();
            if let Some((seen_fingerprint, cached_ack)) = cached {
                if seen_fingerprint == fingerprint {
                    // WP semantics: replay the cached ack, do NOT write again.
                    guard.idempotent_replays += 1;
                    let mut ack = cached_ack.clone();
                    ack["idempotent"] = json!(true);
                    drop(guard);
                    write_wp_json_response(&mut socket, "200 OK", WP_CLIENT_TOKEN, &ack).await?;
                    return Ok(());
                }
                drop(guard);
                write_wp_json_response(
                    &mut socket,
                    "409 Conflict",
                    WP_CLIENT_TOKEN,
                    &json!({
                        "success": false,
                        "error": { "code": "task_result_idempotency_conflict" }
                    }),
                )
                .await?;
                return Ok(());
            }

            guard.writes.push(idempotency_key.clone());
            let ack = json!({
                "success": true,
                "queued": false,
                "result_id": guard.writes.len(),
                "protocol": "v2",
                "result_status": "synced"
            });
            guard
                .ack_cache
                .insert(idempotency_key, (fingerprint, ack.clone()));
            drop(guard);
            write_wp_json_response(&mut socket, "200 OK", WP_CLIENT_TOKEN, &ack).await?;
        }
        ("POST", "/mock/vendor/text") => {
            let body_json = request.json_body()?;
            let text = body_json["text"].as_str().unwrap_or_default();
            let target_lang = body_json["target_lang"].as_str().unwrap_or("zh_CN");
            state.lock().await.vendor_requests.push(text.to_string());
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
        ("GET", "/wp-json/wptsall/v2/route-idem/client/ping") => {
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
