use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rsa::pkcs1v15::SigningKey;
use rsa::signature::{SignatureEncoding, SignerMut};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wptsall_client::test_support::{
    decode_wp_transport_request, read_json_file, sign_wp_plaintext_response, WebUiJsonResponse,
    WebUiTestHarness,
};

const ROUTE_SECRET: &str = "route-test";
const SESSION_TOKEN: &str = "sess-test";
const WP_CLIENT_TOKEN: &str = "wp-client-token-test";
const IMAGE_DATA_URL: &str =
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO7+eVQAAAAASUVORK5CYII=";
const AUDIO_URL_PATH: &str = "/assets/sample.mp3";
const VIDEO_URL_PATH: &str = "/assets/sample.mp4";
const DOCUMENT_URL_PATH: &str = "/assets/sample.pdf";
const WP_IMAGE_URL_PATH: &str = "/assets/wp-hero.jpg";
const MOCK_SIGNING_KEY_ID: &str = "kid-mock";

#[derive(Clone, Copy)]
enum MockScenario {
    TextImage,
    AudioVideoDocument,
    StructuredFormats,
    AuthModesTaskFlow,
    WpSourceMixed,
    TaxonomyMixed,
    LanguagePackPlugin,
}

#[derive(Default)]
struct MockBackendState {
    callbacks: Vec<Value>,
    language_pack_batches_served: usize,
    auth_hits: Vec<String>,
    oauth_token_requests: usize,
    media_uploads: usize,
    lose_media_response: bool,
}

struct MockBackendHandle {
    base_url: String,
    api_base_url: String,
    state: Arc<Mutex<MockBackendState>>,
    shutdown: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl MockBackendHandle {
    async fn start(scenario: MockScenario) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let base_url = format!("http://{}", addr);
        let api_base_url = format!("{}/wp-json/wptsall/v2/client", base_url);
        let state = Arc::new(Mutex::new(MockBackendState::default()));
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
                        let scenario = scenario;
                        tokio::spawn(async move {
                            let _ = handle_backend_connection(socket, state, &base_url, scenario).await;
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

    async fn auth_hits(&self) -> Vec<String> {
        self.state.lock().await.auth_hits.clone()
    }

    async fn oauth_token_requests(&self) -> usize {
        self.state.lock().await.oauth_token_requests
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

fn component_template_flow_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[tokio::test]
async fn mock_component_templates_support_real_worker_task_flow() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::TextImage).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

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
            .create_local_component(json!({
                "id": "local-mock-image",
                "name": "Local Mock Image",
                "template_id": "official-mock-image-v1",
                "template_json": image_template(&backend.base_url),
                "vendor_id": "mock-image-vendor",
                "vendor_name": "Mock Image Vendor",
                "kind": "image",
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
            .upsert_task_type_binding("image", "local-mock-image")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert!(
        component_ids.contains(&json!("local-mock-text")),
        "text component should be recorded in component trace: {}",
        item
    );
    assert!(
        component_ids.contains(&json!("local-mock-image")),
        "image component should be recorded in component trace: {}",
        item
    );

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_eq!(
        translated_envelope["payload"]["translated_fields"]["post_title"],
        json!("[zh_CN] Hello from mock")
    );
    assert!(
        translated_envelope["payload"]["translated_meta"]["post_html"]
            .as_str()
            .is_some_and(|value| value.contains("<strong>world</strong>")),
        "rich_html field should survive the text component flow: {}",
        translated_envelope
    );
    assert_eq!(
        translated_envelope["payload"]["media_mappings"][0]["source_id"],
        json!(77)
    );
    assert_eq!(
        translated_envelope["payload"]["media_mappings"][0]["attachment_id"],
        json!(1077)
    );

    let callbacks = backend.callbacks().await;
    assert_eq!(backend.state.lock().await.media_uploads, 1);
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    let callback = &callbacks[0];
    assert_eq!(
        callback["translated_fields"]["post_title"],
        json!("[zh_CN] Hello from mock")
    );
    assert!(
        callback["translated_meta"]["post_html"]
            .as_str()
            .is_some_and(|value| value.contains("<strong>world</strong>")),
        "callback should include translated rich_html content: {}",
        callback
    );
    assert_eq!(callback["media_mappings"][0]["source_id"], json!(77));
    assert_eq!(callback["media_mappings"][0]["attachment_id"], json!(1077));

    backend.shutdown().await;
    Ok(())
}

async fn discovery_media_failure_case(mode: &str) -> anyhow::Result<()> {
    let _lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::TextImage).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;
    for (kind, template) in [
        ("text", text_template(&backend.base_url)),
        ("image", image_template(&backend.base_url)),
    ] {
        let id = format!("local-receipt-{kind}");
        assert_ok(
            &harness
                .create_local_component(json!({
                    "id":id,"name":id,"template_id":format!("official-mock-{kind}-v1"),
                    "template_json":template,"vendor_id":format!("mock-{kind}-vendor"),
                    "kind":kind,"enabled":true
                }))
                .await?,
        )?;
        assert_ok(&harness.upsert_task_type_binding(kind, &id).await?)?;
    }
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;
    let conn = rusqlite::Connection::open(&harness.db_path)?;
    match mode {
        "intent"=>conn.execute_batch("CREATE TRIGGER owned_discovery_intent BEFORE INSERT ON system_config WHEN NEW.key LIKE 'media-upload-receipt-v1:%' BEGIN SELECT RAISE(ABORT,'owned receipt insert refusal'); END;")?,
        "result"=>conn.execute_batch("CREATE TRIGGER owned_discovery_result BEFORE UPDATE ON system_config WHEN NEW.key LIKE 'media-upload-receipt-v1:%' BEGIN SELECT RAISE(ABORT,'owned receipt result refusal'); END;")?,
        "unknown"=>backend.state.lock().await.lose_media_response=true,
        _=>unreachable!(),
    }
    let run = harness.run_worker_once().await?;
    assert_ok(&run)?;
    assert_eq!(
        run.body["data"]["tasks_succeeded"],
        json!(0),
        "{}",
        run.body
    );
    assert_eq!(run.body["data"]["tasks_failed"], json!(1), "{}", run.body);
    assert!(
        backend.callbacks().await.is_empty(),
        "upload failure must stop the actual discovery callback"
    );
    assert_eq!(
        backend.state.lock().await.media_uploads,
        usize::from(mode != "intent")
    );
    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn discovery_media_intent_refusal_stops_real_worker_callback() -> anyhow::Result<()> {
    discovery_media_failure_case("intent").await
}
#[tokio::test]
async fn discovery_media_result_refusal_stops_real_worker_callback() -> anyhow::Result<()> {
    discovery_media_failure_case("result").await
}
#[tokio::test]
async fn discovery_media_unknown_stops_real_worker_callback() -> anyhow::Result<()> {
    discovery_media_failure_case("unknown").await
}

#[tokio::test]
async fn mock_audio_video_document_templates_support_real_worker_task_flow() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::AudioVideoDocument).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-audio",
                "name": "Local Mock Audio",
                "template_id": "official-mock-audio-v1",
                "template_json": audio_template(&backend.base_url),
                "vendor_id": "mock-audio-vendor",
                "vendor_name": "Mock Audio Vendor",
                "kind": "audio",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-video",
                "name": "Local Mock Video",
                "template_id": "official-mock-video-v1",
                "template_json": video_template(&backend.base_url),
                "vendor_id": "mock-video-vendor",
                "vendor_name": "Mock Video Vendor",
                "kind": "video",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-document",
                "name": "Local Mock Document",
                "template_id": "official-mock-document-v1",
                "template_json": document_template(&backend.base_url),
                "vendor_id": "mock-document-vendor",
                "vendor_name": "Mock Document Vendor",
                "kind": "document",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("audio", "local-mock-audio")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("video", "local-mock-video")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("document", "local-mock-document")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert!(component_ids.contains(&json!("local-mock-audio")));
    assert!(component_ids.contains(&json!("local-mock-video")));
    assert!(component_ids.contains(&json!("local-mock-document")));

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    let media_mappings = translated_envelope["payload"]["media_mappings"]
        .as_array()
        .ok_or_else(|| anyhow!("media_mappings should be an array"))?;
    assert_eq!(media_mappings.len(), 3);
    assert!(media_mappings
        .iter()
        .any(|item| item["source_id"] == json!(88)));
    assert!(media_mappings
        .iter()
        .any(|item| item["source_id"] == json!(89)));
    assert!(media_mappings
        .iter()
        .any(|item| item["source_id"] == json!(90)));

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    let callback = &callbacks[0];
    let callback_mappings = callback["media_mappings"]
        .as_array()
        .ok_or_else(|| anyhow!("callback media_mappings should be an array"))?;
    assert_eq!(callback_mappings.len(), 3);
    assert!(callback_mappings
        .iter()
        .any(|item| item["source_id"] == json!(88)));
    assert!(callback_mappings
        .iter()
        .any(|item| item["source_id"] == json!(89)));
    assert!(callback_mappings
        .iter()
        .any(|item| item["source_id"] == json!(90)));

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_structured_text_templates_support_real_worker_task_flow() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::StructuredFormats).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-structured",
                "name": "Local Mock Structured",
                "template_id": "official-mock-structured-v1",
                "template_json": structured_text_template(&backend.base_url),
                "vendor_id": "mock-structured-vendor",
                "vendor_name": "Mock Structured Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("text", "local-mock-structured")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert_eq!(component_ids.len(), 1);
    assert!(component_ids.contains(&json!("local-mock-structured")));

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_structured_payload(&translated_envelope["payload"], "local-mock-structured")?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_structured_payload(&callbacks[0], "local-mock-structured")?;

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_direct_and_key_bindings_support_real_worker_task_flow() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::AuthModesTaskFlow).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-auth-direct",
                "name": "Local Auth Direct",
                "template_id": "official-mock-auth-direct-v1",
                "template_json": direct_auth_text_template(&backend.base_url),
                "vendor_id": "mock-auth-direct-vendor",
                "vendor_name": "Mock Auth Direct Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-auth-key",
                "name": "Local Auth Key",
                "template_id": "official-mock-auth-key-v1",
                "template_json": key_pool_text_template(&backend.base_url),
                "vendor_id": "mock-auth-key-vendor",
                "vendor_name": "Mock Auth Key Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-auth-oauth",
                "name": "Local Auth OAuth",
                "template_id": "official-mock-auth-oauth-v1",
                "template_json": oauth_text_template(&backend.base_url),
                "vendor_id": "mock-auth-oauth-vendor",
                "vendor_name": "Mock Auth OAuth Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;

    assert_ok(
        &harness
            .post_json(
                "/api/components/bindings/upsert",
                json!({
                    "component_id": "local-auth-direct",
                    "auth": {
                        "api_key": "direct-key-123"
                    }
                }),
            )
            .await?,
    )?;

    assert_ok(
        &harness
            .post_json(
                "/api/vendor-keys",
                json!({
                    "id": "mock-key-pool-main",
                    "vendor_id": "mock-auth-key-vendor",
                    "label": "Mock Key Pool Main",
                    "auth_values": {
                        "api_key": "pooled-key-456"
                    },
                    "enabled": true
                }),
            )
            .await?,
    )?;
    assert_ok(
        &harness
            .post_json(
                "/api/components/bindings/upsert",
                json!({
                    "component_id": "local-auth-key",
                    "key_ids": ["mock-key-pool-main"]
                }),
            )
            .await?,
    )?;

    assert_ok(
        &harness
            .post_json(
                "/api/vendor-oauth",
                json!({
                    "id": "mock-oauth-pool-main",
                    "vendor_id": "mock-auth-oauth-vendor",
                    "label": "Mock OAuth Pool Main",
                    "grant_type": "client_credentials",
                    "token_url": format!("{}/mock/oauth/token", backend.base_url),
                    "client_id": "mock-client",
                    "client_secret": "mock-secret",
                    "scopes": "translate",
                    "token_field": "access_token"
                }),
            )
            .await?,
    )?;
    assert_ok(
        &harness
            .post_json(
                "/api/components/bindings/upsert",
                json!({
                    "component_id": "local-auth-oauth",
                    "oauth_ids": ["mock-oauth-pool-main"]
                }),
            )
            .await?,
    )?;

    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "plain_text", "local-auth-direct")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "rich_html", "local-auth-direct")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "json_structured", "local-auth-key")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "serialized_php", "local-auth-oauth")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert!(component_ids.contains(&json!("local-auth-direct")));
    assert!(component_ids.contains(&json!("local-auth-key")));

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_direct_and_key_payload(&translated_envelope["payload"])?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_direct_and_key_payload(&callbacks[0])?;

    let auth_hits = backend.auth_hits().await;
    assert!(auth_hits.iter().any(|hit| hit == "direct"));
    assert!(auth_hits.iter().any(|hit| hit == "key"));
    assert!(
        !auth_hits.iter().any(|hit| hit == "oauth"),
        "direct+key task flow should not call oauth endpoint"
    );
    assert_eq!(backend.oauth_token_requests().await, 0);

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_oauth_binding_supports_real_worker_task_flow() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::AuthModesTaskFlow).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-auth-oauth",
                "name": "Local Auth OAuth",
                "template_id": "official-mock-auth-oauth-v1",
                "template_json": oauth_text_template(&backend.base_url),
                "vendor_id": "mock-auth-oauth-vendor",
                "vendor_name": "Mock Auth OAuth Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .post_json(
                "/api/vendor-oauth",
                json!({
                    "id": "mock-oauth-task-main",
                    "vendor_id": "mock-auth-oauth-vendor",
                    "label": "Mock OAuth Task Main",
                    "grant_type": "client_credentials",
                    "token_url": format!("{}/mock/oauth/token", backend.base_url),
                    "client_id": "mock-client",
                    "client_secret": "mock-secret",
                    "scopes": "translate",
                    "token_field": "access_token"
                }),
            )
            .await?,
    )?;
    assert_ok(
        &harness
            .post_json(
                "/api/components/bindings/upsert",
                json!({
                    "component_id": "local-auth-oauth",
                    "oauth_ids": ["mock-oauth-task-main"]
                }),
            )
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("text", "local-auth-oauth")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert_eq!(component_ids, &vec![json!("local-auth-oauth")]);

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_oauth_payload(&translated_envelope["payload"], "local-auth-oauth")?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_oauth_payload(&callbacks[0], "local-auth-oauth")?;

    let auth_hits = backend.auth_hits().await;
    assert!(
        auth_hits.iter().any(|hit| hit == "oauth"),
        "oauth task flow should call oauth-backed vendor endpoint"
    );
    assert_eq!(
        backend.oauth_token_requests().await,
        1,
        "oauth task flow should fetch exactly one access token"
    );

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_wp_source_shapes_route_into_component_template_standard() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::WpSourceMixed).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-structured",
                "name": "Local Mock Structured",
                "template_id": "official-mock-structured-v1",
                "template_json": structured_text_template(&backend.base_url),
                "vendor_id": "mock-structured-vendor",
                "vendor_name": "Mock Structured Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-image",
                "name": "Local Mock Image",
                "template_id": "official-mock-image-v1",
                "template_json": image_template(&backend.base_url),
                "vendor_id": "mock-image-vendor",
                "vendor_name": "Mock Image Vendor",
                "kind": "image",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("text", "local-mock-structured")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("image", "local-mock-image")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert!(component_ids.contains(&json!("local-mock-structured")));
    assert!(component_ids.contains(&json!("local-mock-image")));

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_wp_source_payload(&translated_envelope["payload"], &backend.base_url)?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_wp_source_payload(&callbacks[0], &backend.base_url)?;

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_wp_source_rule_slot_bindings_route_formats_to_matching_components(
) -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::WpSourceMixed).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

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
            .create_local_component(json!({
                "id": "local-mock-structured",
                "name": "Local Mock Structured",
                "template_id": "official-mock-structured-v1",
                "template_json": structured_text_template(&backend.base_url),
                "vendor_id": "mock-structured-vendor",
                "vendor_name": "Mock Structured Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-image",
                "name": "Local Mock Image",
                "template_id": "official-mock-image-v1",
                "template_json": image_template(&backend.base_url),
                "vendor_id": "mock-image-vendor",
                "vendor_name": "Mock Image Vendor",
                "kind": "image",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "plain_text", "local-mock-text")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "rich_html", "local-mock-text")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding(
                "global",
                None,
                "json_structured",
                "local-mock-structured",
            )
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding(
                "global",
                None,
                "serialized_php",
                "local-mock-structured",
            )
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_rule_component_binding("global", None, "media_ref:image", "local-mock-image")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(1));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(1));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert!(component_ids.contains(&json!("local-mock-text")));
    assert!(component_ids.contains(&json!("local-mock-structured")));
    assert!(component_ids.contains(&json!("local-mock-image")));

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_wp_source_payload_routed(
        &translated_envelope["payload"],
        &backend.base_url,
        "local-mock-text",
        "local-mock-text",
        "local-mock-structured",
        "local-mock-text",
        "local-mock-image",
    )?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_wp_source_payload_routed(
        &callbacks[0],
        &backend.base_url,
        "local-mock-text",
        "local-mock-text",
        "local-mock-structured",
        "local-mock-text",
        "local-mock-image",
    )?;

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_taxonomy_source_shapes_route_into_component_template_standard() -> anyhow::Result<()>
{
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::TaxonomyMixed).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-taxonomy-text",
                "name": "Local Mock Taxonomy Text",
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
            .create_local_component(json!({
                "id": "local-mock-taxonomy-image",
                "name": "Local Mock Taxonomy Image",
                "template_id": "official-mock-image-v1",
                "template_json": image_template(&backend.base_url),
                "vendor_id": "mock-image-vendor",
                "vendor_name": "Mock Image Vendor",
                "kind": "image",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("text", "local-mock-taxonomy-text")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("image", "local-mock-taxonomy-image")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;

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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(item_rows.len(), 1, "expected one translated taxonomy item");
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert!(component_ids.contains(&json!("local-mock-taxonomy-text")));
    assert!(component_ids.contains(&json!("local-mock-taxonomy-image")));

    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_taxonomy_payload(&translated_envelope["payload"], &backend.base_url)?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_taxonomy_payload(&callbacks[0], &backend.base_url)?;

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_wp_url_like_fields_stay_outside_component_translation_scope() -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::WpSourceMixed).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-structured",
                "name": "Local Mock Structured",
                "template_id": "official-mock-structured-v1",
                "template_json": structured_text_template(&backend.base_url),
                "vendor_id": "mock-structured-vendor",
                "vendor_name": "Mock Structured Vendor",
                "kind": "text",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-image",
                "name": "Local Mock Image",
                "template_id": "official-mock-image-v1",
                "template_json": image_template(&backend.base_url),
                "vendor_id": "mock-image-vendor",
                "vendor_name": "Mock Image Vendor",
                "kind": "image",
                "enabled": true
            }))
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("text", "local-mock-structured")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_task_type_binding("image", "local-mock-image")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;

    let jobs = harness.list_jobs().await?;
    assert_ok(&jobs)?;
    let job_items = jobs.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("jobs.items should be an array"))?;
    let job_id = job_items[0]["id"]
        .as_i64()
        .ok_or_else(|| anyhow!("job id missing"))?;
    let items = harness.list_job_items(job_id).await?;
    assert_ok(&items)?;
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    let item = &item_rows[0];
    let translated_path = item["translated_path"]
        .as_str()
        .ok_or_else(|| anyhow!("translated_path missing"))?;
    let translated_envelope = read_json_file(Path::new(translated_path))?;
    assert_url_like_fields_absent(&translated_envelope["payload"])?;

    let callbacks = backend.callbacks().await;
    assert_eq!(callbacks.len(), 1, "expected a single callback payload");
    assert_url_like_fields_absent(&callbacks[0])?;

    backend.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mock_language_pack_gettext_entries_support_context_and_plural_source_selection(
) -> anyhow::Result<()> {
    let _test_lock = component_template_flow_test_lock().lock().await;
    let backend = MockBackendHandle::start(MockScenario::LanguagePackPlugin).await?;
    let harness = WebUiTestHarness::new(&backend.base_url, Some(SESSION_TOKEN)).await?;

    assert_ok(
        &harness
            .create_local_component(json!({
                "id": "local-mock-plugin-i18n",
                "name": "Local Mock Plugin I18n",
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
            .upsert_task_type_binding("text", "local-mock-plugin-i18n")
            .await?,
    )?;
    assert_ok(
        &harness
            .upsert_domain_binding(&backend.api_base_url, WP_CLIENT_TOKEN, ROUTE_SECRET)
            .await?,
    )?;

    let run_once = harness.run_worker_once().await?;
    assert_ok(&run_once)?;
    assert_eq!(run_once.body["data"]["tasks_processed"], json!(2));
    assert_eq!(run_once.body["data"]["tasks_succeeded"], json!(2));
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
    let item_rows = items.body["data"]["items"]
        .as_array()
        .ok_or_else(|| anyhow!("job items should be an array"))?;
    assert_eq!(
        item_rows.len(),
        1,
        "expected one persisted language-pack batch"
    );
    let item = &item_rows[0];
    assert_eq!(item["status"], json!("done"));
    let component_ids = item["component_ids"]
        .as_array()
        .ok_or_else(|| anyhow!("component_ids should be an array"))?;
    assert_eq!(component_ids, &vec![json!("local-mock-plugin-i18n")]);

    let callbacks = backend.callbacks().await;
    assert_eq!(
        callbacks.len(),
        1,
        "expected a single language-pack callback"
    );
    assert_language_pack_callback(&callbacks[0])?;

    backend.shutdown().await;
    Ok(())
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

fn assert_structured_payload(payload: &Value, component_id: &str) -> anyhow::Result<()> {
    let translated_meta = payload["translated_meta"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_meta should be an object"))?;
    let translated_fields = payload["translated_fields"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_fields should be an object"))?;
    assert!(
        translated_fields.is_empty(),
        "structured scenario should currently write everything into translated_meta: {}",
        payload
    );

    let rich_html = translated_meta
        .get("post_html")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("post_html missing from translated_meta"))?;
    assert!(
        rich_html.contains("<p>"),
        "rich_html should keep paragraph markup: {rich_html}"
    );
    assert!(
        rich_html.contains("<strong>"),
        "rich_html should keep inline markup: {rich_html}"
    );
    assert!(
        rich_html.starts_with("[zh_CN] "),
        "rich_html should include translated prefix: {rich_html}"
    );

    let seo_json = translated_meta
        .get("seo_json")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("seo_json missing from translated_meta"))?;
    let seo_value: Value =
        serde_json::from_str(seo_json).with_context(|| "seo_json should remain valid JSON")?;
    assert_eq!(seo_value["headline"], json!("[zh_CN] Hello SEO"));
    assert_eq!(seo_value["keywords"][0], json!("[zh_CN] Alpha"));
    assert_eq!(seo_value["keywords"][1], json!("[zh_CN] Beta"));
    assert_eq!(
        seo_value["meta"]["summary"],
        json!("[zh_CN] Nested summary")
    );
    assert_eq!(seo_value["count"], json!(2));
    assert_eq!(seo_value["enabled"], json!(true));

    let acf_group = translated_meta
        .get("acf_group")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("acf_group missing from translated_meta"))?;
    assert!(
        acf_group.starts_with("a:"),
        "serialized_php should be restored: {acf_group}"
    );
    assert!(acf_group.contains("[zh_CN] Hello ACF"));
    assert!(acf_group.contains("[zh_CN] One"));
    assert!(acf_group.contains("[zh_CN] Two"));
    assert!(acf_group.contains("b:1;"));

    let field_results = payload["field_results"]
        .as_array()
        .ok_or_else(|| anyhow!("field_results should be an array"))?;
    assert_eq!(field_results.len(), 3);

    let post_html = field_result_by_name(field_results, "post_html")?;
    assert_eq!(post_html["status"], json!("success"));
    assert_eq!(post_html["content_format"], json!("rich_html"));
    assert_eq!(post_html["provider_component"], json!(component_id));
    assert_eq!(post_html["merge_target"], json!("translated_meta"));
    assert_eq!(post_html["transform_stage"], json!("direct"));

    let seo_json_row = field_result_by_name(field_results, "seo_json")?;
    assert_eq!(seo_json_row["status"], json!("success"));
    assert_eq!(seo_json_row["content_format"], json!("json_structured"));
    assert_eq!(seo_json_row["provider_component"], json!(component_id));
    assert_eq!(seo_json_row["merge_target"], json!("translated_meta"));
    assert_eq!(seo_json_row["transform_stage"], json!("direct"));

    let acf_group_row = field_result_by_name(field_results, "acf_group")?;
    assert_eq!(acf_group_row["status"], json!("success"));
    assert_eq!(acf_group_row["content_format"], json!("serialized_php"));
    assert_eq!(acf_group_row["provider_component"], json!(component_id));
    assert_eq!(acf_group_row["merge_target"], json!("translated_meta"));
    assert_eq!(
        acf_group_row["transform_stage"],
        json!("serialized_php_to_json_structured")
    );

    Ok(())
}

fn assert_oauth_payload(payload: &Value, component_id: &str) -> anyhow::Result<()> {
    let translated_meta = payload["translated_meta"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_meta should be an object"))?;
    let translated_fields = payload["translated_fields"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_fields should be an object"))?;
    assert!(
        translated_fields.is_empty(),
        "oauth-only template should not translate unsupported plain_text fields: {}",
        payload
    );

    let rich_html = translated_meta
        .get("post_html")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("post_html missing from translated_meta"))?;
    assert!(rich_html.contains("<p>"));
    assert!(rich_html.contains("<strong>"));
    assert!(rich_html.starts_with("[zh_CN] "));

    let seo_json = translated_meta
        .get("seo_json")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("seo_json missing from translated_meta"))?;
    let seo_value: Value =
        serde_json::from_str(seo_json).with_context(|| "seo_json should remain valid JSON")?;
    assert_eq!(seo_value["headline"], json!("[zh_CN] Key auth headline"));
    assert_eq!(seo_value["keywords"][0], json!("[zh_CN] Alpha"));
    assert_eq!(seo_value["keywords"][1], json!("[zh_CN] Beta"));

    let acf_group = translated_meta
        .get("acf_group")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("acf_group missing from translated_meta"))?;
    assert!(acf_group.starts_with("a:"));
    assert!(acf_group.contains("[zh_CN] OAuth auth headline"));
    assert!(acf_group.contains("[zh_CN] One"));
    assert!(acf_group.contains("[zh_CN] Two"));

    let field_results = payload["field_results"]
        .as_array()
        .ok_or_else(|| anyhow!("field_results should be an array"))?;
    assert_eq!(field_results.len(), 4);

    let post_title = field_result_by_name(field_results, "post_title")?;
    assert_eq!(post_title["status"], json!("failed"));
    assert_eq!(post_title["content_format"], json!("plain_text"));
    assert_eq!(post_title["provider_component"], Value::Null);
    assert_eq!(post_title["merge_target"], Value::Null);
    assert_eq!(post_title["transform_stage"], Value::Null);
    assert_eq!(post_title["detail"], json!("no_component_for_format"));

    let post_html = field_result_by_name(field_results, "post_html")?;
    assert_eq!(post_html["status"], json!("success"));
    assert_eq!(post_html["content_format"], json!("rich_html"));
    assert_eq!(post_html["provider_component"], json!(component_id));
    assert_eq!(post_html["merge_target"], json!("translated_meta"));
    assert_eq!(post_html["transform_stage"], json!("direct"));

    let seo_json_row = field_result_by_name(field_results, "seo_json")?;
    assert_eq!(seo_json_row["status"], json!("success"));
    assert_eq!(seo_json_row["content_format"], json!("json_structured"));
    assert_eq!(seo_json_row["provider_component"], json!(component_id));
    assert_eq!(seo_json_row["merge_target"], json!("translated_meta"));
    assert_eq!(seo_json_row["transform_stage"], json!("direct"));

    let acf_group_row = field_result_by_name(field_results, "acf_group")?;
    assert_eq!(acf_group_row["status"], json!("success"));
    assert_eq!(acf_group_row["content_format"], json!("serialized_php"));
    assert_eq!(acf_group_row["provider_component"], json!(component_id));
    assert_eq!(acf_group_row["merge_target"], json!("translated_meta"));
    assert_eq!(
        acf_group_row["transform_stage"],
        json!("serialized_php_to_json_structured")
    );

    Ok(())
}

fn assert_direct_and_key_payload(payload: &Value) -> anyhow::Result<()> {
    assert_eq!(
        payload["translated_fields"]["post_title"],
        json!("[zh_CN] Direct auth title")
    );

    let translated_meta = payload["translated_meta"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_meta should be an object"))?;

    let post_html = translated_meta
        .get("post_html")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("post_html missing from translated_meta"))?;
    assert!(
        post_html.starts_with("[zh_CN] "),
        "rich_html should be translated by direct-auth component: {post_html}"
    );
    assert!(
        post_html.contains("<strong>content</strong>"),
        "rich_html markup should be preserved: {post_html}"
    );

    let seo_json = translated_meta
        .get("seo_json")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("seo_json missing from translated_meta"))?;
    let seo_value: Value =
        serde_json::from_str(seo_json).with_context(|| "seo_json should remain valid JSON")?;
    assert_eq!(seo_value["headline"], json!("[zh_CN] Key auth headline"));
    assert_eq!(seo_value["keywords"][0], json!("[zh_CN] Alpha"));
    assert_eq!(seo_value["keywords"][1], json!("[zh_CN] Beta"));

    let acf_group = translated_meta
        .get("acf_group")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("acf_group missing from translated_meta"))?;
    assert!(acf_group.starts_with("a:"));
    assert!(acf_group.contains("[zh_CN] OAuth auth headline"));
    assert!(acf_group.contains("[zh_CN] One"));
    assert!(acf_group.contains("[zh_CN] Two"));

    let field_results = payload["field_results"]
        .as_array()
        .ok_or_else(|| anyhow!("field_results should be an array"))?;
    assert_eq!(field_results.len(), 4);

    let post_title = field_result_by_name(field_results, "post_title")?;
    assert_eq!(post_title["provider_component"], json!("local-auth-direct"));
    assert_eq!(post_title["content_format"], json!("plain_text"));

    let post_html_row = field_result_by_name(field_results, "post_html")?;
    assert_eq!(
        post_html_row["provider_component"],
        json!("local-auth-direct")
    );
    assert_eq!(post_html_row["content_format"], json!("rich_html"));
    assert_eq!(post_html_row["merge_target"], json!("translated_meta"));

    let seo_json_row = field_result_by_name(field_results, "seo_json")?;
    assert_eq!(seo_json_row["provider_component"], json!("local-auth-key"));
    assert_eq!(seo_json_row["content_format"], json!("json_structured"));

    let acf_group_row = field_result_by_name(field_results, "acf_group")?;
    assert_eq!(acf_group_row["provider_component"], json!("local-auth-key"));
    assert_eq!(acf_group_row["content_format"], json!("serialized_php"));
    assert_eq!(
        acf_group_row["transform_stage"],
        json!("serialized_php_to_json_structured")
    );

    Ok(())
}

fn assert_wp_source_payload(payload: &Value, base_url: &str) -> anyhow::Result<()> {
    assert_wp_source_payload_routed(
        payload,
        base_url,
        "local-mock-structured",
        "local-mock-structured",
        "local-mock-structured",
        "local-mock-structured",
        "local-mock-image",
    )
}

fn assert_wp_source_payload_routed(
    payload: &Value,
    base_url: &str,
    plain_text_component: &str,
    rich_html_component: &str,
    structured_component: &str,
    media_text_component: &str,
    image_component: &str,
) -> anyhow::Result<()> {
    assert_eq!(
        payload["translated_fields"]["post_title"],
        json!("[zh_CN] Hello product")
    );

    let post_content = payload["translated_fields"]["post_content"]
        .as_str()
        .ok_or_else(|| anyhow!("post_content missing from translated_fields"))?;
    assert!(post_content.starts_with("[zh_CN] "));
    assert!(post_content.contains("<strong>builder</strong>"));

    let translated_meta = payload["translated_meta"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_meta should be an object"))?;

    let seo_schema = translated_meta
        .get("seo_schema")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("seo_schema missing from translated_meta"))?;
    let seo_value: Value =
        serde_json::from_str(seo_schema).with_context(|| "seo_schema should remain valid JSON")?;
    assert_eq!(seo_value["headline"], json!("[zh_CN] SEO headline"));
    assert_eq!(seo_value["faq"][0]["q"], json!("[zh_CN] What is it?"));
    assert_eq!(
        seo_value["faq"][0]["a"],
        json!("[zh_CN] A translated answer")
    );
    assert_eq!(seo_value["rating"], json!(5));

    let acf_group = translated_meta
        .get("acf_group")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("acf_group missing from translated_meta"))?;
    assert!(acf_group.starts_with("a:"));
    assert!(acf_group.contains("[zh_CN] Feature headline"));
    assert!(acf_group.contains("[zh_CN] Fast"));
    assert!(acf_group.contains("[zh_CN] Flexible"));

    let hero_image_alt = translated_meta
        .get("hero_image_alt")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("hero_image_alt missing from translated_meta"))?;
    assert_eq!(hero_image_alt, "[zh_CN] Product hero alt");

    let media_mappings = payload["media_mappings"]
        .as_array()
        .ok_or_else(|| anyhow!("media_mappings should be an array"))?;
    assert_eq!(media_mappings.len(), 1);
    assert_eq!(media_mappings[0]["source_id"], json!(91));
    assert_eq!(media_mappings[0]["attachment_id"], json!(1091));

    let field_results = payload["field_results"]
        .as_array()
        .ok_or_else(|| anyhow!("field_results should be an array"))?;
    assert_eq!(field_results.len(), 8);

    let post_title = field_result_by_name(field_results, "post_title")?;
    assert_eq!(post_title["status"], json!("success"));
    assert_eq!(post_title["content_format"], json!("plain_text"));
    assert_eq!(
        post_title["provider_component"],
        json!(plain_text_component)
    );
    assert_eq!(post_title["merge_target"], json!("translated_fields"));

    let post_content_row = field_result_by_name(field_results, "post_content")?;
    assert_eq!(post_content_row["status"], json!("success"));
    assert_eq!(post_content_row["content_format"], json!("rich_html"));
    assert_eq!(
        post_content_row["provider_component"],
        json!(rich_html_component)
    );
    assert_eq!(post_content_row["merge_target"], json!("translated_fields"));

    let seo_schema_row = field_result_by_name(field_results, "seo_schema")?;
    assert_eq!(seo_schema_row["status"], json!("success"));
    assert_eq!(seo_schema_row["content_format"], json!("json_structured"));
    assert_eq!(
        seo_schema_row["provider_component"],
        json!(structured_component)
    );
    assert_eq!(seo_schema_row["merge_target"], json!("translated_meta"));

    let acf_group_row = field_result_by_name(field_results, "acf_group")?;
    assert_eq!(acf_group_row["status"], json!("success"));
    assert_eq!(acf_group_row["content_format"], json!("serialized_php"));
    assert_eq!(
        acf_group_row["provider_component"],
        json!(structured_component)
    );
    assert_eq!(acf_group_row["merge_target"], json!("translated_meta"));
    assert_eq!(
        acf_group_row["transform_stage"],
        json!("serialized_php_to_json_structured")
    );

    let hero_image_alt_row = field_result_by_name(field_results, "hero_image_alt")?;
    assert_eq!(hero_image_alt_row["status"], json!("success"));
    assert_eq!(hero_image_alt_row["content_format"], json!("media_ref"));
    assert_eq!(
        hero_image_alt_row["provider_component"],
        json!(media_text_component)
    );
    assert_eq!(hero_image_alt_row["merge_target"], json!("translated_meta"));
    assert_eq!(
        hero_image_alt_row["transform_stage"],
        json!("media_text_to_plain_text")
    );

    let hero_media_id_row = field_result_by_name(field_results, "hero_media_id")?;
    assert_eq!(hero_media_id_row["status"], json!("success"));
    assert_eq!(hero_media_id_row["content_format"], json!("media_ref"));
    assert_eq!(
        hero_media_id_row["provider_component"],
        json!(image_component)
    );
    assert_eq!(hero_media_id_row["merge_target"], json!("media_mappings"));
    assert_eq!(
        hero_media_id_row["transform_stage"],
        json!("media_ref_direct")
    );

    let seo_slug_row = field_result_by_name(field_results, "seo_slug")?;
    assert_eq!(seo_slug_row["status"], json!("success"));
    assert_eq!(seo_slug_row["content_format"], json!("slug"));
    assert_eq!(
        seo_slug_row["provider_component"],
        json!(plain_text_component)
    );
    assert_eq!(seo_slug_row["merge_target"], json!("translated_meta"));
    assert_eq!(seo_slug_row["transform_stage"], json!("format_adapted"));
    assert_eq!(seo_slug_row["detail"], json!("translated"));

    let template_code_row = field_result_by_name(field_results, "template_code")?;
    assert_eq!(template_code_row["status"], json!("skipped"));
    assert_eq!(template_code_row["content_format"], json!("code"));
    assert_eq!(template_code_row["provider_component"], Value::Null);
    assert_eq!(template_code_row["merge_target"], json!("translated_meta"));
    assert_eq!(template_code_row["transform_stage"], json!("direct"));
    assert_eq!(
        template_code_row["detail"],
        json!("non_translatable_or_no_change")
    );

    Ok(())
}

fn assert_taxonomy_payload(payload: &Value, base_url: &str) -> anyhow::Result<()> {
    assert_eq!(
        payload["business_line"],
        json!("taxonomy_content"),
        "taxonomy items should route through taxonomy_content: {}",
        payload
    );
    assert_eq!(
        payload["translated_fields"]["name"],
        json!("[zh_CN] Summer Sale")
    );

    let description = payload["translated_fields"]["description"]
        .as_str()
        .ok_or_else(|| anyhow!("description missing from translated_fields"))?;
    assert!(description.starts_with("[zh_CN] "));
    assert!(description.contains("<strong>category</strong>"));

    let term_image_alt = payload["translated_meta"]["term_image_alt"]
        .as_str()
        .ok_or_else(|| anyhow!("term_image_alt missing from translated_meta"))?;
    assert_eq!(term_image_alt, "[zh_CN] Seasonal category banner");

    let media_mappings = payload["media_mappings"]
        .as_array()
        .ok_or_else(|| anyhow!("media_mappings should be an array"))?;
    assert_eq!(media_mappings.len(), 1);
    assert_eq!(media_mappings[0]["source_id"], json!(92));
    assert_eq!(media_mappings[0]["attachment_id"], json!(1092));

    let field_results = payload["field_results"]
        .as_array()
        .ok_or_else(|| anyhow!("field_results should be an array"))?;
    assert_eq!(field_results.len(), 5);

    let name_row = field_result_by_name(field_results, "name")?;
    assert_eq!(name_row["status"], json!("success"));
    assert_eq!(name_row["storage"], json!("term_column"));
    assert_eq!(name_row["content_format"], json!("plain_text"));
    assert_eq!(
        name_row["provider_component"],
        json!("local-mock-taxonomy-text")
    );

    let description_row = field_result_by_name(field_results, "description")?;
    assert_eq!(description_row["status"], json!("success"));
    assert_eq!(description_row["storage"], json!("term_column"));
    assert_eq!(description_row["content_format"], json!("rich_html"));
    assert_eq!(
        description_row["provider_component"],
        json!("local-mock-taxonomy-text")
    );

    let term_image_alt_row = field_result_by_name(field_results, "term_image_alt")?;
    assert_eq!(term_image_alt_row["status"], json!("success"));
    assert_eq!(term_image_alt_row["storage"], json!("term_meta"));
    assert_eq!(term_image_alt_row["content_format"], json!("media_ref"));
    assert_eq!(
        term_image_alt_row["provider_component"],
        json!("local-mock-taxonomy-text")
    );
    assert_eq!(
        term_image_alt_row["transform_stage"],
        json!("media_text_to_plain_text")
    );

    let term_image_id_row = field_result_by_name(field_results, "term_image_id")?;
    assert_eq!(term_image_id_row["status"], json!("success"));
    assert_eq!(term_image_id_row["storage"], json!("term_meta"));
    assert_eq!(term_image_id_row["content_format"], json!("media_ref"));
    assert_eq!(
        term_image_id_row["provider_component"],
        json!("local-mock-taxonomy-image")
    );

    let slug_row = field_result_by_name(field_results, "slug")?;
    assert_eq!(slug_row["status"], json!("success"));
    assert_eq!(slug_row["storage"], json!("term_column"));
    assert_eq!(slug_row["content_format"], json!("slug"));
    assert_eq!(
        slug_row["provider_component"],
        json!("local-mock-taxonomy-text")
    );
    assert_eq!(slug_row["transform_stage"], json!("format_adapted"));

    Ok(())
}

fn assert_url_like_fields_absent(payload: &Value) -> anyhow::Result<()> {
    let translated_fields = payload["translated_fields"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_fields should be an object"))?;
    let translated_meta = payload["translated_meta"]
        .as_object()
        .ok_or_else(|| anyhow!("translated_meta should be an object"))?;
    let field_results = payload["field_results"]
        .as_array()
        .ok_or_else(|| anyhow!("field_results should be an array"))?;

    for blocked in ["cta_href", "gallery_srcset", "hero_poster"] {
        assert!(
            !translated_fields.contains_key(blocked),
            "url-like field should stay out of translated_fields: {}",
            payload
        );
        assert!(
            !translated_meta.contains_key(blocked),
            "url-like field should stay out of translated_meta: {}",
            payload
        );
        assert!(
            field_results
                .iter()
                .all(|row| row["field"] != json!(blocked)),
            "url-like field should not be routed into translation field_results: {}",
            payload
        );
    }

    Ok(())
}

fn assert_language_pack_callback(payload: &Value) -> anyhow::Result<()> {
    assert_eq!(payload["business_line"], json!("plugin_i18n"));
    assert_eq!(payload["source_lang"], json!("en_US"));
    assert_eq!(payload["target_lang"], json!("zh_CN"));

    let entries = payload["entries"]
        .as_array()
        .ok_or_else(|| anyhow!("entries should be an array"))?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["entry_id"], json!(1001));
    assert_eq!(entries[0]["msgstr"], json!("[zh_CN] Save changes"));
    assert_eq!(entries[1]["entry_id"], json!(1002));
    assert_eq!(entries[1]["msgstr"], json!("[zh_CN] %d files deleted"));

    Ok(())
}

fn field_result_by_name<'a>(
    field_results: &'a [Value],
    field_name: &str,
) -> anyhow::Result<&'a Value> {
    field_results
        .iter()
        .find(|row| row["field"] == json!(field_name))
        .ok_or_else(|| anyhow!("field result missing for {}", field_name))
}

async fn handle_backend_connection(
    mut socket: TcpStream,
    state: Arc<Mutex<MockBackendState>>,
    base_url: &str,
    scenario: MockScenario,
) -> anyhow::Result<()> {
    let request = read_http_request(&mut socket).await?;
    let (path, query) = split_target(&request.target);

    match (request.method.as_str(), path.as_str()) {
        ("POST", "/wp-json/wptsall/v2/route-test/client/media-upload") => {
            assert_eq!(request.header("X-WPTSALL-Protocol-Version"), Some("2"));
            assert_eq!(
                request.header("X-WPTSALL-Client-Token"),
                Some(WP_CLIENT_TOKEN)
            );
            assert!(!request.body.is_empty());
            let source_id = request
                .header("X-WPTSALL-Source-ID")
                .unwrap()
                .parse::<u64>()?;
            {
                let mut guard = state.lock().await;
                guard.media_uploads += 1;
                if guard.lose_media_response {
                    return Ok(());
                }
            }
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({"success":true,"attachment_id":source_id+1000,
                    "operation_id":request.header("X-WPTSALL-Operation-ID").unwrap(),
                    "content_sha256":request.header("X-WPTSALL-Content-SHA256").unwrap(),
                    "source_id":source_id,
                    "task_id":request.header("X-WPTSALL-Task-ID").unwrap().parse::<i64>()?,
                    "relation_id":request.header("X-WPTSALL-Relation-ID").unwrap().parse::<i64>()?,
                }),
            )
            .await?;
        }
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
        ("GET", "/api/v1/client/components") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": {
                        "items": component_catalog_for_scenario(scenario),
                        "page": 1,
                        "per_page": 200,
                        "total": component_catalog_for_scenario(scenario).len(),
                        "total_pages": 1
                    }
                }),
            )
            .await?;
        }
        ("GET", _) if path.starts_with("/api/v1/components/") => {
            let component_id = path.trim_start_matches("/api/v1/components/").trim();
            let Some(component) = component_detail_for_scenario(scenario, component_id) else {
                write_json_response(
                    &mut socket,
                    "404 Not Found",
                    &json!({
                        "success": false,
                        "error": {
                            "code": "COMPONENT_NOT_FOUND",
                            "message": "Component not found"
                        }
                    }),
                )
                .await?;
                return Ok(());
            };
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": {
                        "component": component
                    }
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
                    "data": signed_component_download("official-mock-text-v1", text_template(base_url))
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-image-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download("official-mock-image-v1", image_template(base_url))
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-audio-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download("official-mock-audio-v1", audio_template(base_url))
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-video-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download("official-mock-video-v1", video_template(base_url))
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-document-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download("official-mock-document-v1", document_template(base_url))
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-structured-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download(
                        "official-mock-structured-v1",
                        structured_text_template(base_url),
                    )
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-auth-direct-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download(
                        "official-mock-auth-direct-v1",
                        direct_auth_text_template(base_url),
                    )
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-auth-key-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download(
                        "official-mock-auth-key-v1",
                        key_pool_text_template(base_url),
                    )
                }),
            )
            .await?;
        }
        ("GET", "/api/v1/client/components/official-mock-auth-oauth-v1/download") => {
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "success": true,
                    "data": signed_component_download(
                        "official-mock-auth-oauth-v1",
                        oauth_text_template(base_url),
                    )
                }),
            )
            .await?;
        }
        ("GET", "/wp-json/wptsall/v2/route-test/client/site-relations") => {
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "relations": [relation_payload_for_scenario(scenario)]
                }),
            )
            .await?;
        }
        ("GET", "/wp-json/wptsall/v2/route-test/client/rules") => {
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
                &json!({ "rules": rules_for_scenario(scenario) }),
            )
            .await?;
        }
        ("GET", "/wp-json/wptsall/v2/route-test/client/content") => {
            let params = parse_query(query.as_deref().unwrap_or_default());
            let data_type = params
                .get("data_type")
                .map(String::as_str)
                .unwrap_or_default();
            let subtype = params
                .get("subtype")
                .map(String::as_str)
                .unwrap_or_default();
            let page = params.get("page").map(String::as_str).unwrap_or("1");
            let items = if matches!(scenario, MockScenario::LanguagePackPlugin)
                && data_type == "language_pack"
            {
                let mut guard = state.lock().await;
                let served = guard.language_pack_batches_served;
                guard.language_pack_batches_served += 1;
                if served == 0 {
                    content_items_for_scenario(base_url, scenario, data_type, subtype, page)
                } else {
                    Vec::new()
                }
            } else {
                content_items_for_scenario(base_url, scenario, data_type, subtype, page)
            };
            // These fixtures model the pre-snapshot mock WP endpoint. Add a
            // deterministic claim snapshot here so the integration test keeps
            // exercising the production pipeline's strict callback contract.
            let mut items = items;
            for item in &mut items {
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
            let total = if data_type == "language_pack" {
                i64::try_from(items.len()).unwrap_or(0)
            } else if items.is_empty() {
                0
            } else {
                1
            };
            write_wp_json_response(
                &mut socket,
                "200 OK",
                WP_CLIENT_TOKEN,
                &json!({
                    "items": items,
                    "total": total,
                    "page": page.parse::<i64>().unwrap_or(1),
                    "per_page": 50
                }),
            )
            .await?;
        }
        ("POST", "/wp-json/wptsall/v2/route-test/client/content/claim") => {
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
        ("POST", "/wp-json/wptsall/v2/route-test/client/translation-callback") => {
            let body_json = decode_wp_transport_request(WP_CLIENT_TOKEN, &request.body)?;
            let entries_count = body_json
                .get("entries")
                .and_then(Value::as_array)
                .map(Vec::len);
            let mut receipt = json!({
                "success": true,
                "queued": false,
                "result_id": 1,
                "protocol": "v2",
                "result_status": "synced"
            });
            if let Some(entries_count) = entries_count {
                receipt["entries_updated"] = json!(entries_count);
                receipt["entries_rejected"] = json!(0);
                receipt["updated_count"] = json!(entries_count);
            }
            state.lock().await.callbacks.push(body_json);
            write_wp_json_response(&mut socket, "200 OK", WP_CLIENT_TOKEN, &receipt).await?;
        }
        ("GET", AUDIO_URL_PATH) => {
            write_binary_response(&mut socket, "200 OK", "audio/mpeg", b"mock-audio").await?;
        }
        ("GET", VIDEO_URL_PATH) => {
            write_binary_response(&mut socket, "200 OK", "video/mp4", b"mock-video").await?;
        }
        ("GET", DOCUMENT_URL_PATH) => {
            write_binary_response(
                &mut socket,
                "200 OK",
                "application/pdf",
                b"%PDF-1.4\nmock-pdf",
            )
            .await?;
        }
        ("GET", WP_IMAGE_URL_PATH) => {
            write_binary_response(&mut socket, "200 OK", "image/jpeg", b"mock-jpeg").await?;
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
        ("POST", "/mock/vendor/auth/direct") => {
            if request.header("x-mock-api-key") != Some("direct-key-123") {
                write_json_response(
                    &mut socket,
                    "401 Unauthorized",
                    &json!({
                        "error": "missing_or_invalid_direct_key"
                    }),
                )
                .await?;
                return Ok(());
            }
            let body_json = request.json_body()?;
            let text = body_json["text"].as_str().unwrap_or_default();
            let target_lang = body_json["target_lang"].as_str().unwrap_or("zh_CN");
            {
                let mut guard = state.lock().await;
                guard.auth_hits.push("direct".to_string());
            }
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_text": format!("[{}] {}", target_lang, text)
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/auth/key") => {
            if request.header("x-mock-api-key") != Some("pooled-key-456") {
                write_json_response(
                    &mut socket,
                    "401 Unauthorized",
                    &json!({
                        "error": "missing_or_invalid_pooled_key"
                    }),
                )
                .await?;
                return Ok(());
            }
            let body_json = request.json_body()?;
            let text = body_json["text"].as_str().unwrap_or_default();
            let target_lang = body_json["target_lang"].as_str().unwrap_or("zh_CN");
            {
                let mut guard = state.lock().await;
                guard.auth_hits.push("key".to_string());
            }
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_text": format!("[{}] {}", target_lang, text)
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/auth/oauth") => {
            if request.header("authorization") != Some("Bearer oauth-access-token-789") {
                write_json_response(
                    &mut socket,
                    "401 Unauthorized",
                    &json!({
                        "error": "missing_or_invalid_oauth_token"
                    }),
                )
                .await?;
                return Ok(());
            }
            let body_json = request.json_body()?;
            let text = body_json["text"].as_str().unwrap_or_default();
            let target_lang = body_json["target_lang"].as_str().unwrap_or("zh_CN");
            {
                let mut guard = state.lock().await;
                guard.auth_hits.push("oauth".to_string());
            }
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_text": format!("[{}] {}", target_lang, text)
                }),
            )
            .await?;
        }
        ("POST", "/mock/oauth/token") => {
            let form = parse_query(String::from_utf8_lossy(&request.body).as_ref());
            if form.get("grant_type").map(String::as_str) != Some("client_credentials")
                || form.get("client_id").map(String::as_str) != Some("mock-client")
                || form.get("client_secret").map(String::as_str) != Some("mock-secret")
            {
                write_json_response(
                    &mut socket,
                    "401 Unauthorized",
                    &json!({
                        "error": "invalid_client"
                    }),
                )
                .await?;
                return Ok(());
            }
            {
                let mut guard = state.lock().await;
                guard.oauth_token_requests += 1;
            }
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "access_token": "oauth-access-token-789",
                    "expires_in": 3600
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/image") => {
            let body_json = request.json_body()?;
            let source_ref = body_json["source_ref"].as_str().unwrap_or_default();
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_ref": source_ref,
                    "translated_text": "[image-ok]"
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/audio") => {
            let body_json = request.json_body()?;
            let source_ref = body_json["source_ref"].as_str().unwrap_or_default();
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_ref": source_ref,
                    "translated_text": "[audio-ok]"
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/video") => {
            let body_json = request.json_body()?;
            let source_ref = body_json["source_ref"].as_str().unwrap_or_default();
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_ref": source_ref,
                    "translated_text": "[video-ok]"
                }),
            )
            .await?;
        }
        ("POST", "/mock/vendor/document") => {
            let body_json = request.json_body()?;
            let source_ref = body_json["source_ref"].as_str().unwrap_or_default();
            write_json_response(
                &mut socket,
                "200 OK",
                &json!({
                    "translated_ref": source_ref,
                    "translated_text": "[document-ok]"
                }),
            )
            .await?;
        }
        // Identity Contract v1.1 §5 (C-1): the domain loop fail-closes on
        // binding identity before dispatch, so run-once first pings the
        // mock WP backend for identity. Serve the ATS plugin's ping
        // envelope (signed, like every other WP response here).
        ("GET", "/wp-json/wptsall/v2/route-test/client/ping") => {
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

fn component_catalog_for_scenario(scenario: MockScenario) -> Vec<Value> {
    match scenario {
        MockScenario::TextImage => vec![
            component_list_item(
                "official-mock-text-v1",
                "Mock Text Template",
                "text_translation",
                vec!["text"],
                vec!["post_content"],
                vec!["plain_text", "rich_html"],
                vec![],
                "mock-text-vendor",
            ),
            component_list_item(
                "official-mock-image-v1",
                "Mock Image Template",
                "image_translation",
                vec!["image"],
                vec!["post_content"],
                vec!["media_ref"],
                vec!["png", "jpg", "jpeg"],
                "mock-image-vendor",
            ),
        ],
        MockScenario::AudioVideoDocument => vec![
            component_list_item(
                "official-mock-audio-v1",
                "Mock Audio Template",
                "audio_translation",
                vec!["audio"],
                vec!["post_content"],
                vec!["media_ref"],
                vec!["mp3"],
                "mock-audio-vendor",
            ),
            component_list_item(
                "official-mock-video-v1",
                "Mock Video Template",
                "video_translation",
                vec!["video"],
                vec!["post_content"],
                vec!["media_ref"],
                vec!["mp4"],
                "mock-video-vendor",
            ),
            component_list_item(
                "official-mock-document-v1",
                "Mock Document Template",
                "document_translation",
                vec!["document"],
                vec!["post_content"],
                vec!["media_ref"],
                vec!["pdf"],
                "mock-document-vendor",
            ),
        ],
        MockScenario::StructuredFormats => vec![component_list_item(
            "official-mock-structured-v1",
            "Mock Structured Template",
            "text_translation",
            vec!["text"],
            vec!["post_content"],
            vec!["rich_html", "json_structured"],
            vec![],
            "mock-structured-vendor",
        )],
        MockScenario::AuthModesTaskFlow => vec![
            component_list_item(
                "official-mock-auth-direct-v1",
                "Mock Direct Auth Template",
                "text_translation",
                vec!["text"],
                vec!["post_content"],
                vec!["plain_text", "rich_html"],
                vec![],
                "mock-auth-direct-vendor",
            ),
            component_list_item(
                "official-mock-auth-key-v1",
                "Mock Key Pool Template",
                "text_translation",
                vec!["text"],
                vec!["post_content"],
                vec!["json_structured"],
                vec![],
                "mock-auth-key-vendor",
            ),
            component_list_item(
                "official-mock-auth-oauth-v1",
                "Mock OAuth Template",
                "text_translation",
                vec!["text"],
                vec!["post_content"],
                vec!["serialized_php"],
                vec![],
                "mock-auth-oauth-vendor",
            ),
        ],
        MockScenario::WpSourceMixed => vec![
            component_list_item(
                "official-mock-text-v1",
                "Mock Text Template",
                "text_translation",
                vec!["text"],
                vec!["post_content"],
                vec!["plain_text", "rich_html"],
                vec![],
                "mock-text-vendor",
            ),
            component_list_item(
                "official-mock-structured-v1",
                "Mock Structured Template",
                "text_translation",
                vec!["text"],
                vec!["post_content"],
                vec![
                    "plain_text",
                    "rich_html",
                    "json_structured",
                    "serialized_php",
                ],
                vec![],
                "mock-structured-vendor",
            ),
            component_list_item(
                "official-mock-image-v1",
                "Mock Image Template",
                "image_translation",
                vec!["image"],
                vec!["post_content"],
                vec!["media_ref"],
                vec!["png", "jpg", "jpeg"],
                "mock-image-vendor",
            ),
        ],
        MockScenario::TaxonomyMixed => vec![
            component_list_item(
                "official-mock-text-v1",
                "Mock Text Template",
                "text_translation",
                vec!["text"],
                vec!["taxonomy_content"],
                vec!["plain_text", "rich_html"],
                vec![],
                "mock-text-vendor",
            ),
            component_list_item(
                "official-mock-image-v1",
                "Mock Image Template",
                "image_translation",
                vec!["image"],
                vec!["taxonomy_content"],
                vec!["media_ref"],
                vec!["png", "jpg", "jpeg"],
                "mock-image-vendor",
            ),
        ],
        MockScenario::LanguagePackPlugin => vec![component_list_item(
            "official-mock-text-v1",
            "Mock Text Template",
            "text_translation",
            vec!["text"],
            vec!["plugin_i18n"],
            vec!["plain_text"],
            vec![],
            "mock-text-vendor",
        )],
    }
}

fn rules_for_scenario(scenario: MockScenario) -> Vec<Value> {
    match scenario {
        MockScenario::TextImage => vec![json!({
            "id": 201,
            "model_id": 1,
            "name": "Mock Mixed Rule",
            "data_type": "post_type",
            "object_name": "post",
            "translate_fields": ["post_title", "post_html", "hero_media_id"],
            "field_capabilities": {},
            "related_taxonomies": [],
            "field_content_formats": {
                "post_title": "plain_text",
                "post_html": "rich_html",
                "hero_media_id": "media_ref"
            }
        })],
        MockScenario::AudioVideoDocument => vec![json!({
            "id": 202,
            "model_id": 1,
            "name": "Mock AV Document Rule",
            "data_type": "post_type",
            "object_name": "post",
            "translate_fields": ["audio_track_id", "video_clip_id", "document_file_id"],
            "field_capabilities": {},
            "related_taxonomies": [],
            "field_content_formats": {
                "audio_track_id": "media_ref",
                "video_clip_id": "media_ref",
                "document_file_id": "media_ref"
            }
        })],
        MockScenario::StructuredFormats => vec![json!({
            "id": 203,
            "model_id": 1,
            "name": "Mock Structured Rule",
            "data_type": "post_type",
            "object_name": "post",
            "translate_fields": ["post_html", "seo_json", "acf_group"],
            "field_capabilities": {},
            "related_taxonomies": [],
            "field_content_formats": {
                "post_html": "rich_html",
                "seo_json": "json_structured",
                "acf_group": "serialized_php"
            }
        })],
        MockScenario::AuthModesTaskFlow => vec![json!({
            "id": 206,
            "model_id": 1,
            "name": "Mock Auth Modes Rule",
            "data_type": "post_type",
            "object_name": "post",
            "translate_fields": ["post_title", "post_html", "seo_json", "acf_group"],
            "field_capabilities": {},
            "related_taxonomies": [],
            "field_content_formats": {
                "post_title": "plain_text",
                "post_html": "rich_html",
                "seo_json": "json_structured",
                "acf_group": "serialized_php"
            }
        })],
        MockScenario::WpSourceMixed => vec![json!({
            "id": 204,
            "model_id": 1,
            "name": "Mock WP Source Rule",
            "data_type": "post_type",
            "object_name": "product",
            "translate_fields": [
                "post_title",
                "post_content",
                "seo_schema",
                "acf_group",
                "hero_image_alt",
                "hero_media_id",
                "seo_slug",
                "template_code"
            ],
            "field_capabilities": {},
            "related_taxonomies": [],
            "field_content_formats": {
                "post_title": "plain_text",
                "post_content": "rich_html",
                "seo_schema": "json_structured",
                "acf_group": "serialized_php",
                "hero_image_alt": "media_ref",
                "hero_media_id": "media_ref",
                "seo_slug": "slug",
                "template_code": "code"
            },
            "field_storage_map": {
                "post_title": "post_column",
                "post_content": "post_column",
                "seo_schema": "post_meta",
                "acf_group": "post_meta",
                "hero_image_alt": "post_meta",
                "hero_media_id": "post_meta",
                "seo_slug": "post_meta",
                "template_code": "post_meta"
            }
        })],
        MockScenario::TaxonomyMixed => vec![json!({
            "id": 205,
            "model_id": 1,
            "name": "Mock Taxonomy Rule",
            "data_type": "taxonomy",
            "object_name": "product_cat",
            "translate_fields": [
                "name",
                "description",
                "term_image_alt",
                "term_image_id",
                "slug"
            ],
            "field_capabilities": {},
            "related_taxonomies": [],
            "field_content_formats": {
                "name": "plain_text",
                "description": "rich_html",
                "term_image_alt": "media_ref",
                "term_image_id": "media_ref",
                "slug": "slug"
            },
            "field_storage_map": {
                "name": "term_column",
                "description": "term_column",
                "term_image_alt": "term_meta",
                "term_image_id": "term_meta",
                "slug": "term_column"
            }
        })],
        MockScenario::LanguagePackPlugin => vec![],
    }
}

fn content_item_for_scenario(base_url: &str, scenario: MockScenario) -> Value {
    match scenario {
        MockScenario::TextImage => json!({
            "object_type": "post",
            "subtype": "post",
            "object_id": 501,
            "complete_data": {
                "post_title": "Hello from mock",
                "post_html": "<p>Hello <strong>world</strong></p>",
                "hero_media_id": 77,
                "hero_media_url": IMAGE_DATA_URL
            }
        }),
        MockScenario::AudioVideoDocument => json!({
            "object_type": "post",
            "subtype": "post",
            "object_id": 601,
            "complete_data": {
                "audio_track_id": 88,
                "audio_track_url": format!("{}{}", base_url, AUDIO_URL_PATH),
                "video_clip_id": 89,
                "video_clip_url": format!("{}{}", base_url, VIDEO_URL_PATH),
                "document_file_id": 90,
                "document_file_url": format!("{}{}", base_url, DOCUMENT_URL_PATH)
            }
        }),
        MockScenario::StructuredFormats => json!({
            "object_type": "post",
            "subtype": "post",
            "object_id": 701,
            "complete_data": {
                "post_html": "<p>Hello <strong>world</strong></p>",
                "seo_json": {
                    "headline": "Hello SEO",
                    "keywords": ["Alpha", "Beta"],
                    "meta": {
                        "summary": "Nested summary"
                    },
                    "count": 2,
                    "enabled": true
                },
                "acf_group": {
                    "headline": "Hello ACF",
                    "items": ["One", "Two"],
                    "enabled": true
                }
            }
        }),
        MockScenario::AuthModesTaskFlow => json!({
            "object_type": "post",
            "subtype": "post",
            "object_id": 751,
            "complete_data": {
                "post_title": "Direct auth title",
                "post_html": "<p>Direct <strong>content</strong></p>",
                "seo_json": {
                    "headline": "Key auth headline",
                    "keywords": ["Alpha", "Beta"]
                },
                "acf_group": {
                    "headline": "OAuth auth headline",
                    "items": ["One", "Two"]
                }
            }
        }),
        MockScenario::WpSourceMixed => json!({
            "object_type": "post",
            "subtype": "product",
            "object_id": 801,
            "complete_data": {
                "post": {
                    "post_title": "Hello product",
                    "post_content": "<section><p>Hello <strong>builder</strong></p></section>"
                },
                "meta": {
                    "seo_schema": {
                        "headline": "SEO headline",
                        "faq": [{
                            "q": "What is it?",
                            "a": "A translated answer"
                        }],
                        "rating": 5
                    },
                    "acf_group": {
                        "headline": "Feature headline",
                        "items": ["Fast", "Flexible"],
                        "enabled": true
                    },
                    "hero_image_alt": "Product hero alt",
                    "hero_media_id": 91,
                    "hero_media_url": format!("{}{}", base_url, WP_IMAGE_URL_PATH),
                    "cta_href": "https://example.com/pricing",
                    "gallery_srcset": "https://example.com/hero-640.jpg 640w, https://example.com/hero-1280.jpg 1280w",
                    "hero_poster": "https://example.com/poster.mp4",
                    "seo_slug": "hello-product",
                    "template_code": "hero-banner-v1"
                }
            }
        }),
        MockScenario::TaxonomyMixed => json!({
            "object_type": "term",
            "subtype": "product_cat",
            "object_id": 901,
            "complete_data": {
                "term": {
                    "name": "Summer Sale",
                    "description": "<p>Seasonal <strong>category</strong></p>",
                    "slug": "summer-sale"
                },
                "meta": {
                    "term_image_alt": "Seasonal category banner",
                    "term_image_id": 92,
                    "term_image_url": format!("{}{}", base_url, WP_IMAGE_URL_PATH)
                }
            }
        }),
        MockScenario::LanguagePackPlugin => {
            unreachable!("language pack uses content_items_for_scenario")
        }
    }
}

fn relation_models_for_scenario(scenario: MockScenario) -> Vec<Value> {
    match scenario {
        MockScenario::WpSourceMixed => vec![json!({
            "model_id": 1,
            "plugin_slug": "",
            "plugin_name": "",
            "post_types": ["product"],
            "taxonomies": []
        })],
        MockScenario::TaxonomyMixed => vec![json!({
            "model_id": 1,
            "plugin_slug": "",
            "plugin_name": "",
            "post_types": [],
            "taxonomies": ["product_cat"]
        })],
        MockScenario::LanguagePackPlugin => vec![json!({
            "model_id": 1,
            "plugin_slug": "mock-plugin",
            "plugin_name": "Mock Plugin",
            "post_types": [],
            "taxonomies": []
        })],
        _ => vec![json!({
            "model_id": 1,
            "plugin_slug": "",
            "plugin_name": "",
            "post_types": ["post"],
            "taxonomies": []
        })],
    }
}

fn relation_payload_for_scenario(scenario: MockScenario) -> Value {
    let mut relation = json!({
        "id": 101,
        "source_lang": "en_US",
        "target_lang": "zh_CN",
        "sync_mode": "manual",
        "target_site_type": "virtual",
        "models": relation_models_for_scenario(scenario)
    });
    if matches!(scenario, MockScenario::LanguagePackPlugin) {
        relation["i18n_config"] = json!({
            "translate_plugin_i18n": true,
            "translate_theme_i18n": false,
            "translate_config_i18n": false
        });
    }
    relation
}

fn content_items_for_scenario(
    base_url: &str,
    scenario: MockScenario,
    data_type: &str,
    subtype: &str,
    page: &str,
) -> Vec<Value> {
    if page != "1" {
        return Vec::new();
    }

    match scenario {
        MockScenario::LanguagePackPlugin => {
            if data_type != "language_pack" || subtype != "plugin" {
                return Vec::new();
            }
            vec![
                json!({
                    "object_id": 3001,
                    "text_domain": "mock-plugin",
                    "complete_data": {
                        "entry_id": 1001,
                        "msgid": "Save changes",
                        "msgctxt": "button label",
                        "msgid_plural": "",
                        "plural_index": 0,
                        "text_domain": "mock-plugin"
                    }
                }),
                json!({
                    "object_id": 3002,
                    "text_domain": "mock-plugin",
                    "complete_data": {
                        "entry_id": 1002,
                        "msgid": "%d file deleted",
                        "msgctxt": "cleanup notice",
                        "msgid_plural": "%d files deleted",
                        "plural_index": 1,
                        "text_domain": "mock-plugin"
                    }
                }),
            ]
        }
        MockScenario::TaxonomyMixed => {
            if data_type != "term" {
                return Vec::new();
            }
            vec![content_item_for_scenario(base_url, scenario)]
        }
        _ => {
            if data_type != "post" {
                return Vec::new();
            }
            vec![content_item_for_scenario(base_url, scenario)]
        }
    }
}

fn component_detail_for_scenario(scenario: MockScenario, component_id: &str) -> Option<Value> {
    component_catalog_for_scenario(scenario)
        .into_iter()
        .find(|component| component["id"] == json!(component_id))
}

fn component_list_item(
    id: &str,
    name: &str,
    kind: &str,
    supported_types: Vec<&str>,
    supported_business_lines: Vec<&str>,
    supported_content_formats: Vec<&str>,
    supported_formats: Vec<&str>,
    vendor_id: &str,
) -> Value {
    let task_kind = match kind {
        "image_translation" => "image",
        "video_translation" => "video",
        "audio_translation" => "audio",
        "document_translation" => "document",
        _ => "text",
    };
    let input_mode = if supported_content_formats
        .iter()
        .any(|fmt| *fmt == "media_ref")
    {
        if supported_content_formats
            .iter()
            .any(|fmt| *fmt != "media_ref")
        {
            "mixed"
        } else {
            "media_ref"
        }
    } else {
        "text"
    };
    let output_mode = match task_kind {
        "image" => "translated_image_ref",
        "video" => "translated_video_ref",
        "audio" => "translated_audio_ref",
        "document" => "translated_document_ref",
        _ => "translated_text",
    };
    json!({
        "id": id,
        "name": name,
        "owner_type": "official",
        "version": "1.0.0",
        "type": kind,
        "supported_types": supported_types,
        "supported_business_lines": supported_business_lines,
        "supported_content_formats": supported_content_formats,
        "supported_formats": supported_formats,
        "vendor_id": vendor_id,
        "api_version": "1.0.0",
        "status": "active",
        "updated_at": "2026-03-20T00:00:00Z",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": task_kind,
            "input_mode": input_mode,
            "workflow_mode": "sync",
            "output_mode": output_mode,
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
        },
        "translation_modes": [{
            "id": "plain_mode",
            "label": "Plain Text",
            "supported_content_formats": ["plain_text"]
        }, {
            "id": "html_mode",
            "label": "Rich HTML",
            "supported_content_formats": ["rich_html"]
        }]
    })
}

fn structured_text_template(base_url: &str) -> Value {
    json!({
        "id": "official-mock-structured-v1",
        "name": "Mock Structured Template",
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
            "supported_content_formats": ["plain_text", "rich_html", "json_structured", "serialized_php"],
            "max_input_chars": 8000
        },
        "translation_modes": [{
            "id": "plain_mode",
            "label": "Plain Text",
            "supported_content_formats": ["plain_text"]
        }, {
            "id": "html_mode",
            "label": "Rich HTML",
            "supported_content_formats": ["rich_html"]
        }, {
            "id": "json_mode",
            "label": "JSON Structured",
            "supported_content_formats": ["json_structured"]
        }, {
            "id": "php_mode",
            "label": "Serialized PHP",
            "supported_content_formats": ["serialized_php"]
        }]
    })
}

fn auth_text_template(
    id: &str,
    name: &str,
    base_url: &str,
    endpoint: &str,
    headers: Value,
    auth_fields: Value,
    supported_content_formats: Vec<&str>,
) -> Value {
    json!({
        "id": id,
        "name": name,
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
        "auth": { "fields": auth_fields },
        "request": {
            "method": "POST",
            "url": format!("{}/{}", base_url, endpoint.trim_start_matches('/')),
            "headers": headers,
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
            "supported_content_formats": supported_content_formats
        }
    })
}

fn direct_auth_text_template(base_url: &str) -> Value {
    auth_text_template(
        "official-mock-auth-direct-v1",
        "Mock Direct Auth Template",
        base_url,
        "/mock/vendor/auth/direct",
        json!({
            "Content-Type": "application/json",
            "X-Mock-Api-Key": "{{auth.api_key}}"
        }),
        json!([{
            "name": "api_key",
            "required": true
        }]),
        vec!["plain_text", "rich_html"],
    )
}

fn key_pool_text_template(base_url: &str) -> Value {
    auth_text_template(
        "official-mock-auth-key-v1",
        "Mock Key Pool Template",
        base_url,
        "/mock/vendor/auth/key",
        json!({
            "Content-Type": "application/json",
            "X-Mock-Api-Key": "{{auth.api_key}}"
        }),
        json!([]),
        vec!["json_structured"],
    )
}

fn oauth_text_template(base_url: &str) -> Value {
    auth_text_template(
        "official-mock-auth-oauth-v1",
        "Mock OAuth Template",
        base_url,
        "/mock/vendor/auth/oauth",
        json!({
            "Content-Type": "application/json",
            "Authorization": "Bearer {{auth.access_token}}"
        }),
        json!([]),
        vec!["rich_html", "json_structured", "serialized_php"],
    )
}

fn image_template(base_url: &str) -> Value {
    json!({
        "id": "official-mock-image-v1",
        "name": "Mock Image Template",
        "version": "1.0.0",
        "type": "image_translation",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": "image",
            "input_mode": "media_ref",
            "workflow_mode": "sync",
            "output_mode": "translated_image_ref",
            "stages": ["request"],
            "request_body_type": "json",
            "submit_response_type": "json",
            "result_transport": "inline_json"
        },
        "auth": { "fields": [] },
        "request": {
            "method": "POST",
            "url": format!("{}/mock/vendor/image", base_url),
            "headers": {
                "Content-Type": "application/json"
            },
            "body": {
                "source_ref": "{{input.source_ref}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            },
            "body_type": "json"
        },
        "response": {
            "translated_image_ref_path": "translated_ref",
            "translated_text_path": "translated_text"
        },
        "constraints": {
            "supported_content_formats": ["media_ref"],
            "supported_formats": ["png", "jpg", "jpeg"],
            "max_input_bytes": 1048576
        },
        "translation_modes": [{
            "id": "image-default",
            "label": "Default Image",
            "supported_content_formats": ["media_ref"]
        }]
    })
}

fn audio_template(base_url: &str) -> Value {
    json!({
        "id": "official-mock-audio-v1",
        "name": "Mock Audio Template",
        "version": "1.0.0",
        "type": "audio_translation",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": "audio",
            "input_mode": "media_ref",
            "workflow_mode": "sync",
            "output_mode": "translated_audio_ref",
            "stages": ["request"],
            "request_body_type": "json",
            "submit_response_type": "json",
            "result_transport": "inline_json"
        },
        "auth": { "fields": [] },
        "request": {
            "method": "POST",
            "url": format!("{}/mock/vendor/audio", base_url),
            "headers": {
                "Content-Type": "application/json"
            },
            "body": {
                "source_ref": "{{input.source_ref}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            },
            "body_type": "json"
        },
        "response": {
            "translated_audio_ref_path": "translated_ref",
            "translated_text_path": "translated_text"
        },
        "constraints": {
            "supported_content_formats": ["media_ref"],
            "supported_formats": ["mp3"],
            "max_input_bytes": 10485760
        }
    })
}

fn video_template(base_url: &str) -> Value {
    json!({
        "id": "official-mock-video-v1",
        "name": "Mock Video Template",
        "version": "1.0.0",
        "type": "video_translation",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": "video",
            "input_mode": "media_ref",
            "workflow_mode": "sync",
            "output_mode": "translated_video_ref",
            "stages": ["request"],
            "request_body_type": "json",
            "submit_response_type": "json",
            "result_transport": "inline_json"
        },
        "auth": { "fields": [] },
        "request": {
            "method": "POST",
            "url": format!("{}/mock/vendor/video", base_url),
            "headers": {
                "Content-Type": "application/json"
            },
            "body": {
                "source_ref": "{{input.source_ref}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            },
            "body_type": "json"
        },
        "response": {
            "translated_video_ref_path": "translated_ref",
            "translated_text_path": "translated_text"
        },
        "constraints": {
            "supported_content_formats": ["media_ref"],
            "supported_formats": ["mp4"],
            "max_input_bytes": 104857600
        }
    })
}

fn document_template(base_url: &str) -> Value {
    json!({
        "id": "official-mock-document-v1",
        "name": "Mock Document Template",
        "version": "1.0.0",
        "type": "document_translation",
        "client_contract": {
            "schema_version": "component-client-contract-v1",
            "task_kind": "document",
            "input_mode": "media_ref",
            "workflow_mode": "sync",
            "output_mode": "translated_document_ref",
            "stages": ["request"],
            "request_body_type": "json",
            "submit_response_type": "json",
            "result_transport": "inline_json"
        },
        "auth": { "fields": [] },
        "request": {
            "method": "POST",
            "url": format!("{}/mock/vendor/document", base_url),
            "headers": {
                "Content-Type": "application/json"
            },
            "body": {
                "source_ref": "{{input.source_ref}}",
                "source_lang": "{{input.source_lang}}",
                "target_lang": "{{input.target_lang}}"
            },
            "body_type": "json"
        },
        "response": {
            "translated_document_ref_path": "translated_ref",
            "translated_text_path": "translated_text"
        },
        "constraints": {
            "supported_content_formats": ["media_ref"],
            "supported_formats": ["pdf"],
            "max_input_bytes": 10485760
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

async fn write_binary_response(
    socket: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        content_type,
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.write_all(body).await?;
    Ok(())
}
