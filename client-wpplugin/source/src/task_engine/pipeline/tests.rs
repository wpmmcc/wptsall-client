use super::*;
use crate::component_rt::proxy::ProxyClientPool;
use crate::types::{
    ComponentConstraints, ComponentRequest, ComponentResponse, ComponentRuntime, ComponentTemplate,
    EffectiveConstraints, ProxyProfile,
};
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};

// -----------------------------------------------------------------------
// Helpers shared by the content-format tests
// -----------------------------------------------------------------------

fn empty_rule() -> DiscoveredRule {
    DiscoveredRule {
        id: 1,
        model_id: 1,
        name: "test".to_string(),
        data_type: "post".to_string(),
        object_name: "post".to_string(),
        field_capabilities: json!({}),
        translate_fields: Vec::new(),
        related_taxonomies: Vec::new(),
        field_content_formats: HashMap::new(),
        field_storage_map: HashMap::new(),
        source_group: String::new(),
        routing_profile: String::new(),
        delivery_target: String::new(),
        required_component_slots: Vec::new(),
        required_content_formats: Vec::new(),
        field_source_roles: HashMap::new(),
    }
}

#[test]
fn fse_synthetic_rule_routes_block_content_by_post_type() {
    let template = ContentItem {
        object_type: "post_type".to_string(),
        subtype: "wp_template".to_string(),
        object_id: 11,
        needs_resync: true,
        mapping_id: None,
        complete_data: json!({}),
    };
    let global_styles = ContentItem {
        subtype: "wp_global_styles".to_string(),
        ..template.clone()
    };

    let template_rule = build_fse_synthetic_rule(&template).expect("template rule");
    assert_eq!(template_rule.data_type, "post");
    assert_eq!(
        template_rule.field_content_formats["post_content"],
        "rich_html"
    );
    assert_eq!(
        template_rule.field_storage_map["post_content"],
        "post_column"
    );

    let styles_rule = build_fse_synthetic_rule(&global_styles).expect("global styles rule");
    assert_eq!(
        styles_rule.field_content_formats["post_content"],
        "json_structured"
    );
}

#[test]
fn non_fse_objects_do_not_receive_synthetic_rule() {
    let item = ContentItem {
        object_type: "post_type".to_string(),
        subtype: "post".to_string(),
        object_id: 12,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({}),
    };
    assert!(build_fse_synthetic_rule(&item).is_none());
}

#[tokio::test]
async fn fse_without_relation_rule_enters_component_pipeline_and_keeps_blocks() {
    let client = Client::new();
    let port = start_echo_server().await;
    let runtime = echo_runtime_with_kind_and_formats(
        "comp-fse-echo",
        "text",
        port,
        vec!["plain_text".to_string(), "rich_html".to_string()],
    );
    let mut runtimes = HashMap::new();
    runtimes.insert("comp-fse-echo".to_string(), runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-fse-echo".to_string()],
    };
    let item = ContentItem {
        object_type: "post_type".to_string(),
        subtype: "wp_template".to_string(),
        object_id: 13,
        needs_resync: true,
        mapping_id: None,
        complete_data: json!({
            "post": {
                "post_title": "FSE Home",
                "post_content": "<!-- wp:paragraph --><p>Hero block</p><!-- /wp:paragraph -->"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "wp"
    }))
    .unwrap();
    let wc = test_worker_config();
    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("synthetic FSE rule should translate");
    let (payload, _) = result.expect("FSE object should not be a no-op");
    assert_eq!(
        payload
            .translated_fields
            .get("post_content")
            .map(String::as_str),
        Some("<!-- wp:paragraph --><p>Hero block</p><!-- /wp:paragraph -->")
    );
    assert!(payload
        .field_results
        .iter()
        .any(|row| row.field == "post_content" && row.content_format == "rich_html"));
}

#[tokio::test]
async fn fse_global_styles_are_copied_without_translating_style_tokens() {
    let item = ContentItem {
        object_type: "post_type".to_string(),
        subtype: "wp_global_styles".to_string(),
        object_id: 14,
        needs_resync: true,
        mapping_id: None,
        complete_data: json!({
            "post": {
                "post_title": "",
                "post_content": "{\"version\":3,\"styles\":{\"color\":{\"text\":\"#ff0000\"}},\"settings\":{\"custom\":{\"css\":\".hero { color: red; }\"}}}"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "wp"
    }))
    .unwrap();
    let wc = test_worker_config();
    let result = translate_item_fields(
        &Client::new(),
        "https://example.com",
        &item,
        &relation,
        &[],
        None,
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("structure-preserving global-style copy should not need a provider");
    let (payload, _) = result.expect("global styles should produce a copy callback");
    assert_eq!(
        payload.translated_fields.get("post_content").map(String::as_str),
        Some("{\"version\":3,\"styles\":{\"color\":{\"text\":\"#ff0000\"}},\"settings\":{\"custom\":{\"css\":\".hero { color: red; }\"}}}")
    );
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "post_content")
        .expect("global-style field result");
    assert_eq!(row.transform_stage, "fse_global_styles_structure_preserved");
    assert_eq!(row.provider_component, "");
}

#[tokio::test]
async fn translate_item_fields_with_proxy_pool_uses_runtime_proxy_profile() {
    let _allowlist = crate::db::TestEnvVarGuard::set("WPTSALL_PROVIDER_ALLOWLIST", "provider.test");
    let (proxy_port, request_rx) = start_component_proxy_fixture().await;

    let mut proxy_profiles = HashMap::new();
    proxy_profiles.insert(
        "main-proxy".to_string(),
        ProxyProfile {
            name: "main".to_string(),
            protocol: "http".to_string(),
            host: "127.0.0.1".to_string(),
            port: proxy_port,
            username: String::new(),
            password: String::new(),
            enabled: true,
        },
    );
    let proxy_pool = ProxyClientPool::new(&proxy_profiles).expect("proxy pool should build");

    let mut runtime = echo_runtime_with_kind_and_formats(
        "comp-proxy-text",
        "text",
        proxy_port,
        vec!["plain_text".to_string()],
    );
    runtime.proxy_profile_id = Some("main-proxy".to_string());
    runtime.template.request.url = "http://provider.test/translate".to_string();
    runtime.template.response.translated_text_path = Some("text".to_string());

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-proxy-text".to_string(), runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-proxy-text".to_string()],
    };

    let client = Client::new();
    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 43,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title": "Hello"}),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 11,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title"],
        "field_capabilities": {
            "post_title": {"type": "translate", "enabled": true, "content_format": "plain_text"}
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields_with_trace_using_proxy(
        &client,
        Some(&proxy_pool),
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("proxy-backed translation should succeed");

    let trace = result.expect("translation should produce a payload");
    assert_eq!(
        trace
            .payload
            .translated_fields
            .get("post_title")
            .map(String::as_str),
        Some("proxied title")
    );
    let captured = request_rx.await.expect("proxy should capture request");
    assert!(
        captured.starts_with("POST http://provider.test/translate HTTP/1.1\r\n"),
        "proxy should receive absolute-form request"
    );
    assert!(
        captured.contains("\"text\":\"Hello\""),
        "translated body should reach the proxy"
    );
}

/// Start a local HTTP server that echoes back the "text" field from the
/// JSON request body verbatim. Returns the port it is listening on.
async fn start_echo_server() -> u16 {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut reader = BufReader::new(socket);

                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if lower.starts_with("content-length:") {
                        content_length =
                            lower["content-length:".len()..].trim().parse().unwrap_or(0);
                    }
                }

                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body).await;
                }

                let text = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["text"].as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let resp_body = serde_json::to_string(&serde_json::json!({"text": text})).unwrap();
                let http_resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                let _ = reader.get_mut().write_all(http_resp.as_bytes()).await;
            });
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    port
}

async fn start_component_proxy_fixture() -> (u16, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (request_tx, request_rx) = oneshot::channel::<String>();

    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let Ok(read_result) = timeout(Duration::from_secs(5), socket.read(&mut chunk)).await
            else {
                return;
            };
            let Ok(read) = read_result else {
                return;
            };
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if http_request_is_complete(&request) {
                break;
            }
        }

        let captured = String::from_utf8_lossy(&request).to_string();
        let _ = request_tx.send(captured);

        let resp_body = serde_json::to_string(&json!({"text": "proxied title"})).unwrap();
        let http_resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        let _ = socket.write_all(http_resp.as_bytes()).await;
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    (port, request_rx)
}

fn http_request_is_complete(request: &[u8]) -> bool {
    let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let header_text = String::from_utf8_lossy(&request[..header_end + 4]);
    let content_length = header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    });
    match content_length {
        Some(length) => request.len() >= header_end + 4 + length,
        None => true,
    }
}

#[test]
fn derive_business_line_from_rule_routes_config_object_to_config_i18n() {
    let mut rule = empty_rule();
    rule.data_type = "option".to_string();
    rule.source_group = "config_object".to_string();
    rule.routing_profile = "config_i18n".to_string();

    assert_eq!(
        derive_business_line_from_rule(Some(&rule), "option"),
        "config_i18n"
    );
}

#[test]
fn derive_business_line_from_rule_keeps_message_template_on_coarse_lane() {
    let mut rule = empty_rule();
    rule.data_type = "option".to_string();
    rule.source_group = "message_template".to_string();
    rule.routing_profile = "notification_email".to_string();

    assert_eq!(
        derive_business_line_from_rule(Some(&rule), "option"),
        "custom_model"
    );
}

/// Build a `ComponentRuntime` whose request URL points to a local echo server.
fn echo_runtime_with_kind_and_formats(
    id: &str,
    kind: &str,
    port: u16,
    supported_content_formats: Vec<String>,
) -> ComponentRuntime {
    echo_runtime_with_kind_formats_and_ref_path(id, kind, port, supported_content_formats, None)
}

fn echo_runtime_with_kind_formats_and_ref_path(
    id: &str,
    kind: &str,
    port: u16,
    supported_content_formats: Vec<String>,
    translated_ref_path: Option<&str>,
) -> ComponentRuntime {
    ComponentRuntime {
        template: ComponentTemplate {
            id: id.to_string(),
            name: "Echo".to_string(),
            version: "1.0".to_string(),
            kind: kind.to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("http://127.0.0.1:{}", port),
                headers: None,
                body: Some(serde_json::json!({"text": "{{input.text}}"})),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: Some("text".to_string()),
                error_path: None,
                translated_ref_path: translated_ref_path.map(|s| s.to_string()),
                translated_media_ref_path: None,
                translated_image_ref_path: None,
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec![],
        language_map: HashMap::new(),
        supported_content_formats,
        supported_formats: vec![],
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

fn media_runtime_with_artifacts(
    id: &str,
    kind: &str,
    port: u16,
    input_artifact_kind: &str,
    output_artifact_kinds: &[&str],
) -> ComponentRuntime {
    let mut runtime = ComponentRuntime {
        template: ComponentTemplate {
            id: id.to_string(),
            name: "MediaEcho".to_string(),
            version: "1.0".to_string(),
            kind: kind.to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("http://127.0.0.1:{}", port),
                headers: None,
                body: Some(serde_json::json!({"text": "{{input.source_ref}}"})),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: None,
                error_path: None,
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: Some("text".to_string()),
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec![],
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: vec![],
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };
    runtime.template.constraints = Some(ComponentConstraints {
        input_artifact_kind: Some(input_artifact_kind.to_string()),
        output_artifact_kinds: Some(
            output_artifact_kinds
                .iter()
                .map(|value| value.to_string())
                .collect(),
        ),
        ..Default::default()
    });
    runtime
}

fn echo_runtime_with_formats(
    id: &str,
    port: u16,
    supported_content_formats: Vec<String>,
) -> ComponentRuntime {
    echo_runtime_with_kind_and_formats(id, "text", port, supported_content_formats)
}

fn echo_runtime(port: u16) -> ComponentRuntime {
    echo_runtime_with_formats("test-echo", port, vec![])
}

fn no_constraints() -> EffectiveConstraints {
    EffectiveConstraints {
        max_input_chars: 0,
        split_strategy: "none".to_string(),
        split_separator: String::new(),
        rate_limit_qps: 0,
        max_concurrent_requests: 0,
    }
}

#[test]
fn infer_storage_fallback_returns_option_value_for_option_objects() {
    assert_eq!(
        infer_storage_fallback("option", "email_body"),
        "option_value"
    );
    assert_eq!(
        infer_storage_fallback("option", "option_value"),
        "option_value"
    );
}

#[test]
fn get_authoritative_field_value_reads_option_fields_from_root_fallback() {
    let complete_data = serde_json::json!({
        "option": {
            "option_name": "wptsall_message_template",
            "option_value": {
                "email_subject": "Original subject",
                "email_body": "<p>Original body</p>"
            }
        },
        "email_subject": "Translated subject",
        "email_body": "<div><strong>Translated</strong> body</div>"
    });
    let complete_data = complete_data.as_object().expect("object");

    assert_eq!(
        get_authoritative_field_value(complete_data, "email_subject", "option_value")
            .and_then(|value| value.as_str()),
        Some("Translated subject")
    );
    assert_eq!(
        get_authoritative_field_value(complete_data, "email_body", "option_value")
            .and_then(|value| value.as_str()),
        Some("<div><strong>Translated</strong> body</div>")
    );
}

// -----------------------------------------------------------------------
// sanitize_domain_key
// -----------------------------------------------------------------------

#[test]
fn sanitize_domain_key_https() {
    assert_eq!(
        sanitize_domain_key("https://blog.example.com"),
        "https-blog.example.com"
    );
}

#[test]
fn sanitize_domain_key_http_with_port() {
    assert_eq!(
        sanitize_domain_key("http://localhost:8080"),
        "http-localhost-8080"
    );
}

#[test]
fn sanitize_domain_key_with_path() {
    assert_eq!(
        sanitize_domain_key("https://example.com/wp"),
        "https-example.com"
    );
}

#[test]
fn sanitize_domain_key_preserves_dots() {
    let key = sanitize_domain_key("https://sub.domain.example.com");
    assert!(key.contains('.'));
    assert!(!key.contains('/'));
    assert!(!key.contains(':'));
}

// -----------------------------------------------------------------------
// build_translated_path
// -----------------------------------------------------------------------

#[test]
fn build_translated_path_standard() {
    let raw = "./data/raw/https-example.com/rel_1/post_42.json";
    let result = build_translated_path(raw, "./data", "https-example.com");
    assert_eq!(
        result,
        "./data/translated/https-example.com/rel_1/post_42.json"
    );
}

#[test]
fn build_translated_path_fallback() {
    let raw = "/some/other/path/file.json";
    let result = build_translated_path(raw, "./data", "example");
    assert_eq!(result, "/some/other/path/file.json.translated");
}

// -----------------------------------------------------------------------
// ensure_db
// -----------------------------------------------------------------------

#[tokio::test]
async fn ensure_db_creates_in_memory_when_none() {
    let db = ensure_db(None);
    let conn = db.lock().await;
    // Verify the DB has tables (open_db runs migrations)
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0, "in-memory DB should have tables");
}

#[test]
fn ensure_db_returns_existing_when_some() {
    let conn = crate::db::open_db(":memory:").unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(conn));
    let result = ensure_db(Some(Arc::clone(&db)));
    assert!(Arc::ptr_eq(&result, &db));
}

// -----------------------------------------------------------------------
// persist_raw_content
// -----------------------------------------------------------------------

#[tokio::test]
async fn persist_raw_content_writes_file_and_updates_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let raw_path = dir
        .path()
        .join("raw")
        .join("domain")
        .join("rel_1")
        .join("post_42.json");
    let raw_path_str = raw_path.to_str().unwrap();

    let db = ensure_db(None);
    // Create a job + item so we have a valid item_db_id
    let item_db_id = {
        let conn = db.lock().await;
        conn.execute(
                "INSERT INTO translation_jobs (domain, relation_id, business_line, status, created_at, updated_at)
                 VALUES ('test', 1, 'post_content', 'running', 0, 0)",
                [],
            ).unwrap();
        let job_id: i64 = conn.last_insert_rowid();
        crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "test".to_string(),
                relation_id: 1,
                business_line: "post_content".to_string(),
                object_type: "post".to_string(),
                wp_object_id: 42,
                wp_object_subtype: "".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "".to_string(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: raw_path_str.to_string(),
                client_task_id: "test-42".to_string(),
                max_retries: 3,
            },
        )
        .unwrap()
    };

    let content = json!({"post_title": "Hello", "post_content": "<p>World</p>"});
    persist_raw_content(&db, item_db_id, &content, raw_path_str, "/dev/null")
        .await
        .unwrap();

    // Verify file exists
    assert!(raw_path.exists());
    let written = std::fs::read_to_string(&raw_path).unwrap();
    assert!(written.contains("Hello"));

    // Verify DB status
    let status: String = {
        let conn = db.lock().await;
        conn.query_row(
            "SELECT status FROM translation_items WHERE id = ?1",
            [item_db_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(status, "fetched");
}

// -----------------------------------------------------------------------
// persist_translated
// -----------------------------------------------------------------------

#[tokio::test]
async fn persist_translated_writes_envelope_and_updates_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    let translated_path = dir.path().join("translated").join("post_42.json");
    let translated_path_str = translated_path.to_str().unwrap();

    let db = ensure_db(None);
    let item_db_id = {
        let conn = db.lock().await;
        conn.execute(
                "INSERT INTO translation_jobs (domain, relation_id, business_line, status, created_at, updated_at)
                 VALUES ('test', 1, 'post_content', 'running', 0, 0)",
                [],
            ).unwrap();
        let job_id: i64 = conn.last_insert_rowid();
        let id = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "test".to_string(),
                relation_id: 1,
                business_line: "post_content".to_string(),
                object_type: "post".to_string(),
                wp_object_id: 42,
                wp_object_subtype: "".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "".to_string(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: "/tmp/raw.json".to_string(),
                client_task_id: "test-42".to_string(),
                max_retries: 3,
            },
        )
        .unwrap();
        let _ = crate::db::jobs::update_item_status(&conn, id, "fetched", None);
        id
    };

    let payload = TranslationCallbackPayload {
        schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
        attempt_id: "att-test-1".to_string(),
        object_snapshot_hash: "hash-test-1".to_string(),
        source_revision: String::new(),
        policy_version: String::new(),
        field_results: Vec::new(),
        relation_id: 1,
        business_line: "post_content".to_string(),
        object_type: "post_type".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        translated_fields: [("post_title".to_string(), "你好".to_string())].into(),
        translated_meta: HashMap::new(),
        media_mappings: Vec::new(),
        media_field_sources: HashMap::new(),
        client_task_id: "test-42".to_string(),
        outbox_id: None,
        worker_id: "w1".to_string(),
        source_lang: "en".to_string(),
        target_lang: "zh".to_string(),
        execution_time_ms: 100,
    };

    persist_translated(
        &db,
        item_db_id,
        &payload,
        "idem-key-1",
        Some("secret123"),
        translated_path_str,
        "/dev/null",
    )
    .await
    .unwrap();

    // Verify file exists and contains envelope
    assert!(translated_path.exists());
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&translated_path).unwrap()).unwrap();
    assert_eq!(written["idempotency_key"], "idem-key-1");
    assert!(written["payload"]["translated_fields"]["post_title"]
        .as_str()
        .unwrap()
        .contains("你好"));

    // Verify DB status and translated_path
    let (status, tp): (String, String) = {
        let conn = db.lock().await;
        conn.query_row(
            "SELECT status, translated_path FROM translation_items WHERE id = ?1",
            [item_db_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(status, "translated");
    assert_eq!(tp, translated_path_str);
}

// -----------------------------------------------------------------------
// translate_serialized_php
// -----------------------------------------------------------------------

#[tokio::test]
async fn serialized_php_rejects_non_php_prefix() {
    let client = Client::new();
    let comp = echo_runtime(9);
    let c = no_constraints();

    let err = translate_serialized_php(
        &client,
        &comp,
        "not serialized at all",
        "en",
        "zh",
        &c,
        "/dev/null",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("does not look like PHP"),
        "unexpected error: {}",
        err
    );
}

#[tokio::test]
async fn serialized_php_passthrough_preserves_simple_string() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let php = r#"s:5:"hello";"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert_eq!(result, php, "passthrough should preserve input");
}

#[tokio::test]
async fn serialized_php_updates_byte_length_after_translation() {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut reader = BufReader::new(socket);
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if lower.starts_with("content-length:") {
                        content_length =
                            lower["content-length:".len()..].trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body).await;
                }
                let text = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["text"].as_str().map(|s| format!("X:{}", s)))
                    .unwrap_or_default();
                let resp = serde_json::to_string(&serde_json::json!({"text": text})).unwrap();
                let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp.len(), resp
                    );
                let _ = reader.get_mut().write_all(http.as_bytes()).await;
            });
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let php = r#"s:2:"hi";"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert!(
        result.contains(r#"s:4:"X:hi""#),
        "length should update to 4: got {}",
        result
    );
}

#[tokio::test]
async fn serialized_php_preserves_non_string_tokens() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let php = r#"a:1:{i:0;s:3:"foo";}"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert!(result.starts_with("a:1:{"), "array marker lost: {}", result);
    assert!(result.contains("i:0;"), "integer token lost: {}", result);
    assert!(
        result.contains(r#"s:3:"foo""#),
        "string token lost: {}",
        result
    );
}

// -----------------------------------------------------------------------
// SEM-SERIALIZED-KEY L3 oracles (catalog content-semantics; plan §7
// TEST-CONTENT-SEMANTICS-001). Key immutability is pinned known_red
// (BUG-SERIALIZED-KEY-MIXING): array keys currently get machine-translated
// too. Structure/nested/malformed invariants below are hard asserts.
// -----------------------------------------------------------------------

/// Start a local HTTP server that prefixes every translated "text" with the
/// given marker so key-vs-value mixing is observable in the output.
async fn start_prefix_echo_server(prefix: &'static str) -> u16 {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut reader = BufReader::new(socket);
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if lower.starts_with("content-length:") {
                        content_length =
                            lower["content-length:".len()..].trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body).await;
                }
                let text = serde_json::from_slice::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v["text"].as_str().map(|s| format!("{}{}", prefix, s)))
                    .unwrap_or_default();
                let resp = serde_json::to_string(&serde_json::json!({"text": text})).unwrap();
                let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp.len(),
                        resp
                    );
                let _ = reader.get_mut().write_all(http.as_bytes()).await;
            });
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    port
}

#[tokio::test]
async fn serialized_php_key_mixing_pin_known_red() {
    // KNOWN-RED PIN — BUG-SERIALIZED-KEY-MIXING (content-semantics
    // SEM-SERIALIZED-KEY; plan §7 TEST-CONTENT-SEMANTICS-001). The scanner
    // translates every s: token including array KEYS, so a host plugin's
    // serialized option/map structure is corrupted (the key side must be
    // immutable; only the value side may change). This pins today's broken
    // behavior: both the key "title" and the value "Hero" come back marked.
    // When the parser is fixed this pin FAILS — flip it to assert the keys
    // round-trip byte-identically (s:5:"title" / s:11:"description").
    let port = start_prefix_echo_server("T:").await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();
    let php = r#"a:2:{s:5:"title";s:4:"Hero";s:11:"description";s:4:"Text";}"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert!(
        result.contains(r#"s:7:"T:title""#),
        "KNOWN-RED pin (BUG-SERIALIZED-KEY-MIXING): expected the array key to be mixed today; got {} — if keys are now immutable the product lane fixed the parser: flip this pin into the key-immutability assert",
        result
    );
    assert!(
        result.contains(r#"s:6:"T:Hero""#),
        "value side should translate (same root behavior, kept explicit): {}",
        result
    );
    assert!(
        result.contains(r#"s:13:"T:description""#),
        "second key should also be mixed today: {}",
        result
    );
}

#[tokio::test]
async fn serialized_php_nested_array_round_trip_structure() {
    // Hard invariant (SEM-SERIALIZED-KEY structure side): nested arrays keep
    // every structural token byte-identically under identity translation.
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();
    let php = r#"a:2:{s:3:"key";a:1:{i:0;s:3:"foo";}i:1;s:3:"bar";}"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert_eq!(
        result, php,
        "nested array structure must round-trip byte-identically"
    );
}

#[tokio::test]
async fn serialized_php_malformed_length_token_passes_through() {
    // Hard invariant (SEM-SERIALIZED-KEY malformed side): a declared length
    // that exceeds the payload must not panic, rewrite or truncate — the
    // malformed token passes through verbatim.
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();
    let php = r#"s:10:"abc";"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert_eq!(
        result, php,
        "malformed length token must pass through unchanged"
    );
}

#[tokio::test]
async fn serialized_php_midstream_garbage_passes_through() {
    // Hard invariant (SEM-SERIALIZED-KEY malformed side): non-token garbage
    // inside a serialized payload is copied through, never silently dropped.
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();
    let php = r#"a:1:{garbage}"#;
    let result = translate_serialized_php(&client, &comp, php, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    assert_eq!(result, php, "garbage must pass through unchanged");
}

// -----------------------------------------------------------------------
// translate_json_structured
// -----------------------------------------------------------------------

#[tokio::test]
async fn json_structured_rejects_invalid_json() {
    let client = Client::new();
    let comp = echo_runtime(9);
    let c = no_constraints();

    let err = translate_json_structured(
        &client,
        &comp,
        "definitely not json",
        "en",
        "zh",
        &c,
        "/dev/null",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("failed to parse JSON"),
        "unexpected error: {}",
        err
    );
}

#[tokio::test]
async fn json_structured_preserves_non_string_values() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let input = r#"{"count":42,"enabled":true,"nothing":null,"title":"hello"}"#;
    let result = translate_json_structured(&client, &comp, input, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["count"], json!(42), "number changed");
    assert_eq!(parsed["enabled"], json!(true), "bool changed");
    assert_eq!(parsed["nothing"], json!(null), "null changed");
    assert_eq!(parsed["title"], json!("hello"), "string changed by echo");
}

#[tokio::test]
async fn json_structured_translates_nested_strings() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let input = r#"{"outer":{"inner":"deep"},"arr":["a","b"]}"#;
    let result = translate_json_structured(&client, &comp, input, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        parsed["outer"]["inner"],
        json!("deep"),
        "nested string echoed"
    );
    assert_eq!(parsed["arr"][0], json!("a"), "array[0] echoed");
    assert_eq!(parsed["arr"][1], json!("b"), "array[1] echoed");
}

#[tokio::test]
async fn json_structured_technical_fields_pin_known_red() {
    // KNOWN-RED PIN — client-side half of SEM-JSON-TECH-FIELDS (catalog
    // content-semantics; plan §7 TEST-CONTENT-SEMANTICS-001; gap-analysis
    // SEM-01 "部分成立": the WP Elementor path is protected by
    // Widget_Schema, but the generic client pipeline has no key context and
    // machine-translates EVERY string value, including technical
    // discriminators (id / elType / widgetType). This pins today's broken
    // behavior with a mutating provider: the id comes back marked. When a
    // technical-field whitelist lands (Content_Format_Registry owns it),
    // this pin FAILS — flip it to assert id/elType/widgetType are
    // byte-identical while human strings translate.
    let port = start_prefix_echo_server("T:").await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();
    let input = r#"{"id":"m-123","elType":"widget","widgetType":"heading","settings":{"title":"Hero"}}"#;
    let result = translate_json_structured(&client, &comp, input, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        parsed["id"], json!("T:m-123"),
        "KNOWN-RED pin (SEM-JSON-TECH-FIELDS client side): the technical id is machine-translated today; if it is now unchanged the whitelist landed — flip this pin"
    );
    assert_eq!(
        parsed["settings"]["title"], json!("T:Hero"),
        "human-readable string should translate"
    );
}
#[tokio::test]
async fn json_structured_empty_strings_not_translated() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let input = r#"{"empty":"","spaces":"   "}"#;
    let result = translate_json_structured(&client, &comp, input, "en", "zh", &c, "/dev/null")
        .await
        .unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(
        parsed["empty"],
        json!(""),
        "empty string should be preserved"
    );
}

#[tokio::test]
async fn json_structured_deep_nesting_fails_fast() {
    let client = Client::new();
    let comp = echo_runtime(9);
    let c = no_constraints();

    let mut value = json!("leaf");
    for _ in 0..(MAX_JSON_TRANSLATION_DEPTH + 50) {
        value = json!({ "n": value });
    }
    let input = value.to_string();

    let err = translate_json_structured(&client, &comp, &input, "en", "zh", &c, "/dev/null")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("nesting too deep") || msg.contains("failed to parse JSON"),
        "unexpected error: {}",
        msg
    );
}

// -----------------------------------------------------------------------
// extract_translate_fields
// -----------------------------------------------------------------------

#[test]
fn extract_translate_fields_explicit_array() {
    let caps = json!({
        "translate_fields": ["post_title", "post_content", "post_excerpt"]
    });
    let fields = extract_translate_fields(&caps);
    assert_eq!(fields, vec!["post_title", "post_content", "post_excerpt"]);
}

#[test]
fn extract_translate_fields_action_map() {
    let caps = json!({
        "post_title": "translate",
        "post_content": "translate",
        "post_name": "copy",
        "guid": "ignore"
    });
    let fields = extract_translate_fields(&caps);
    assert!(fields.contains(&"post_title".to_string()));
    assert!(fields.contains(&"post_content".to_string()));
    assert!(!fields.contains(&"post_name".to_string()));
    assert!(!fields.contains(&"guid".to_string()));
}

#[test]
fn extract_translate_fields_object_action_map() {
    let caps = json!({
        "post_title": { "type": "translate", "enabled": true, "content_format": "plain_text" },
        "post_content": { "type": "translate", "enabled": true, "content_format": "rich_html" },
        "post_excerpt": { "type": "translate", "enabled": true, "content_format": "plain_text" },
        "post_name": { "type": "compute", "enabled": true },
        "post_status": { "type": "sync", "enabled": true },
        "_price": { "type": "skip" },
        "_disabled_field": { "type": "translate", "enabled": false, "content_format": "plain_text" }
    });
    let fields = extract_translate_fields(&caps);
    assert!(fields.contains(&"post_title".to_string()));
    assert!(fields.contains(&"post_content".to_string()));
    assert!(fields.contains(&"post_excerpt".to_string()));
    assert!(!fields.contains(&"post_name".to_string()));
    assert!(!fields.contains(&"post_status".to_string()));
    assert!(!fields.contains(&"_price".to_string()));
    assert!(!fields.contains(&"_disabled_field".to_string()));
}

#[test]
fn extract_translate_fields_empty() {
    let caps = json!(null);
    let fields = extract_translate_fields(&caps);
    assert!(fields.is_empty());
}

#[test]
fn extract_translate_fields_empty_object() {
    let caps = json!({});
    let fields = extract_translate_fields(&caps);
    assert!(fields.is_empty());
}

#[test]
fn extract_translate_fields_prefers_explicit_over_map() {
    let caps = json!({
        "translate_fields": ["post_title"],
        "post_content": "translate"
    });
    let fields = extract_translate_fields(&caps);
    assert_eq!(fields, vec!["post_title"]);
}

// -----------------------------------------------------------------------
// content_format safe degradation (I7)
// -----------------------------------------------------------------------

#[test]
fn safe_content_format_known_values() {
    let log = "/dev/null";
    assert_eq!(
        get_safe_content_format("plain_text", "f", log).unwrap(),
        "plain_text"
    );
    assert_eq!(
        get_safe_content_format("rich_html", "f", log).unwrap(),
        "rich_html"
    );
    assert_eq!(
        get_safe_content_format("serialized_php", "f", log).unwrap(),
        "serialized_php"
    );
    assert_eq!(
        get_safe_content_format("json_structured", "f", log).unwrap(),
        "json_structured"
    );
    assert_eq!(get_safe_content_format("slug", "f", log).unwrap(), "slug");
    assert_eq!(get_safe_content_format("code", "f", log).unwrap(), "code");
    assert_eq!(
        get_safe_content_format("media_ref", "f", log).unwrap(),
        "media_ref"
    );
}

#[test]
fn safe_content_format_case_insensitive() {
    let log = "/dev/null";
    assert_eq!(
        get_safe_content_format("Rich_HTML", "f", log).unwrap(),
        "rich_html"
    );
    assert_eq!(
        get_safe_content_format("PLAIN_TEXT", "f", log).unwrap(),
        "plain_text"
    );
}

#[test]
fn safe_content_format_unknown_is_rejected() {
    let log = "/dev/null";
    assert!(get_safe_content_format("fancy_format", "f", log).is_err());
    assert!(get_safe_content_format("xml_structured", "f", log).is_err());
}

#[test]
fn safe_content_format_empty_is_plain_text() {
    let log = "/dev/null";
    assert_eq!(get_safe_content_format("", "f", log).unwrap(), "plain_text");
    assert_eq!(
        get_safe_content_format("  ", "f", log).unwrap(),
        "plain_text"
    );
}

#[test]
fn task_type_for_content_format_mapping() {
    assert_eq!(task_type_for_content_format("plain_text"), "text");
    assert_eq!(task_type_for_content_format("rich_html"), "text");
    assert_eq!(task_type_for_content_format("media_ref"), "image");
}

#[test]
fn build_field_format_adapter_media_text_keeps_source_and_executes_plain_text() {
    let normalized = NormalizedFieldInput {
        text: "caption".to_string(),
        effective_format: "media_ref".to_string(),
        restore_plan: FieldRestorePlan::None,
    };
    let complete_data = serde_json::Map::new();

    let adapter = build_field_format_adapter(
        "image_caption",
        "media_ref",
        &normalized,
        &Value::String("caption".to_string()),
        &complete_data,
    );

    assert_eq!(adapter.source_content_format, "media_ref");
    assert_eq!(adapter.execution_content_format, "plain_text");
    assert_eq!(adapter.routing_content_format, "plain_text");
    assert_eq!(adapter.preferred_task_type, "text");
    assert_eq!(adapter.routing_group_key, "plain_text");
    assert_eq!(adapter.translation_kind, FieldTranslationKind::Text);
}

#[test]
fn build_field_format_adapter_media_asset_routes_by_inferred_media_kind() {
    let normalized = NormalizedFieldInput {
        text: "https://example.com/clip.mp4".to_string(),
        effective_format: "media_ref".to_string(),
        restore_plan: FieldRestorePlan::None,
    };
    let complete_data = serde_json::Map::new();

    let adapter = build_field_format_adapter(
        "hero_video",
        "media_ref",
        &normalized,
        &Value::String("https://example.com/clip.mp4".to_string()),
        &complete_data,
    );

    assert_eq!(adapter.source_content_format, "media_ref");
    assert_eq!(adapter.execution_content_format, "media_ref");
    assert_eq!(adapter.routing_content_format, "media_ref");
    assert_eq!(adapter.preferred_task_type, "video");
    assert_eq!(adapter.routing_group_key, "media_ref::video");
    assert_eq!(adapter.translation_kind, FieldTranslationKind::MediaAsset);
}

// -----------------------------------------------------------------------
// strip_translation_markers
// -----------------------------------------------------------------------

#[test]
fn strip_markers_no_markers_unchanged() {
    assert_eq!(strip_translation_markers("Hello world"), "Hello world");
    assert_eq!(strip_translation_markers(""), "");
}

#[test]
fn strip_markers_removes_outer_markers() {
    assert_eq!(strip_translation_markers("【en】Hello【/en】"), "Hello");
    assert_eq!(strip_translation_markers("【zh_CN】你好【/zh_CN】"), "你好");
    assert_eq!(
        strip_translation_markers("【en-US】Hi there【/en-US】"),
        "Hi there"
    );
}

#[test]
fn strip_markers_recursive() {
    assert_eq!(
        strip_translation_markers("【en】【en】Hello【/en】【/en】"),
        "Hello"
    );
}

#[test]
fn strip_markers_deep_nesting_does_not_overflow() {
    let mut s = String::from("X");
    for _ in 0..2048 {
        s = format!("【en_US】{}【/en_US】", s);
    }
    let out = strip_translation_markers(&s);
    assert!(!out.is_empty(), "should return non-empty text");
}

#[test]
fn strip_markers_preserves_inner_content_with_markers() {
    let input = "【en】Some 【not-a-close】 text【/en】";
    assert_eq!(
        strip_translation_markers(input),
        "Some 【not-a-close】 text"
    );
}

#[test]
fn strip_markers_mismatched_close_tag_untouched() {
    let input = "【en】Hello【/zh】";
    assert_eq!(strip_translation_markers(input), input);
}

#[test]
fn strip_markers_invalid_lang_code_untouched() {
    let input = "【en US】Hello【/en US】";
    assert_eq!(strip_translation_markers(input), input);
}

#[test]
fn strip_markers_empty_lang_code_untouched() {
    let input = "【】Hello【/】";
    assert_eq!(strip_translation_markers(input), input);
}

// -----------------------------------------------------------------------
// translate_field_value
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_translate_field_value_slug_uses_text_lane() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let slug_result = translate_field_value(
        &client,
        &comp,
        "hello world",
        "post_name",
        "slug",
        "en",
        "zh",
        &c,
        "/dev/null",
        1,
    )
    .await
    .unwrap();
    assert_eq!(slug_result, Some("hello world".to_string()));

    let code_result = translate_field_value(
        &client,
        &comp,
        "const x = 1;",
        "template_code",
        "code",
        "en",
        "zh",
        &c,
        "/dev/null",
        1,
    )
    .await
    .unwrap();
    assert!(code_result.is_none(), "code format should return None");

    // media_ref with non-text field name returns None
    let result = translate_field_value(
        &client,
        &comp,
        "hello",
        "featured_image",
        "media_ref",
        "en",
        "zh",
        &c,
        "/dev/null",
        1,
    )
    .await
    .unwrap();
    assert!(
        result.is_none(),
        "media_ref with non-text field should return None"
    );
}

#[tokio::test]
async fn test_translate_field_value_media_ref_alt_translates() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    for field in &[
        "_wp_attachment_image_alt",
        "image_alt_text",
        "media_caption",
        "post_title_media",
        "image_description",
    ] {
        let result = translate_field_value(
            &client,
            &comp,
            "A cat",
            field,
            "media_ref",
            "en",
            "zh",
            &c,
            "/dev/null",
            1,
        )
        .await
        .unwrap();
        assert!(
            result.is_some(),
            "media_ref with field '{}' should translate",
            field
        );
    }
}

#[tokio::test]
async fn test_translate_field_value_plain_text_echoes() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let result = translate_field_value(
        &client,
        &comp,
        "hello",
        "post_title",
        "plain_text",
        "en",
        "zh",
        &c,
        "/dev/null",
        1,
    )
    .await
    .unwrap();
    assert_eq!(result, Some("hello".to_string()));
}

#[tokio::test]
async fn test_translate_field_value_unknown_format_returns_error() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let err = translate_field_value(
        &client,
        &comp,
        "world",
        "field_x",
        "xml_custom",
        "en",
        "zh",
        &c,
        "/dev/null",
        1,
    )
    .await
    .expect_err("unknown content_format should fail closed");
    assert!(err.to_string().contains("unsupported content_format"));
}

#[tokio::test]
async fn test_translate_field_value_rich_html_returns_some() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);
    let c = no_constraints();

    let result = translate_field_value(
        &client,
        &comp,
        "<p>hello</p>",
        "post_content",
        "rich_html",
        "en",
        "zh",
        &c,
        "/dev/null",
        1,
    )
    .await
    .unwrap();
    assert!(result.is_some(), "rich_html should return Some");
}

// -----------------------------------------------------------------------
// translate_item_fields
// -----------------------------------------------------------------------

fn test_worker_config() -> WorkerConfig {
    WorkerConfig {
        worker_id: "test-worker".to_string(),
        task_pull_statuses: vec!["pending".to_string()],
        task_concurrency: 1,
        retry_max: 2,
        retry_base_ms: 100,
        retry_max_ms: 1000,
        component_fallback_enabled: false,
        default_max_input_chars: 0,
        default_split_strategy: "none".to_string(),
        discovery_mode: true,
        review_mode: false,
    }
}

fn sign_plaintext_response_body(token: &str, body: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let signing_key = crate::crypto::derive_signing_key(token);
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&signing_key).unwrap();
    mac.update(body.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

#[tokio::test]
async fn test_translate_item_fields_no_matching_rule_returns_none() {
    let client = Client::new();
    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title": "Hello"}),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rules: Vec<DiscoveredRule> = vec![]; // no rules
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &rules,
        None,
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .unwrap();
    assert!(result.is_none(), "no matching rule → None");
}

#[tokio::test]
async fn test_translate_item_fields_empty_translate_fields_returns_none() {
    let client = Client::new();
    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title": "Hello"}),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 10,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": [],
        "field_capabilities": {}
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        None,
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .unwrap();
    assert!(result.is_none(), "empty translate_fields → None");
}

#[tokio::test]
async fn test_translate_item_fields_no_component_returns_error() {
    let client = Client::new();
    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title": "Hello"}),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 10,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title"],
        "field_capabilities": {
            "post_title": {"type": "translate", "enabled": true, "content_format": "plain_text"}
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let err = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        None,
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("no component runtime available"),
        "expected 'no component' error, got: {}",
        err
    );
}

#[tokio::test]
async fn test_translate_item_fields_all_fields_failed_returns_error() {
    let client = Client::new();
    let mut comp = echo_runtime(1);
    // Host-only loopback is deliberately blocked even in test builds. This
    // simulates an available component whose provider request fails before any
    // translated field can be produced.
    comp.template.request.url = "http://127.0.0.1/blocked-provider".to_string();

    let mut runtimes = HashMap::new();
    runtimes.insert("blocked-loopback".to_string(), comp);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["blocked-loopback".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "post_title": "Hello",
            "__wptsall_job_snapshot": {
                "source_revision": "rev-test-42",
                "policy_version": "policy-v1"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 10,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title"],
        "field_capabilities": {
            "post_title": {"type": "translate", "enabled": true, "content_format": "plain_text"}
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let err = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "blocked-loopback",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("all translatable fields failed"),
        "expected all-fields-failed error, got: {}",
        err
    );
}

#[tokio::test]
async fn test_translate_item_fields_success_builds_payload() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);

    let mut runtimes = HashMap::new();
    runtimes.insert("test-echo".to_string(), comp);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["test-echo".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "post_title": "Hello",
            "__wptsall_job_snapshot": {
                "source_revision": "rev-test-42",
                "policy_version": "policy-v1"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 10,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title"],
        "field_capabilities": {
            "post_title": {"type": "translate", "enabled": true, "content_format": "plain_text"}
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "test-echo",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .unwrap();

    assert!(result.is_some(), "should produce a payload");
    let (payload, idem_key) = result.unwrap();
    assert_eq!(payload.relation_id, 1);
    assert_eq!(payload.object_id, 42);
    assert_eq!(payload.business_line, "post_content");
    assert_eq!(payload.object_type, "post_type");
    assert_eq!(payload.subtype, "post");
    assert_eq!(payload.source_lang, "en");
    assert_eq!(payload.target_lang, "zh");
    assert_eq!(
        payload.schema_version,
        crate::config::TASK_CALLBACK_SCHEMA_VERSION
    );
    assert!(!payload.attempt_id.is_empty());
    assert!(!payload.object_snapshot_hash.is_empty());
    assert_eq!(payload.source_revision, "rev-test-42");
    assert_eq!(payload.policy_version, "policy-v1");
    assert!(payload
        .field_results
        .iter()
        .any(|r| r.field == "post_title" && r.status == "success"));
    assert_eq!(
        payload
            .translated_fields
            .get("post_title")
            .map(|s| s.as_str()),
        Some("Hello"),
        "echo server should return the input text"
    );
    let parts: Vec<&str> = idem_key.split('-').collect();
    assert_eq!(parts.len(), 6, "idempotency_key format: {}", idem_key);
    assert_eq!(
        parts[0], "discovery",
        "idempotency_key format: {}",
        idem_key
    );
    assert_eq!(parts[2], "1", "idempotency_key format: {}", idem_key);
    assert_eq!(
        parts[3], "post_type",
        "idempotency_key format: {}",
        idem_key
    );
    assert_eq!(parts[4], "42", "idempotency_key format: {}", idem_key);
    assert_eq!(parts[5].len(), 12, "idempotency_key format: {}", idem_key);
    assert_eq!(parts[1].len(), 12, "idempotency_key format: {}", idem_key);
    assert!(
        parts[1].chars().all(|c| c.is_ascii_hexdigit()),
        "idempotency_key format: {}",
        idem_key
    );
    assert!(
        parts[5].chars().all(|c| c.is_ascii_hexdigit()),
        "idempotency_key format: {}",
        idem_key
    );
}

#[tokio::test]
async fn test_translate_item_fields_echoes_wordpress_claim_snapshot() {
    let client = Client::new();
    let port = start_echo_server().await;
    let registry = ComponentRuntimeRegistry {
        runtimes: HashMap::from([("snapshot-echo".to_string(), echo_runtime(port))]),
        ordered_ids: vec!["snapshot-echo".to_string()],
    };
    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 4242,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "post_title": "Claimed source",
            "__wptsall_job_snapshot": {
                "source_revision": "source-revision-at-claim",
                "policy_version": "policy-version-at-claim"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 14,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title"],
        "field_capabilities": {
            "post_title": {"type": "translate", "enabled": true, "content_format": "plain_text"}
        }
    }))
    .unwrap();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "snapshot-echo",
        &[],
        None,
        None,
        &test_worker_config(),
        "/dev/null",
    )
    .await
    .unwrap()
    .expect("snapshot item should produce a callback payload");

    assert_eq!(result.0.source_revision, "source-revision-at-claim");
    assert_eq!(result.0.policy_version, "policy-version-at-claim");
}

#[tokio::test]
async fn test_translate_item_fields_with_trace_reports_used_component_ids() {
    let port = start_echo_server().await;
    let client = Client::new();
    let comp = echo_runtime(port);

    let mut runtimes = HashMap::new();
    runtimes.insert("test-echo".to_string(), comp);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["test-echo".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 42,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({"post_title": "Hello"}),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 10,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title"],
        "field_capabilities": {
            "post_title": {"type": "translate", "enabled": true, "content_format": "plain_text"}
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields_with_trace(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "test-echo",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .unwrap()
    .expect("trace should exist");

    assert_eq!(result.component_ids, vec!["test-echo".to_string()]);
    assert_eq!(result.payload.object_id, 42);
}

#[tokio::test]
async fn test_translate_item_fields_grouped_component_routing_uses_per_format_runtime() {
    let client = Client::new();
    let port = start_echo_server().await;

    let slug_runtime =
        echo_runtime_with_formats("comp-slug-only", port, vec!["plain_text".to_string()]);

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-slug-only".to_string(), slug_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-slug-only".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 77,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "seo_slug": "hello-world",
            "template_code": "header-hero"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 11,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        // Put code first so old "representative-field" routing would fail
        // with no component and return an error.
        "translate_fields": ["template_code", "seo_slug"],
        "field_content_formats": {
            "template_code": "code",
            "seo_slug": "slug"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("grouped routing should succeed when slug can reuse a text component");
    let (payload, _) = result.expect("slug field should now produce a callback payload");

    assert_eq!(
        payload.translated_meta.get("seo_slug"),
        Some(&"hello-world".to_string()),
        "slug field should route through the text lane and preserve content for write-back"
    );
    let slug_row = payload
        .field_results
        .iter()
        .find(|row| row.field == "seo_slug")
        .expect("seo_slug field result should exist");
    assert_eq!(slug_row.status, "success");
    assert_eq!(slug_row.content_format, "slug");
    assert_eq!(slug_row.transform_stage, "format_adapted");
    let code_row = payload
        .field_results
        .iter()
        .find(|row| row.field == "template_code")
        .expect("template_code field result should exist");
    assert_eq!(code_row.status, "skipped");
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_text_field_routes_to_text_runtime() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 78,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "image_alt": "cover image alt"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 12,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["image_alt"],
        "field_content_formats": {
            "image_alt": "media_ref"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("media_ref text field should route to text runtime");

    assert!(result.is_some(), "expected translated payload");
    let (payload, _) = result.unwrap();
    let translated_value = payload
        .translated_fields
        .get("image_alt")
        .or_else(|| payload.translated_meta.get("image_alt"))
        .map(|s| s.as_str());
    assert_eq!(translated_value, Some("cover image alt"));
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "image_alt")
        .expect("image_alt field result");
    assert_eq!(row.provider_component, "comp-text");
    assert_eq!(row.merge_target, "translated_meta");
    assert_eq!(row.transform_stage, "media_text_to_plain_text");
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_description_field_uses_plain_text_runtime() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 780,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "image_description": "hero image long description"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 121,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["image_description"],
        "field_content_formats": {
            "image_description": "media_ref"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("description field should use plain_text runtime path");

    let (payload, _) = result.expect("expected translated payload");
    let translated_value = payload
        .translated_fields
        .get("image_description")
        .or_else(|| payload.translated_meta.get("image_description"))
        .map(|s| s.as_str());
    assert_eq!(translated_value, Some("hero image long description"));
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "image_description")
        .expect("image_description field result");
    assert_eq!(row.provider_component, "comp-text");
    assert_eq!(row.merge_target, "translated_meta");
    assert_eq!(row.transform_stage, "media_text_to_plain_text");
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_excerpt_field_uses_text_runtime() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "attachment".to_string(),
        object_id: 781,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "post_excerpt": "attachment caption text"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 1211,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "attachment",
        "translate_fields": ["post_excerpt"],
        "field_content_formats": {
            "post_excerpt": "media_ref"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("excerpt field should use text runtime path");

    let (payload, _) = result.expect("expected translated payload");
    assert_eq!(
        payload
            .translated_fields
            .get("post_excerpt")
            .map(String::as_str),
        Some("attachment caption text")
    );
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "post_excerpt")
        .expect("post_excerpt field result");
    assert_eq!(row.provider_component, "comp-text");
    assert_eq!(row.transform_stage, "media_text_to_plain_text");
}

#[tokio::test]
async fn attachment_without_text_rule_builds_authenticated_source_copy_mapping() {
    let item = ContentItem {
        object_type: "post_type".to_string(),
        subtype: "attachment".to_string(),
        object_id: 782,
        needs_resync: true,
        mapping_id: None,
        complete_data: json!({
            "attachment_url": "https://example.com/wp-content/uploads/2026/08/source.jpg",
            "attachment_mime_type": "image/jpeg",
            "__wptsall_job_snapshot": {
                "source_revision": "rev-attachment-782",
                "policy_version": "policy-attachment-v1"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "wp"
    }))
    .unwrap();

    let result = translate_item_fields(
        &Client::new(),
        "https://example.com",
        &item,
        &relation,
        &[],
        None,
        "",
        &[],
        None,
        None,
        &test_worker_config(),
        "/dev/null",
    )
    .await
    .expect("attachment source-copy lane should not require a text component")
    .expect("attachment source-copy payload should be built");

    assert_eq!(result.0.media_mappings.len(), 1);
    assert_eq!(result.0.media_mappings[0].source_id, 782);
    assert_eq!(
        result.0.media_mappings[0].translated_ref,
        "https://example.com/wp-content/uploads/2026/08/source.jpg"
    );
    assert!(result.0.media_mappings[0].source_copy);
    assert!(result.0.translated_fields.is_empty());
    assert_eq!(result.0.source_revision, "rev-attachment-782");
    assert_eq!(result.0.policy_version, "policy-attachment-v1");
}

#[tokio::test]
async fn attachment_source_copy_requires_job_snapshot() {
    let item = ContentItem {
        object_type: "post_type".to_string(),
        subtype: "attachment".to_string(),
        object_id: 783,
        needs_resync: true,
        mapping_id: None,
        complete_data: json!({
            "attachment_url": "https://example.com/wp-content/uploads/2026/08/source-2.jpg",
            "attachment_mime_type": "image/jpeg"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "wp"
    }))
    .unwrap();

    let err = translate_item_fields(
        &Client::new(),
        "https://example.com",
        &item,
        &relation,
        &[],
        None,
        "",
        &[],
        None,
        None,
        &test_worker_config(),
        "/dev/null",
    )
    .await
    .expect_err("attachment source-copy without snapshot should fail");

    assert!(
        err.to_string().contains("missing source_revision"),
        "expected missing snapshot error, got: {}",
        err
    );
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_text_field_does_not_fall_back_to_media_runtime() {
    let client = Client::new();
    let port = start_echo_server().await;

    let image_runtime = echo_runtime_with_kind_and_formats(
        "comp-image",
        "image",
        port,
        vec!["media_ref".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-image".to_string(), image_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-image".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 79,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "image_caption": "caption text"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 120,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["image_caption"],
        "field_content_formats": {
            "image_caption": "media_ref"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let err = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect_err("pipeline should fail-closed when only media runtime exists");

    assert!(
        format!("{:#}", err).contains("no component runtime available"),
        "media text field must not be hijacked by media runtime when no text runtime exists: {:#}",
        err
    );
}

#[tokio::test]
async fn test_translate_item_fields_explicit_internal_meta_field_is_allowed() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 790,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "meta": {
                "_wptsall_private_label": "internal meta label"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 122,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["_wptsall_private_label"],
        "field_content_formats": {
            "_wptsall_private_label": "plain_text"
        },
        "field_storage_map": {
            "_wptsall_private_label": "post_meta"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("explicit internal meta field should not be blocked");

    let (payload, _) = result.expect("expected translated payload");
    assert_eq!(
        payload
            .translated_meta
            .get("_wptsall_private_label")
            .map(String::as_str),
        Some("internal meta label")
    );
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "_wptsall_private_label")
        .expect("field result for explicit internal meta field");
    assert_eq!(row.status, "success");
    assert_eq!(row.provider_component, "comp-text");
}

#[tokio::test]
async fn test_translate_item_fields_meta_storage_reads_meta_container() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 791,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "meta": {
                "_e2e_manual_text": "meta storage fixture"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 123,
        "model_id": 1,
        "data_type": "post",
        "object_name": "post",
        "field_capabilities": {
            "_e2e_manual_text": {
                "type": "translate",
                "enabled": true,
                "storage": "meta",
                "content_format": "plain_text"
            }
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("meta storage should be readable");

    let (payload, _) = result.expect("expected translated payload");
    assert_eq!(
        payload
            .translated_meta
            .get("_e2e_manual_text")
            .map(String::as_str),
        Some("meta storage fixture")
    );
    assert!(
        payload.translated_fields.is_empty(),
        "storage=meta must write through translated_meta"
    );
}

#[tokio::test]
async fn test_translate_item_fields_selects_rule_with_present_fields() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 792,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "meta": {
                "_e2e_manual_text": "fixture-specific field"
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let missing_email_rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 11,
        "model_id": 1,
        "data_type": "post",
        "object_name": "post",
        "field_capabilities": {
            "_email_subject": {
                "type": "translate",
                "enabled": true,
                "storage": "meta",
                "content_format": "plain_text"
            }
        }
    }))
    .unwrap();
    let fixture_rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 3,
        "model_id": 1,
        "data_type": "post",
        "object_name": "post",
        "field_capabilities": {
            "_e2e_manual_text": {
                "type": "translate",
                "enabled": true,
                "storage": "meta",
                "content_format": "plain_text"
            }
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[missing_email_rule, fixture_rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("rule selector should choose the rule with present fields");

    let (payload, _) = result.expect("expected translated payload");
    assert_eq!(
        payload
            .translated_meta
            .get("_e2e_manual_text")
            .map(String::as_str),
        Some("fixture-specific field")
    );
    assert!(
        payload
            .field_results
            .iter()
            .any(|row| row.field == "_e2e_manual_text" && row.status == "success"),
        "field result should come from the fixture rule, not the missing email rule"
    );
}

#[tokio::test]
async fn test_translate_item_fields_unknown_content_format_fails_only_that_field() {
    let client = Client::new();
    let port = start_echo_server().await;
    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-text",
        "text",
        port,
        vec!["plain_text".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-text".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-text".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 88,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "post_title": "Hello",
            "custom_blob": "<xml>Hello</xml>"
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 122,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["post_title", "custom_blob"],
        "field_content_formats": {
            "post_title": "plain_text",
            "custom_blob": "xml_custom"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("pipeline should keep valid fields and fail closed on unknown format");

    let (payload, _) = result.expect("expected payload from valid field");
    assert_eq!(
        payload
            .translated_fields
            .get("post_title")
            .map(|s| s.as_str()),
        Some("Hello")
    );
    assert!(!payload.translated_fields.contains_key("custom_blob"));
    assert!(payload
        .field_results
        .iter()
        .any(|r| r.field == "custom_blob"
            && r.status == "failed"
            && r.detail == "unsupported_content_format"
            && r.fallback_reason == "unsupported_content_format"));
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_builds_media_mapping_payload() {
    let client = Client::new();
    let port = start_echo_server().await;
    let source_ref = "data:image/jpeg;base64,QUJDREVGRw==";

    let media_runtime = ComponentRuntime {
        template: ComponentTemplate {
            id: "comp-image-media-ref".to_string(),
            name: "MediaRefEcho".to_string(),
            version: "1.0".to_string(),
            kind: "image".to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("http://127.0.0.1:{}", port),
                headers: None,
                body: Some(serde_json::json!({"text": "{{input.source_ref}}"})),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: None,
                error_path: None,
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: Some("text".to_string()),
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec![],
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: vec![],
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-image-media-ref".to_string(), media_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-image-media-ref".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 79,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "hero_media_id": 123,
            "hero_media_url": source_ref
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 13,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["hero_media_id"],
        "field_content_formats": {
            "hero_media_id": "media_ref"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("media_ref mapping should produce payload");

    let (payload, _) = result.expect("expected payload with media_mappings");
    assert!(payload.translated_fields.is_empty());
    assert!(payload.translated_meta.is_empty());
    assert_eq!(payload.media_mappings.len(), 1);
    assert_eq!(
        payload.schema_version,
        crate::config::TASK_CALLBACK_SCHEMA_VERSION
    );
    assert!(!payload.attempt_id.is_empty());
    assert!(!payload.object_snapshot_hash.is_empty());
    assert!(payload
        .field_results
        .iter()
        .any(|r| r.field == "hero_media_id" && r.status == "success"));
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "hero_media_id")
        .expect("hero_media_id field result");
    assert_eq!(row.provider_component, "comp-image-media-ref");
    assert_eq!(row.merge_target, "media_mappings");
    assert_eq!(row.transform_stage, "media_ref_direct");
    assert_eq!(payload.media_mappings[0].source_id, 123);
    assert_eq!(payload.media_mappings[0].translated_ref, source_ref);
    assert!(payload.media_mappings[0].attachment_id.is_none());
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_url_only_writes_direct_field() {
    let client = Client::new();
    let port = start_echo_server().await;
    let source_ref = "data:image/jpeg;base64,QUJDREVGRw==";

    let media_runtime = ComponentRuntime {
        template: ComponentTemplate {
            id: "comp-image-media-url".to_string(),
            name: "MediaUrlEcho".to_string(),
            version: "1.0".to_string(),
            kind: "image".to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("http://127.0.0.1:{}", port),
                headers: None,
                body: Some(serde_json::json!({"text": "{{input.source_ref}}"})),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: None,
                error_path: None,
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: Some("text".to_string()),
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec![],
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: vec![],
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-image-media-url".to_string(), media_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-image-media-url".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 80,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "hero_media_url": {
                "source_ref": source_ref
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 14,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["hero_media_url"],
        "field_content_formats": {
            "hero_media_url": "media_ref"
        },
        "field_storage_map": {
            "hero_media_url": "post_meta"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("media_ref URL-only field should produce payload");

    let (payload, _) = result.expect("expected payload");
    assert!(payload.translated_fields.is_empty());
    assert_eq!(
        payload
            .translated_meta
            .get("hero_media_url")
            .map(|s| s.as_str()),
        Some(source_ref)
    );
    assert!(payload.media_mappings.is_empty());
    assert!(payload.media_field_sources.is_empty());
    assert!(payload
        .field_results
        .iter()
        .any(|r| r.field == "hero_media_url" && r.status == "success"));
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "hero_media_url")
        .expect("hero_media_url field result");
    assert_eq!(row.provider_component, "comp-image-media-url");
    assert_eq!(row.merge_target, "translated_meta");
    assert_eq!(row.transform_stage, "media_ref_direct");
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_prefers_translated_file_artifact_component() {
    let client = Client::new();
    let port = start_echo_server().await;
    let source_ref = "data:image/jpeg;base64,QUJDREVGRw==";

    let ocr_runtime = media_runtime_with_artifacts(
        "comp-image-ocr",
        "image",
        port,
        "image_file",
        &["translated_text_content"],
    );
    let translated_runtime = media_runtime_with_artifacts(
        "comp-image-translated-file",
        "image",
        port,
        "image_file",
        &["translated_image_file"],
    );

    let registry = ComponentRuntimeRegistry {
        runtimes: HashMap::from([
            ("comp-image-ocr".to_string(), ocr_runtime),
            ("comp-image-translated-file".to_string(), translated_runtime),
        ]),
        ordered_ids: vec![
            "comp-image-ocr".to_string(),
            "comp-image-translated-file".to_string(),
        ],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 801,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "hero_media_id": 123,
            "hero_media_url": source_ref
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 15,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["hero_media_id"],
        "field_content_formats": {
            "hero_media_id": "media_ref"
        }
    }))
    .unwrap();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &test_worker_config(),
        "/dev/null",
    )
    .await
    .expect("media_ref selection should succeed");

    let (payload, _) = result.expect("expected payload");
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "hero_media_id")
        .expect("hero_media_id field result");
    assert_eq!(row.provider_component, "comp-image-translated-file");
}

#[tokio::test]
async fn test_translate_item_fields_media_ref_id_reads_companion_url_from_meta_container() {
    let client = Client::new();
    let port = start_echo_server().await;
    let source_ref = "data:image/jpeg;base64,QUJDREVGRw==";

    let media_runtime = ComponentRuntime {
        template: ComponentTemplate {
            id: "comp-image-meta-id".to_string(),
            name: "MediaMetaIdEcho".to_string(),
            version: "1.0".to_string(),
            kind: "image".to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("http://127.0.0.1:{}", port),
                headers: None,
                body: Some(serde_json::json!({"text": "{{input.source_ref}}"})),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: None,
                error_path: None,
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: Some("text".to_string()),
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec![],
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: vec![],
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-image-meta-id".to_string(), media_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-image-meta-id".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 81,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "meta": {
                "hero_media_id": 123,
                "hero_media_url": source_ref
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 141,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["hero_media_id"],
        "field_content_formats": {
            "hero_media_id": "media_ref"
        },
        "field_storage_map": {
            "hero_media_id": "post_meta"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("media_ref ID field with meta URL companion should produce payload");

    let (payload, _) = result.expect("expected payload");
    assert!(payload.translated_fields.is_empty());
    assert_eq!(payload.media_mappings.len(), 1);
    assert_eq!(payload.media_mappings[0].source_id, 123);
    assert_eq!(payload.media_mappings[0].translated_ref, source_ref);
    assert_eq!(payload.media_field_sources.get("hero_media_id"), Some(&123));
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "hero_media_id")
        .expect("hero_media_id field result");
    assert_eq!(row.provider_component, "comp-image-meta-id");
    assert_eq!(row.merge_target, "media_mappings");
    assert_eq!(row.transform_stage, "media_ref_direct");
}

#[test]
fn test_serialize_json_value_to_php_array() {
    let input = json!([1, "hi", true, null]);
    let serialized = serialize_json_value_to_php(&input);
    assert_eq!(serialized, r#"a:4:{i:0;i:1;i:1;s:2:"hi";i:2;b:1;i:3;N;}"#);
}

#[test]
fn test_normalize_field_value_json_structured_non_string() {
    let input = json!({
        "title": "Hello",
        "nested": {"description": "World"},
        "count": 1
    });
    let normalized = normalize_field_value_for_translation(
        &input,
        "json_structured",
        "seo_json",
        52,
        "/dev/null",
    )
    .unwrap()
    .expect("expected non-empty normalized value");
    assert_eq!(normalized.effective_format, "json_structured");
    assert_eq!(normalized.restore_plan, FieldRestorePlan::None);
    let reparsed: Value = serde_json::from_str(&normalized.text).expect("normalized JSON");
    assert_eq!(reparsed, input);
}

#[test]
fn test_normalize_and_restore_field_value_serialized_php_non_string() {
    let input = json!({
        "headline": "Hello",
        "enabled": true,
        "count": 2,
        "items": ["One", "Two"]
    });
    let normalized = normalize_field_value_for_translation(
        &input,
        "serialized_php",
        "acf_group",
        88,
        "/dev/null",
    )
    .unwrap()
    .expect("expected non-empty normalized value");

    assert_eq!(normalized.effective_format, "json_structured");
    assert_eq!(
        normalized.restore_plan,
        FieldRestorePlan::JsonToSerializedPhp
    );

    let restored = restore_translated_value_after_translation(
        normalized.text,
        normalized.restore_plan,
        "acf_group",
        88,
        "/dev/null",
    );
    assert!(restored.starts_with("a:"));
    assert!(restored.contains(r#"s:8:"headline";"#));
    assert!(restored.contains(r#"s:5:"Hello";"#));
    assert!(restored.contains("b:1;"));
}

#[tokio::test]
async fn test_translate_item_fields_serialized_php_non_string_reports_source_format() {
    let client = Client::new();
    let port = start_echo_server().await;

    let text_runtime = echo_runtime_with_kind_and_formats(
        "comp-json",
        "text",
        port,
        vec!["json_structured".to_string()],
    );

    let mut runtimes = HashMap::new();
    runtimes.insert("comp-json".to_string(), text_runtime);
    let registry = ComponentRuntimeRegistry {
        runtimes,
        ordered_ids: vec!["comp-json".to_string()],
    };

    let item = ContentItem {
        object_type: "post".to_string(),
        subtype: "post".to_string(),
        object_id: 890,
        needs_resync: false,
        mapping_id: None,
        complete_data: json!({
            "acf_group": {
                "headline": "Hello",
                "enabled": true,
                "count": 2
            }
        }),
    };
    let relation: DiscoveredRelation = serde_json::from_value(json!({
        "id": 1,
        "source_lang": "en",
        "target_lang": "zh",
        "sync_mode": "manual",
        "target_site_type": "virtual"
    }))
    .unwrap();
    let rule: DiscoveredRule = serde_json::from_value(json!({
        "id": 123,
        "model_id": 1,
        "data_type": "post_type",
        "object_name": "post",
        "translate_fields": ["acf_group"],
        "field_content_formats": {
            "acf_group": "serialized_php"
        }
    }))
    .unwrap();
    let wc = test_worker_config();

    let result = translate_item_fields(
        &client,
        "https://example.com",
        &item,
        &relation,
        &[rule],
        Some(&registry),
        "",
        &[],
        None,
        None,
        &wc,
        "/dev/null",
    )
    .await
    .expect("serialized_php object should translate through adapter layer");

    let (payload, _) = result.expect("expected payload");
    let translated_value = payload
        .translated_fields
        .get("acf_group")
        .or_else(|| payload.translated_meta.get("acf_group"));
    assert!(translated_value.is_some_and(|value| value.starts_with("a:")));
    let row = payload
        .field_results
        .iter()
        .find(|row| row.field == "acf_group")
        .expect("acf_group field result");
    assert_eq!(row.status, "success");
    assert_eq!(row.content_format, "serialized_php");
    assert_eq!(row.provider_component, "comp-json");
    assert_eq!(row.merge_target, "translated_meta");
    assert_eq!(row.transform_stage, "serialized_php_to_json_structured");
}

// -----------------------------------------------------------------------
// fetch_item_content
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_fetch_item_content_writes_file_and_transitions_to_fetched() {
    let dir = tempfile::tempdir().expect("temp dir");
    let raw_path = dir.path().join("raw").join("domain").join("post_42.json");
    let raw_path_str = raw_path.to_str().unwrap().to_string();

    let db = ensure_db(None);
    let item_id = {
        let conn = db.lock().await;
        conn.execute(
                "INSERT INTO translation_jobs (domain, relation_id, business_line, status, created_at, updated_at)
                 VALUES ('test', 1, 'post_content', 'running', 0, 0)",
                [],
            ).unwrap();
        let job_id: i64 = conn.last_insert_rowid();
        crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "test".to_string(),
                relation_id: 1,
                business_line: "post_content".to_string(),
                object_type: "post".to_string(),
                wp_object_id: 42,
                wp_object_subtype: "".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "".to_string(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: raw_path_str.clone(),
                client_task_id: "test-42".to_string(),
                max_retries: 3,
            },
        )
        .unwrap()
    };

    let item = {
        let conn = db.lock().await;
        crate::db::jobs::get_item(&conn, item_id).unwrap()
    };
    let content = json!({"post_title": "Hello", "post_content": "<p>World</p>"});
    let client = Client::new();

    fetch_item_content(Arc::clone(&db), &client, "/dev/null", &item, &content)
        .await
        .unwrap();

    // Verify file was written
    assert!(raw_path.exists());
    let written = std::fs::read_to_string(&raw_path).unwrap();
    assert!(written.contains("Hello"));

    // Verify DB status: fetched
    let status: String = {
        let conn = db.lock().await;
        conn.query_row(
            "SELECT status FROM translation_items WHERE id = ?1",
            [item_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(status, "fetched");
}

#[tokio::test]
async fn test_fetch_item_content_creates_parent_directories() {
    let dir = tempfile::tempdir().expect("temp dir");
    let raw_path = dir
        .path()
        .join("deep")
        .join("nested")
        .join("dirs")
        .join("post_99.json");
    let raw_path_str = raw_path.to_str().unwrap().to_string();

    let db = ensure_db(None);
    let item_id = {
        let conn = db.lock().await;
        conn.execute(
                "INSERT INTO translation_jobs (domain, relation_id, business_line, status, created_at, updated_at)
                 VALUES ('test', 1, 'post_content', 'running', 0, 0)",
                [],
            ).unwrap();
        let job_id: i64 = conn.last_insert_rowid();
        crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "test".to_string(),
                relation_id: 1,
                business_line: "post_content".to_string(),
                object_type: "post".to_string(),
                wp_object_id: 99,
                wp_object_subtype: "".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "".to_string(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: raw_path_str.clone(),
                client_task_id: "test-99".to_string(),
                max_retries: 3,
            },
        )
        .unwrap()
    };

    let item = {
        let conn = db.lock().await;
        crate::db::jobs::get_item(&conn, item_id).unwrap()
    };
    let content = json!({"title": "Nested"});
    let client = Client::new();

    fetch_item_content(Arc::clone(&db), &client, "/dev/null", &item, &content)
        .await
        .unwrap();

    // Verify deeply nested directories were created and file exists
    assert!(raw_path.exists());
    assert!(raw_path.parent().unwrap().is_dir());
}

#[tokio::test]
async fn test_fetch_item_content_invalid_path_returns_error() {
    let db = ensure_db(None);
    let item_id = {
        let conn = db.lock().await;
        conn.execute(
                "INSERT INTO translation_jobs (domain, relation_id, business_line, status, created_at, updated_at)
                 VALUES ('test', 1, 'post_content', 'running', 0, 0)",
                [],
            ).unwrap();
        let job_id: i64 = conn.last_insert_rowid();
        crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "test".to_string(),
                relation_id: 1,
                business_line: "post_content".to_string(),
                object_type: "post".to_string(),
                wp_object_id: 50,
                wp_object_subtype: "".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "".to_string(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: "/proc/0/impossible_path/file.json".to_string(),
                client_task_id: "test-50".to_string(),
                max_retries: 3,
            },
        )
        .unwrap()
    };

    let item = {
        let conn = db.lock().await;
        crate::db::jobs::get_item(&conn, item_id).unwrap()
    };
    let content = json!({"title": "Fail"});
    let client = Client::new();

    let err = fetch_item_content(Arc::clone(&db), &client, "/dev/null", &item, &content).await;
    assert!(err.is_err(), "writing to /proc should fail");

    // DB status should be "failed"
    let status: String = {
        let conn = db.lock().await;
        conn.query_row(
            "SELECT status FROM translation_items WHERE id = ?1",
            [item_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(status, "failed");
}

// -----------------------------------------------------------------------
// sync_item_to_wp
// -----------------------------------------------------------------------

#[tokio::test]
async fn test_sync_item_to_wp_missing_translated_file_returns_error() {
    let db = ensure_db(None);
    let client = Client::new();
    let wc = test_worker_config();
    let sem = Arc::new(Semaphore::new(1));

    let err = sync_item_to_wp(
        &db,
        &client,
        1,
        "/nonexistent/translated.json",
        "https://example.com",
        "token",
        &wc,
        Some("secret"),
        "/dev/null",
        &sem,
    )
    .await;
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("failed to read translated file"),
        "should fail reading nonexistent file"
    );
}

#[tokio::test]
async fn test_sync_item_to_wp_invalid_json_file_returns_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("bad.json");
    std::fs::write(&path, "not json").unwrap();

    let db = ensure_db(None);
    let client = Client::new();
    let wc = test_worker_config();
    let sem = Arc::new(Semaphore::new(1));

    let err = sync_item_to_wp(
        &db,
        &client,
        1,
        path.to_str().unwrap(),
        "https://example.com",
        "token",
        &wc,
        Some("secret"),
        "/dev/null",
        &sem,
    )
    .await;
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("failed to parse translated file"),
        "should fail parsing invalid JSON"
    );
}

#[tokio::test]
async fn test_sync_item_to_wp_uploads_pending_media_before_callback() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        dir.path().to_string_lossy().into_owned(),
    );
    let domain_dir = dir.path().join("translated").join("demo");
    std::fs::create_dir_all(&domain_dir).unwrap();
    let translated_path = domain_dir.join("post_42.json");
    let raw_dir = dir.path().join("raw").join("demo");
    std::fs::create_dir_all(&raw_dir).unwrap();
    let raw_path = raw_dir.join("post_42.json");
    std::fs::write(&raw_path, b"{\"post_title\":\"Hello\"}").unwrap();

    let envelope = json!({
        "idempotency_key": "idem-media-sync",
        "route_secret": "route_test",
        "payload": {
            "schema_version": crate::config::TASK_CALLBACK_SCHEMA_VERSION,
            "attempt_id": "att-1",
            "object_snapshot_hash": "hash",
            "field_results": [],
            "relation_id": 1,
            "business_line": "post_content",
            "object_type": "post_type",
            "post_type": "post",
            "object_id": 42,
            "translated_fields": {},
            "translated_meta": {},
            "media_mappings": [{
                "source_id": 7,
                "translated_ref": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO7+eVQAAAAASUVORK5CYII=",
                "attachment_id": null
            }],
            "media_field_sources": {},
            "client_task_id": "ctask-media-sync",
            "worker_id": "test-worker",
            "source_lang": "en_US",
            "target_lang": "zh_CN",
            "execution_time_ms": 1
        },
        "persisted_at": 1
    });
    std::fs::write(
        &translated_path,
        serde_json::to_string_pretty(&envelope).unwrap(),
    )
    .unwrap();

    let db = ensure_db(None);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen_paths = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let seen_paths_task = Arc::clone(&seen_paths);
    tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let read = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            let first_line = request.lines().next().unwrap_or_default().to_string();
            seen_paths_task.lock().await.push(first_line.clone());

            if first_line.contains("media-upload") {
                let response_body = "{\"success\":true,\"attachment_id\":321}";
                let response_sig = sign_plaintext_response_body("token", response_body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_sig,
                    response_body.len(),
                    response_body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            } else {
                let response_body = "{\"success\":true,\"result_id\":123,\"protocol\":\"v2\"}";
                let response_sig = sign_plaintext_response_body("token", response_body);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_sig,
                    response_body.len(),
                    response_body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        }
    });

    sync_item_to_wp(
        &db,
        &Client::new(),
        77,
        translated_path.to_string_lossy().as_ref(),
        &format!("http://127.0.0.1:{port}"),
        "token",
        &test_worker_config(),
        Some("route_test"),
        "/dev/null",
        &Arc::new(Semaphore::new(1)),
    )
    .await
    .unwrap();

    let seen_paths = seen_paths.lock().await.clone();
    assert_eq!(seen_paths.len(), 2, "expected media upload + callback");
    assert!(
        seen_paths[0].contains("media-upload"),
        "first request should upload media before callback: {:?}",
        seen_paths
    );
    assert!(
        seen_paths[1].contains("translation-callback"),
        "second request should submit callback: {:?}",
        seen_paths
    );
    assert!(
        !translated_path.exists(),
        "translated envelope should be removed after successful sync"
    );
    assert!(
        !raw_path.exists(),
        "raw file should be removed after successful sync"
    );
}

#[tokio::test]
async fn sync_i18n_item_to_wp_marks_item_done_and_cleans_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        dir.path().to_string_lossy().into_owned(),
    );

    let raw_dir = dir
        .path()
        .join("raw")
        .join("https-example.com")
        .join("rel_1");
    let translated_dir = dir
        .path()
        .join("translated")
        .join("https-example.com")
        .join("rel_1");
    std::fs::create_dir_all(&raw_dir).unwrap();
    std::fs::create_dir_all(&translated_dir).unwrap();

    let raw_path = raw_dir.join("language_pack_plugin_i18n_plugin_abc123.json");
    let translated_path = translated_dir.join("language_pack_plugin_i18n_plugin_abc123.json");
    std::fs::write(&raw_path, "{\"msgid\":\"hello\"}").unwrap();

    let db = ensure_db(None);
    let item_id = {
        let conn = db.lock().await;
        let job_id = crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: "https://example.com".to_string(),
                relation_id: 1,
                business_line: "discovery".to_string(),
                triggered_by: "auto".to_string(),
            },
        )
        .unwrap();
        let item_id = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "https://example.com".to_string(),
                relation_id: 1,
                business_line: "plugin_i18n".to_string(),
                object_type: "language_pack".to_string(),
                wp_object_id: 12345,
                wp_object_subtype: "plugin".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: "comp-i18n".to_string(),
                component_ids: vec!["comp-i18n".to_string()],
                selected_component_id: Some("comp-i18n".to_string()),
                effective_source_lang: Some("en".to_string()),
                effective_target_lang: Some("zh".to_string()),
                editable_overrides: None,
                raw_path: raw_path.to_string_lossy().to_string(),
                client_task_id: "lang-pack-idem".to_string(),
                max_retries: 2,
            },
        )
        .unwrap();
        crate::db::jobs::update_item_translated_path(
            &conn,
            item_id,
            translated_path.to_string_lossy().as_ref(),
        )
        .unwrap();
        crate::db::jobs::update_item_status(&conn, item_id, "translated", None).unwrap();
        item_id
    };

    let envelope = crate::types::I18nTranslatedEnvelope {
        payload_type: "i18n_language_pack".to_string(),
        idempotency_key: "lang-pack-idem".to_string(),
        route_secret: Some("route_test".to_string()),
        payload: crate::types::I18nCallbackPayload {
            business_line: "plugin_i18n".to_string(),
            relation_id: 1,
            client_task_id: "lang-pack-idem".to_string(),
            worker_id: "worker-test".to_string(),
            source_lang: "en".to_string(),
            target_lang: "zh".to_string(),
            entries: vec![crate::types::I18nCallbackEntry {
                entry_id: 88,
                msgstr: "你好".to_string(),
            }],
        },
        persisted_at: 1,
    };
    std::fs::write(
        &translated_path,
        serde_json::to_string_pretty(&envelope).unwrap(),
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        let response_body = "{\"success\":true,\"result_id\":123}";
        let response_sig = sign_plaintext_response_body("token", response_body);
        let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Transport: plaintext\r\nX-WPTSALL-Response-Signature: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_sig,
                response_body.len(),
                response_body
            );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let count = sync_i18n_item_to_wp(
        &db,
        &Client::new(),
        item_id,
        translated_path.to_string_lossy().as_ref(),
        &format!("http://127.0.0.1:{port}"),
        "token",
        &test_worker_config(),
        Some("route_test"),
        "/dev/null",
        &Arc::new(Semaphore::new(1)),
    )
    .await
    .unwrap();

    assert_eq!(count, 1);
    assert!(
        !translated_path.exists(),
        "translated envelope should be removed"
    );
    assert!(
        !raw_path.exists(),
        "raw file should be removed after successful sync"
    );

    let status: String = {
        let conn = db.lock().await;
        conn.query_row(
            "SELECT status FROM translation_items WHERE id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(status, "done");
}
