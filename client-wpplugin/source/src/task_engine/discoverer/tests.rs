use std::collections::HashMap;
use std::sync::atomic::AtomicI64;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
#[path = "../../../../../tests/modules/client-wpplugin/unit/encrypted_json_discovery.rs"]
mod encrypted_json_discovery;
#[path = "../../../../../tests/modules/client-wpplugin/unit/physical_discovery_capacity.rs"]
mod physical_discovery_capacity;

mod language_pack_recovery {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/modules/client-wpplugin/unit/language_pack_recovery.rs"
    ));
}
use crate::types::{
    ComponentEditableParam, ComponentRequest, ComponentResponse, ComponentRuntime,
    ComponentTemplate,
};

mod job_accounting;
mod ownership;
mod startup_resume;

#[test]
fn extract_retry_after_ms_from_error_supports_ms_and_seconds_tokens() {
    assert_eq!(
        extract_retry_after_ms_from_error("rate limited retry_after_ms=2500"),
        Some(2500)
    );
    assert_eq!(
        extract_retry_after_ms_from_error("status=429 retry-after=3"),
        Some(3000)
    );
    assert_eq!(extract_retry_after_ms_from_error("no retry hint"), None);
}

#[test]
fn is_pressure_error_message_matches_rate_limit_and_timeout_shapes() {
    assert!(is_pressure_error_message("status=429"));
    assert!(is_pressure_error_message("request timed out"));
    assert!(is_pressure_error_message("upstream status 503"));
    assert!(!is_pressure_error_message("validation failed"));
}

#[test]
fn retain_claimed_content_items_keeps_only_claimed_subtypes() {
    let mut content_items = vec![
        ContentItem {
            object_type: "post_type".to_string(),
            subtype: "post".to_string(),
            object_id: 11,
            needs_resync: false,
            mapping_id: None,
            complete_data: json!({}),
        },
        ContentItem {
            object_type: "post_type".to_string(),
            subtype: "page".to_string(),
            object_id: 12,
            needs_resync: false,
            mapping_id: None,
            complete_data: json!({}),
        },
    ];

    let retained = retain_claimed_content_items(
        &mut content_items,
        vec![ClaimedContentItem {
            object_id: 12,
            post_type: "page".to_string(),
            ..Default::default()
        }],
        false,
    );

    assert_eq!(retained, 1);
    assert_eq!(content_items.len(), 1);
    assert_eq!(content_items[0].object_id, 12);
}

#[test]
fn retain_claimed_language_pack_items_keeps_only_claimed_entries() {
    let mut language_pack_items = vec![
        LanguagePackItem {
            object_id: 1,
            text_domain: "demo".to_string(),
            complete_data: LanguagePackCompleteData {
                entry_id: 101,
                msgid: "Hello".to_string(),
                msgctxt: String::new(),
                msgid_plural: String::new(),
                plural_index: None,
                text_domain: "demo".to_string(),
            },
        },
        LanguagePackItem {
            object_id: 2,
            text_domain: "demo".to_string(),
            complete_data: LanguagePackCompleteData {
                entry_id: 102,
                msgid: "World".to_string(),
                msgctxt: String::new(),
                msgid_plural: String::new(),
                plural_index: None,
                text_domain: "demo".to_string(),
            },
        },
    ];

    let retained = retain_claimed_language_pack_items(
        &mut language_pack_items,
        vec![ClaimedContentItem {
            entry_id: 102,
            ..Default::default()
        }],
    );

    assert_eq!(retained, 1);
    assert_eq!(language_pack_items.len(), 1);
    assert_eq!(language_pack_items[0].complete_data.entry_id, 102);
}

#[test]
fn language_pack_batch_idempotency_key_tracks_actual_batch_body() {
    let entries = vec![
        I18nCallbackEntry {
            entry_id: 101,
            msgstr: "Hello translated".to_string(),
        },
        I18nCallbackEntry {
            entry_id: 102,
            msgstr: "World translated".to_string(),
        },
    ];

    let key = language_pack_batch_idempotency_key(
        "http://127.0.0.1:9181/wp-json/wptsall/v2/secret/client",
        7,
        "theme_i18n",
        "theme",
        "zh_CN",
        "en_US",
        "e2e-slot-a",
        &entries,
    );
    let repeat_key = language_pack_batch_idempotency_key(
        "http://127.0.0.1:9181/wp-json/wptsall/v2/other-secret/client",
        7,
        "theme_i18n",
        "theme",
        "zh_CN",
        "en_US",
        "e2e-slot-a",
        &entries,
    );
    let next_batch_key = language_pack_batch_idempotency_key(
        "http://127.0.0.1:9181/wp-json/wptsall/v2/secret/client",
        7,
        "theme_i18n",
        "theme",
        "zh_CN",
        "en_US",
        "e2e-slot-a",
        &[I18nCallbackEntry {
            entry_id: 103,
            msgstr: "Next translated".to_string(),
        }],
    );
    let changed_body_key = language_pack_batch_idempotency_key(
        "http://127.0.0.1:9181/wp-json/wptsall/v2/secret/client",
        7,
        "theme_i18n",
        "theme",
        "zh_CN",
        "en_US",
        "e2e-slot-a",
        &[I18nCallbackEntry {
            entry_id: 101,
            msgstr: "Changed translation".to_string(),
        }],
    );

    assert_eq!(
        key, repeat_key,
        "route_secret changes must not create a new idempotency identity"
    );
    assert_ne!(
        key, next_batch_key,
        "different language-pack entry batches must not reuse one Idempotency-Key"
    );
    assert_ne!(
        key, changed_body_key,
        "same entry id with different translated body must get a different key"
    );
    assert!(
        key.len() <= 128,
        "Idempotency-Key must satisfy WP header validator: {}",
        key
    );
    assert!(
        !key.contains("http://") && !key.contains("https://"),
        "Idempotency-Key should use a bounded domain fingerprint: {}",
        key
    );
}

#[test]
fn tighten_total_pages_only_lowers_upper_bound() {
    let total_pages = AtomicI64::new(i64::MAX);
    tighten_total_pages(&total_pages, 45, 10);
    assert_eq!(total_pages.load(std::sync::atomic::Ordering::Relaxed), 5);

    tighten_total_pages(&total_pages, 100, 10);
    assert_eq!(total_pages.load(std::sync::atomic::Ordering::Relaxed), 5);
}

fn sample_relation() -> DiscoveredRelation {
    DiscoveredRelation {
        id: 7,
        source_site_id: json!(1),
        source_lang: "en".to_string(),
        target_site_id: json!(2),
        target_site_type: "site".to_string(),
        target_lang: "zh".to_string(),
        sync_mode: "push".to_string(),
        media_handling: String::new(),
        template: String::new(),
        models: Vec::new(),
        i18n_config: None,
        source_group_config: None,
        preflight_policy: String::new(),
        missing_component_behavior: String::new(),
    }
}

fn sample_rule(data_type: &str, object_name: &str) -> DiscoveredRule {
    DiscoveredRule {
        id: 1,
        model_id: 1,
        name: format!("sample-{data_type}-rule"),
        data_type: data_type.to_string(),
        object_name: object_name.to_string(),
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

fn sample_registry() -> ComponentRuntimeRegistry {
    let runtime = ComponentRuntime {
        template: ComponentTemplate {
            id: "comp-selected".to_string(),
            name: "Selected".to_string(),
            version: "1.0.0".to_string(),
            kind: "text".to_string(),
            client_contract: None,
            default_values: Some(json!({ "temperature": 0.9 })),
            auth: None,
            prepare: None,
            request: ComponentRequest {
                http_limits: None,
                method: "POST".to_string(),
                url: "https://example.com".to_string(),
                headers: None,
                body: Some(json!({ "text": "{{input.text}}" })),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: Some("text".to_string()),
                error_path: None,
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: None,
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: Some(ComponentConstraints {
                max_input_chars: Some(123),
                ..Default::default()
            }),
            editable_params: vec![
                ComponentEditableParam {
                    path: "default_values.temperature".to_string(),
                    scope: None,
                    value_type: Some("number".to_string()),
                    required: None,
                },
                ComponentEditableParam {
                    path: "constraints.max_input_chars".to_string(),
                    scope: None,
                    value_type: Some("number".to_string()),
                    required: None,
                },
            ],
            translation_modes: vec![],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec!["plugin_i18n".to_string()],
        language_map: HashMap::new(),
        supported_content_formats: vec![],
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
    ComponentRuntimeRegistry {
        runtimes: HashMap::from([(runtime.template.id.clone(), runtime)]),
        ordered_ids: vec!["comp-selected".to_string()],
    }
}

fn discovery_test_worker_config() -> WorkerConfig {
    WorkerConfig {
        worker_id: "worker-test".to_string(),
        device_id: "worker-test".to_string(),
        task_pull_statuses: vec!["pending".to_string()],
        task_concurrency: 1,
        retry_max: 0,
        retry_base_ms: 1,
        retry_max_ms: 1,
        component_fallback_enabled: true,
        default_max_input_chars: 10_000,
        default_split_strategy: "none".to_string(),
        discovery_mode: true,
        discovery_max_items_per_run: 100,
        review_mode: false,
    }
}

async fn spawn_recording_wp_server(
    relations_body: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = Arc::clone(&requests);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0u8; 8192];
            let Ok(n) = socket.read(&mut request).await else {
                continue;
            };
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("").to_string();
            requests_for_server.lock().await.push(first_line.clone());

            let (status, body) = if first_line.contains("/site-relations") {
                ("200 OK", relations_body.to_string())
            } else if first_line.contains("/content-changes") {
                (
                    "200 OK",
                    r#"{"success":true,"data":{"items":[],"schema_version":1}}"#.to_string(),
                )
            } else if first_line.contains("/rules?relation_id=7") {
                ("200 OK", r#"{"rules":[]}"#.to_string())
            } else if first_line.contains("/content?relation_id=7") {
                (
                    "200 OK",
                    r#"{"items":[],"total":0,"page":1,"per_page":20}"#.to_string(),
                )
            } else {
                (
                    "404 Not Found",
                    format!(r#"{{"unexpected_request":"{}"}}"#, first_line),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    (format!("http://{}", addr), requests)
}

/// GAP-06 收尾 (批 J): WP 旅程测试专用——记录完整请求文本（含请求头），
/// 用于断言 run 级 X-WPTSALL-Trace-Id 出站头。路由行为与
/// spawn_recording_wp_server 一致。
async fn spawn_full_request_recording_wp_server(
    relations_body: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = Arc::clone(&requests);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let requests_for_server = requests_for_server.clone();
            tokio::spawn(async move {
                let mut request = [0u8; 16384];
                let Ok(n) = socket.read(&mut request).await else {
                    return;
                };
                let request_text = String::from_utf8_lossy(&request[..n]).to_string();
                let first_line = request_text.lines().next().unwrap_or("").to_string();
                {
                    let mut guard = requests_for_server.lock().await;
                    guard.push(request_text);
                }
                let (status, body) = if first_line.contains("/site-relations") {
                    ("200 OK", relations_body.to_string())
                } else if first_line.contains("/content-changes") {
                    (
                        "200 OK",
                        r#"{"success":true,"data":{"items":[],"schema_version":1}}"#.to_string(),
                    )
                } else if first_line.contains("/rules?relation_id=7") {
                    ("200 OK", r#"{"rules":[]}"#.to_string())
                } else if first_line.contains("/content?relation_id=7") {
                    (
                        "200 OK",
                        r#"{"items":[],"total":0,"page":1,"per_page":20}"#.to_string(),
                    )
                } else {
                    (
                        "404 Not Found",
                        format!(r#"{{"unexpected_request":"{}"}}"#, first_line),
                    )
                };
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });

    (format!("http://{}", addr), requests)
}

#[tokio::test]
async fn webui_db_outbox_drain_is_relation_scoped_and_skips_disabled_relations() {
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let relations_body = r#"{"relations":[{"id":7,"source_site_id":1,"source_lang":"en_US","target_site_id":2,"target_site_type":"virtual","target_lang":"zh_CN","sync_mode":"push","models":[]},{"id":8,"source_site_id":1,"source_lang":"en_US","target_site_id":3,"target_site_type":"virtual","target_lang":"fr_FR","sync_mode":"push","models":[]}]}"#;
    let (base, requests) = spawn_recording_wp_server(relations_body).await;
    let wp_base = format!("{}/wp-json/wptsall/v2/secret/client", base);
    let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").expect("db")));
    {
        let conn = db.lock().await;
        ensure_discovery_tasks(&conn, &wp_base, &[7, 8]).expect("discovery tasks");
        conn.execute(
            "UPDATE discovery_tasks SET enabled = 0 WHERE domain = ?1 AND relation_id = ?2",
            rusqlite::params![&wp_base, 8_i64],
        )
        .expect("disable relation 8");
    }

    let pending_db = temp.path().join("pending.sqlite");
    let pending_store = Arc::new(Mutex::new(
        PendingCallbackStore::open(pending_db.to_string_lossy().as_ref())
            .expect("pending callback store"),
    ));
    let client = Client::new();
    let worker_config = discovery_test_worker_config();
    let prefer_ids: Vec<String> = Vec::new();
    let log_file = temp.path().join("worker.log").display().to_string();

    let report = discover_and_translate(
        &client,
        &wp_base,
        "token-test",
        &log_file,
        None,
        None,
        "",
        &prefer_ids,
        None,
        None,
        &worker_config,
        Some("secret"),
        &pending_store,
        None,
        None,
        None,
        Some(db),
        crate::types::PluginIdentity::WpmmccAts,
    )
    .await
    .expect("discovery should complete");

    assert_eq!(report.failed, 0);
    let seen = requests.lock().await.clone();
    let site_relations_pos = seen
        .iter()
        .position(|line| line.contains("/site-relations"))
        .expect("site-relations request");
    let scoped_outbox_pos = seen
        .iter()
        .position(|line| line.contains("/content-changes?limit=100&relation_id=7"))
        .expect("enabled relation-scoped outbox request");
    assert!(
        site_relations_pos < scoped_outbox_pos,
        "WebUI/Desktop DB path must fetch site-relations before draining outbox: {:?}",
        seen
    );
    assert!(
        !seen
            .iter()
            .any(|line| line.contains("/content-changes?limit=100 HTTP/")),
        "DB path must not perform global outbox drain: {:?}",
        seen
    );
    assert!(
        !seen
            .iter()
            .any(|line| line.contains("/content-changes") && line.contains("relation_id=8")),
        "disabled relation must not drain outbox: {:?}",
        seen
    );
    assert!(
        seen.iter()
            .any(|line| line.contains("/rules?relation_id=7")),
        "enabled relation should continue to rules/content discovery: {:?}",
        seen
    );
    assert!(
        !seen
            .iter()
            .any(|line| line.contains("/rules?relation_id=8")),
        "disabled relation must not fetch rules: {:?}",
        seen
    );
    assert!(
        !seen
            .iter()
            .any(|line| line.contains("/content?relation_id=8")),
        "disabled relation must not fetch content: {:?}",
        seen
    );
}

#[tokio::test]
async fn cli_without_db_keeps_legacy_global_outbox_drain_before_relations() {
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data-cli").display().to_string(),
    );
    let (base, requests) = spawn_recording_wp_server(r#"{"relations":[]}"#).await;
    let wp_base = format!("{}/wp-json/wptsall/v2/secret/client", base);
    let pending_db = temp.path().join("pending-cli.sqlite");
    let pending_store = Arc::new(Mutex::new(
        PendingCallbackStore::open(pending_db.to_string_lossy().as_ref())
            .expect("pending callback store"),
    ));
    let client = Client::new();
    let worker_config = discovery_test_worker_config();
    let prefer_ids: Vec<String> = Vec::new();
    let log_file = temp.path().join("worker-cli.log").display().to_string();

    discover_and_translate(
        &client,
        &wp_base,
        "token-test",
        &log_file,
        None,
        None,
        "",
        &prefer_ids,
        None,
        None,
        &worker_config,
        Some("secret"),
        &pending_store,
        None,
        None,
        None,
        None,
        crate::types::PluginIdentity::WpmmccAts,
    )
    .await
    .expect("CLI discovery should complete");

    let seen = requests.lock().await.clone();
    assert!(
        seen.first()
            .map(|line| line.contains("/content-changes?limit=100 HTTP/"))
            .unwrap_or(false),
        "CLI mode should keep the legacy global outbox drain first: {:?}",
        seen
    );
    assert!(
        !seen
            .first()
            .map(|line| line.contains("relation_id="))
            .unwrap_or(false),
        "legacy CLI outbox drain must not add relation_id: {:?}",
        seen
    );
    assert!(
        seen.get(1)
            .map(|line| line.contains("/site-relations"))
            .unwrap_or(false),
        "CLI mode should fetch site-relations after global outbox drain: {:?}",
        seen
    );
}

/// GAP-06 收尾 (批 J): 一个发现 run 的全部出站 WP 请求必须携带同一
/// X-WPTSALL-Trace-Id（= report.trace_id），供 WP/mock 日志按 run 对齐。
#[tokio::test]
async fn discovery_run_stamps_one_outbound_trace_id_on_all_wp_requests() {
    // 共享 run-trace static 的测试必须串行（见 auth::run_trace_test_lock）：
    // 本测试的整个 run 期间持有 run trace，与 auth/runner 的 trace 测试互斥。
    let _serialized = crate::auth::run_trace_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data-trace").display().to_string(),
    );
    let relations_body = r#"{"relations":[{"id":7,"source_site_id":1,"source_lang":"en_US","target_site_id":2,"target_site_type":"virtual","target_lang":"zh_CN","sync_mode":"push","models":[]}]}"#;
    let (base, requests) = spawn_full_request_recording_wp_server(relations_body).await;
    let wp_base = format!("{}/wp-json/wptsall/v2/secret/client", base);
    let pending_db = temp.path().join("pending-trace.sqlite");
    let pending_store = Arc::new(Mutex::new(
        PendingCallbackStore::open(pending_db.to_string_lossy().as_ref())
            .expect("pending callback store"),
    ));
    let client = Client::new();
    let worker_config = discovery_test_worker_config();
    let prefer_ids: Vec<String> = Vec::new();
    let log_file = temp.path().join("worker-trace.log").display().to_string();

    let report = discover_and_translate(
        &client,
        &wp_base,
        "token-test",
        &log_file,
        None,
        None,
        "",
        &prefer_ids,
        None,
        None,
        &worker_config,
        Some("secret"),
        &pending_store,
        None,
        None,
        None,
        None,
        crate::types::PluginIdentity::WpmmccAts,
    )
    .await
    .expect("discovery should complete");

    assert!(
        report.trace_id.starts_with("disc-"),
        "run report must carry a disc- trace id, got: {:?}",
        report.trace_id
    );
    let seen = requests.lock().await.clone();
    assert!(
        seen.len() >= 3,
        "run should issue several WP requests, got: {:?}",
        seen.iter()
            .map(|r| r.lines().next().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    );
    let expected_header = format!("x-wptsall-trace-id: {}", report.trace_id);
    for (idx, req) in seen.iter().enumerate() {
        let lowered = req.to_lowercase();
        assert!(
            lowered.contains(&expected_header),
            "WP request #{} must carry the run trace header {:?}: {:?}",
            idx,
            expected_header,
            req.lines().next().unwrap_or("")
        );
    }
}

#[test]
fn build_task_scoped_runtime_registry_applies_editable_overrides() {
    let registry = sample_registry();
    let overrides = json!({
        "default_values.temperature": 0.2,
        "constraints.max_input_chars": 42
    });

    let scoped = build_task_scoped_runtime_registry(
        Some(&registry),
        Some("comp-selected"),
        Some(&overrides),
    )
    .unwrap()
    .expect("scoped registry");

    let runtime = scoped.runtimes.get("comp-selected").expect("runtime");
    assert_eq!(
        runtime.template.default_values.as_ref().unwrap()["temperature"],
        json!(0.2)
    );
    assert_eq!(
        runtime
            .template
            .constraints
            .as_ref()
            .and_then(|constraints| constraints.max_input_chars),
        Some(42)
    );
}

#[test]
fn discover_content_data_types_includes_option_when_rules_declare_option_sources() {
    let relation = sample_relation();
    let rules = vec![sample_rule("option", "wptsall_mail_template")];

    let discovered = discover_content_data_types(&relation, &rules);

    assert!(discovered.contains(&"option"));
    assert!(discovered.contains(&"post"));
    assert!(!discovered.contains(&"term"));
}

#[tokio::test]
async fn persist_language_pack_batch_for_sync_persists_task_params_and_files() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").expect("db"),
    ));
    let job_id = {
        let conn = db.lock().await;
        crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: "https://example.com".to_string(),
                relation_id: 7,
                business_line: "discovery".to_string(),
                triggered_by: "auto".to_string(),
            },
        )
        .unwrap()
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let relation = sample_relation();
    let mut task_params = DiscoveryTaskParams::default();
    task_params.selected_component_id = Some("comp-selected".to_string());
    task_params.effective_source_lang = Some("fr".to_string());
    task_params.effective_target_lang = Some("de".to_string());
    task_params.editable_overrides =
        Some(json!({ "default_values.temperature": 0.2, "request.body.mode": "strict" }));
    let effective_relation = apply_discovery_task_relation_overrides(&relation, &task_params);
    let payload = I18nCallbackPayload {
        business_line: "plugin_i18n".to_string(),
        relation_id: 7,
        client_task_id: "lang-pack-batch".to_string(),
        worker_id: "worker-test".to_string(),
        source_lang: effective_relation.source_lang.clone(),
        target_lang: effective_relation.target_lang.clone(),
        entries: vec![
            I18nCallbackEntry {
                entry_id: 101,
                msgstr: "Hallo".to_string(),
            },
            I18nCallbackEntry {
                entry_id: 102,
                msgstr: "Welt".to_string(),
            },
        ],
    };
    let source_items = vec![
        LanguagePackItem {
            object_id: 88,
            text_domain: "my-plugin".to_string(),
            complete_data: LanguagePackCompleteData {
                entry_id: 101,
                msgid: "Hello".to_string(),
                msgctxt: String::new(),
                msgid_plural: String::new(),
                plural_index: None,
                text_domain: "my-plugin".to_string(),
            },
        },
        LanguagePackItem {
            object_id: 89,
            text_domain: "my-plugin".to_string(),
            complete_data: LanguagePackCompleteData {
                entry_id: 102,
                msgid: "World".to_string(),
                msgctxt: String::new(),
                msgid_plural: String::new(),
                plural_index: None,
                text_domain: "my-plugin".to_string(),
            },
        },
    ];

    let persisted = persist_language_pack_batch_for_sync(
        &db,
        job_id,
        dir.path().to_string_lossy().as_ref(),
        "https-example.com",
        "https://example.com",
        &relation,
        &effective_relation,
        "plugin_i18n",
        "plugin",
        &source_items,
        &payload.entries,
        "lang-pack-batch",
        Some("route_test"),
        &payload,
        "comp-selected",
        &task_params,
    )
    .await
    .unwrap();

    assert!(
        std::path::Path::new(&persisted.translated_path).exists(),
        "translated file should exist"
    );
    let raw_path = {
        let conn = db.lock().await;
        let item = crate::db::jobs::get_item(&conn, persisted.item_id).expect("item");
        assert_eq!(item.status, "translated");
        assert_eq!(item.selected_component_id.as_deref(), Some("comp-selected"));
        assert_eq!(item.effective_source_lang.as_deref(), Some("fr"));
        assert_eq!(item.effective_target_lang.as_deref(), Some("de"));
        assert_eq!(
            item.editable_overrides.as_ref().unwrap()["default_values.temperature"],
            json!(0.2)
        );
        assert_eq!(item.source_lang, "fr");
        assert_eq!(item.target_lang, "de");
        let job = crate::db::jobs::get_job(&conn, job_id).unwrap();
        assert_eq!(
            (job.total_items, job.done_items, job.failed_items),
            (1, 0, 0)
        );
        assert_eq!(
            crate::db::jobs::project_job_from_items(&conn, job_id, true, "completed").unwrap(),
            "partial"
        );
        item.raw_path
    };
    assert!(
        std::path::Path::new(&raw_path).exists(),
        "raw file should exist"
    );

    let translated: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&persisted.translated_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(translated["payload"]["source_lang"], "fr");
    assert_eq!(translated["payload"]["target_lang"], "de");
    assert_eq!(
        translated["payload"]["entries"].as_array().unwrap().len(),
        2
    );

    let raw: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&raw_path)).unwrap(),
    )
    .unwrap();
    assert_eq!(raw["source_lang"], "fr");
    assert_eq!(raw["target_lang"], "de");
    assert_eq!(raw["entries"][0]["source"]["msgid"], "Hello");
    assert_eq!(raw["entries"][1]["source"]["msgid"], "World");
    assert_eq!(raw["entries"].as_array().unwrap().len(), 2);

    let (before_item, before_job) = {
        let conn = db.lock().await;
        conn.execute(
            "UPDATE translation_jobs SET total_items = 2 WHERE id = ?1",
            rusqlite::params![job_id],
        )
        .unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_language_pack_path BEFORE UPDATE OF translated_path ON translation_items
             BEGIN SELECT RAISE(ABORT, 'fixture language-pack path failure'); END;",
        ).unwrap();
        (
            serde_json::to_value(crate::db::jobs::get_item(&conn, persisted.item_id).unwrap())
                .unwrap(),
            serde_json::to_value(crate::db::jobs::get_job(&conn, job_id).unwrap()).unwrap(),
        )
    };
    assert!(persist_language_pack_batch_for_sync(
        &db,
        job_id,
        dir.path().to_str().unwrap(),
        "https-example.com",
        "https://example.com",
        &relation,
        &effective_relation,
        "plugin_i18n",
        "plugin",
        &source_items,
        &payload.entries,
        "lang-pack-batch",
        Some("route_test"),
        &payload,
        "comp-other",
        &task_params,
    )
    .await
    .is_err());
    let conn = db.lock().await;
    assert_eq!(
        serde_json::to_value(crate::db::jobs::get_item(&conn, persisted.item_id).unwrap()).unwrap(),
        before_item,
    );
    assert_eq!(
        serde_json::to_value(crate::db::jobs::get_job(&conn, job_id).unwrap()).unwrap(),
        before_job,
    );
    assert_eq!(
        crate::db::jobs::list_items_by_job(&conn, job_id, None)
            .unwrap()
            .len(),
        1
    );
    assert!(
        std::path::Path::new(&persisted.translated_path).exists(),
        "paid snapshot must remain"
    );
}

#[tokio::test]
async fn persist_language_pack_batch_for_sync_preserves_context_and_plural_metadata() {
    let _key = crate::db::owned_mock_bindings_key();
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(":memory:").expect("db"),
    ));
    let job_id = {
        let conn = db.lock().await;
        crate::db::jobs::create_job(
            &conn,
            &crate::db::jobs::CreateJobRequest {
                domain: "https://example.com".to_string(),
                relation_id: 7,
                business_line: "discovery".to_string(),
                triggered_by: "auto".to_string(),
            },
        )
        .unwrap()
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let relation = sample_relation();
    let task_params = DiscoveryTaskParams::default();
    let payload = I18nCallbackPayload {
        business_line: "plugin_i18n".to_string(),
        relation_id: 7,
        client_task_id: "lang-pack-meta".to_string(),
        worker_id: "worker-test".to_string(),
        source_lang: relation.source_lang.clone(),
        target_lang: relation.target_lang.clone(),
        entries: vec![I18nCallbackEntry {
            entry_id: 301,
            msgstr: "两篇文章".to_string(),
        }],
    };
    let source_items = vec![LanguagePackItem {
        object_id: 91,
        text_domain: "my-plugin".to_string(),
        complete_data: LanguagePackCompleteData {
            entry_id: 301,
            msgid: "%d post".to_string(),
            msgctxt: "dashboard counter".to_string(),
            msgid_plural: "%d posts".to_string(),
            plural_index: Some(1),
            text_domain: "my-plugin".to_string(),
        },
    }];

    let persisted = persist_language_pack_batch_for_sync(
        &db,
        job_id,
        dir.path().to_string_lossy().as_ref(),
        "https-example.com",
        "https://example.com",
        &relation,
        &relation,
        "plugin_i18n",
        "plugin",
        &source_items,
        &payload.entries,
        "lang-pack-meta",
        Some("route_test"),
        &payload,
        "comp-selected",
        &task_params,
    )
    .await
    .unwrap();

    let raw_path = {
        let conn = db.lock().await;
        crate::db::jobs::get_item(&conn, persisted.item_id)
            .expect("item")
            .raw_path
    };
    let raw: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&raw_path)).unwrap(),
    )
    .unwrap();

    assert_eq!(raw["entries"][0]["source"]["msgid"], "%d post");
    assert_eq!(raw["entries"][0]["source"]["msgctxt"], "dashboard counter");
    assert_eq!(raw["entries"][0]["source"]["msgid_plural"], "%d posts");
    assert_eq!(raw["entries"][0]["source"]["plural_index"], 1);
}

// ---------------------------------------------------------------------------
// FL-7/FL-2b domain circuits + outbox re-offer hold (Wave-2)
// ---------------------------------------------------------------------------

async fn spawn_claim_404_server(relations_body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = Arc::clone(&requests);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0u8; 8192];
            let Ok(n) = socket.read(&mut request).await else {
                continue;
            };
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("").to_string();
            requests_for_server.lock().await.push(first_line.clone());

            let (status, body) = if first_line.contains("/site-relations") {
                ("200 OK", relations_body.to_string())
            } else if first_line.contains("/content-changes") {
                (
                    "200 OK",
                    r#"{"success":true,"data":{"items":[],"schema_version":1}}"#.to_string(),
                )
            } else if first_line.contains("/content/claim") {
                // The route is MISSING on this (mock) site: permanent 404.
                (
                    "404 Not Found",
                    r#"{"code":"rest_no_route","message":"No route was found"}"#.to_string(),
                )
            } else if first_line.contains("/rules?relation_id=22") {
                ("200 OK", r#"{"rules":[]}"#.to_string())
            } else if first_line.contains("/content?relation_id=22") {
                (
                    "200 OK",
                    r#"{"items":[{"object_type":"post","subtype":"post","object_id":7001,"complete_data":{"post_title":"Circuit title","post_content":"Circuit body"}}],"total":1,"page":1,"per_page":20}"#
                        .to_string(),
                )
            } else {
                (
                    "404 Not Found",
                    format!(r#"{{"unexpected_request":"{}"}}"#, first_line),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    (format!("http://{}", addr), requests)
}

async fn spawn_reoffer_server(relations_body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = Arc::clone(&requests);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0u8; 8192];
            let Ok(n) = socket.read(&mut request).await else {
                continue;
            };
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("").to_string();
            requests_for_server.lock().await.push(first_line.clone());

            // This (mock) site re-offers the SAME outbox row on every list —
            // it accepts acks but never settles the row (ignores
            // available_at / completed marks): the FL-2b re-offer storm.
            let outbox_body = r#"{"success":true,"data":{"items":[{"relation_id":21,"outbox_id":501,"task_id":5,"client_task_id":"ct-reoffer","item":{"object_type":"post","subtype":"post","object_id":6001,"complete_data":{"post_title":"Reoffer title","post_content":"Reoffer body"}}}],"schema_version":1}}"#;
            let (status, body) = if first_line.contains("/site-relations") {
                ("200 OK", relations_body.to_string())
            } else if first_line.contains("/content-changes/501/ack") {
                ("200 OK", r#"{"success":true}"#.to_string())
            } else if first_line.contains("/content-changes") {
                ("200 OK", outbox_body.to_string())
            } else if first_line.contains("/rules?relation_id=21") {
                ("200 OK", r#"{"rules":[]}"#.to_string())
            } else if first_line.contains("/content?relation_id=21") {
                (
                    "200 OK",
                    r#"{"items":[],"total":0,"page":1,"per_page":20}"#.to_string(),
                )
            } else {
                (
                    "404 Not Found",
                    format!(r#"{{"unexpected_request":"{}"}}"#, first_line),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    (format!("http://{}", addr), requests)
}

async fn run_discover_against(
    wp_base: &str,
    log_file: &str,
    binding_identity: crate::types::PluginIdentity,
) -> anyhow::Result<DomainRunReport> {
    let temp_db = Arc::new(Mutex::new(crate::db::open_db(":memory:").expect("db")));
    let pending_store = Arc::new(Mutex::new(
        PendingCallbackStore::open(":memory:").expect("pending store"),
    ));
    let client = Client::new();
    let worker_config = discovery_test_worker_config();
    let prefer_ids: Vec<String> = Vec::new();
    discover_and_translate(
        &client,
        wp_base,
        "token-test",
        log_file,
        None,
        None,
        "",
        &prefer_ids,
        None,
        None,
        &worker_config,
        Some("secret"),
        &pending_store,
        None,
        None,
        None,
        Some(temp_db),
        binding_identity,
    )
    .await
}

#[tokio::test]
async fn claim_route_404_opens_scan_circuit_but_outbox_lane_keeps_flowing() {
    // The backoff registry is process-global: hold the shared test lock so
    // a concurrent backoff unit test's clear_all() cannot wipe this test's
    // scan circuit between its two passes.
    let _backoff_lock = crate::task_engine::backoff::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let relations_body = r#"{"relations":[{"id":22,"source_site_id":1,"source_lang":"en_US","target_site_id":2,"target_site_type":"virtual","target_lang":"zh_CN","sync_mode":"push","models":[]}]}"#;
    let (base, requests) = spawn_claim_404_server(relations_body).await;
    let wp_base = format!("{}/wp-json/wptsall/v2/secret/client", base);
    let log_file = temp.path().join("worker-a.log").display().to_string();

    // Pass 1: the claim route 404s (permanent) — the scan attempts it once.
    run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::WpmmccAts)
        .await
        .expect("pass 1 should complete despite the claim 404");
    {
        let seen = requests.lock().await.clone();
        assert!(
            seen.iter().any(|line| line.contains("/content/claim")),
            "pass 1 must attempt the claim once: {seen:?}"
        );
    }
    assert!(
        crate::task_engine::backoff::check_domain_scan(&wp_base).blocked,
        "permanent claim-route 404 must open the scan circuit"
    );
    assert!(
        !crate::task_engine::backoff::check_domain(&wp_base).blocked,
        "a scan-lane 404 must NOT trip the whole-domain (outbox) circuit"
    );

    let seen_after_pass1 = requests.lock().await.len();

    // Pass 2: the scan circuit is open — no /content page fetch, no claim
    // POST — but the outbox lane (site-relations + content-changes) keeps
    // flowing: a claim-route gap must not stop lifecycle events.
    run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::WpmmccAts)
        .await
        .expect("pass 2 should complete with the scan circuit open");
    {
        let seen = requests.lock().await.clone();
        let fresh = &seen[seen_after_pass1..];
        assert!(
            fresh.iter().any(|line| line.contains("/site-relations")),
            "pass 2 must still fetch site-relations (lane alive): {fresh:?}"
        );
        assert!(
            fresh.iter().any(|line| line.contains("/content-changes")),
            "pass 2 must still drain the outbox lane: {fresh:?}"
        );
        assert!(
            !fresh
                .iter()
                .any(|line| line.contains("/content?relation_id=22")),
            "pass 2 must skip the scan pages while the circuit is open: {fresh:?}"
        );
        assert!(
            !fresh.iter().any(|line| line.contains("/content/claim")),
            "pass 2 must not re-attempt the missing claim route: {fresh:?}"
        );
    }
}

// STA-02 (ISS gap doc, batch H 2026-09-22): the option claim lane journey.
// Pre-fix the option data type fell into the else branch and logged
// claim_not_required_for_data_type — the WP callback then rejected every
// option translation with claim_required, and dual clients raced on
// options with no lease at all. This journey pins the closed loop: the
// scan lane claims option items (option_name contract, crc32 synthetic
// object_id) BEFORE any translation work happens.
async fn spawn_option_claim_server(
    relations_body: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let claim_bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = Arc::clone(&requests);
    let claim_bodies_for_server = Arc::clone(&claim_bodies);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0u8; 8192];
            let Ok(n) = socket.read(&mut request).await else {
                continue;
            };
            let request_text = String::from_utf8_lossy(&request[..n]);
            let first_line = request_text.lines().next().unwrap_or("").to_string();
            if first_line.contains("/content/claim") {
                let body = request_text
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default();
                claim_bodies_for_server.lock().await.push(body);
            }
            requests_for_server.lock().await.push(first_line.clone());

            // Mirrors the WP contract: option discovery emits items whose
            // object_id is crc32("option:" + name) made positive, and the
            // claim endpoint echoes {object_id, post_type, option_name}.
            let (status, body) = if first_line.contains("/site-relations") {
                ("200 OK", relations_body.to_string())
            } else if first_line.contains("/content-changes") {
                (
                    "200 OK",
                    r#"{"success":true,"data":{"items":[],"schema_version":1}}"#.to_string(),
                )
            } else if first_line.contains("/content/claim") {
                (
                    "200 OK",
                    r#"{"claimed_count":1,"claimed_items":[{"object_id":3645736316,"post_type":"wptsall_mail_template","option_name":"wptsall_mail_template"}]}"#
                        .to_string(),
                )
            } else if first_line.contains("/rules?relation_id=23") {
                (
                    "200 OK",
                    r#"{"rules":[{"id":1,"model_id":1,"data_type":"option","object_name":"wptsall_mail_template"}]}"#
                        .to_string(),
                )
            } else if first_line.contains("/content?relation_id=23")
                && first_line.contains("data_type=option")
            {
                (
                    "200 OK",
                    r#"{"items":[{"object_type":"option","subtype":"wptsall_mail_template","object_id":3645736316,"complete_data":{"option_name":"wptsall_mail_template","option_value":"Hello"}}],"total":1,"page":1,"per_page":20}"#
                        .to_string(),
                )
            } else if first_line.contains("/content?relation_id=23") {
                // The post lane (defaulted because the relation declares no
                // models) has nothing to offer.
                (
                    "200 OK",
                    r#"{"items":[],"total":0,"page":1,"per_page":20}"#.to_string(),
                )
            } else {
                (
                    "404 Not Found",
                    format!(r#"{{"unexpected_request":"{}"}}"#, first_line),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    (format!("http://{}", addr), requests, claim_bodies)
}

#[tokio::test]
async fn option_discovery_claims_items_before_translation_instead_of_skipping() {
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    // Share the logging tests' global lock: this test reads info lines
    // (discovery.content_claimed) off disk, so LOG_ENABLED must be flipped
    // under the shared serialization lock.
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(true, "info");
    let relations_body = r#"{"relations":[{"id":23,"source_site_id":1,"source_lang":"en_US","target_site_id":2,"target_site_type":"virtual","target_lang":"zh_CN","sync_mode":"push","models":[]}]}"#;
    let (base, _requests, claim_bodies) = spawn_option_claim_server(relations_body).await;
    let wp_base = format!("{}/wp-json/wptsall/v2/secret/client", base);
    let log_file = temp.path().join("worker-option.log").display().to_string();

    run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::WpmmccAts)
        .await
        .expect("option discovery run should complete");

    // The claim POST carries the option contract exactly as WP's
    // claim_option_entries validates it: option_name key (not post_type)
    // and the crc32("option:" + name) synthetic object_id.
    let bodies = claim_bodies.lock().await.clone();
    let option_claim = bodies
        .iter()
        .find(|body| body.contains(r#""data_type":"option""#))
        .expect("option lane must POST a claim");
    assert!(
        option_claim.contains(r#""option_name":"wptsall_mail_template""#),
        "claim items must use the option_name key: {option_claim}"
    );
    assert!(
        option_claim.contains(r#""object_id":3645736316"#),
        "claim items must carry the crc32 synthetic id: {option_claim}"
    );

    // Info lines are buffered: flush before reading the file.
    crate::logging::flush_log();
    // The claim succeeded and the item survived the retain filter.
    let log_text = std::fs::read_to_string(&log_file).unwrap_or_default();
    assert!(
        log_text.contains("discovery.content_claimed"),
        "claimed event missing from log: {log_text}"
    );
    // Pre-fix behavior: the option lane logged
    // claim_not_required_for_data_type and never claimed at all.
    let option_skipped = log_text.lines().any(|line| {
        line.contains("claim_not_required_for_data_type") && line.contains(r#""option""#)
    });
    assert!(
        !option_skipped,
        "option lane must not fall back to the claim-not-required branch: {log_text}"
    );
}

#[tokio::test]
async fn outbox_reoffer_hold_makes_second_pass_skip_acked_row() {
    // Same shared backoff-registry lock: the hold armed in pass 1 must
    // survive into pass 2 without a concurrent clear_all() wiping it.
    let _backoff_lock = crate::task_engine::backoff::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    let relations_body = r#"{"relations":[{"id":21,"source_site_id":1,"source_lang":"en_US","target_site_id":2,"target_site_type":"virtual","target_lang":"zh_CN","sync_mode":"push","models":[]}]}"#;
    let (base, requests) = spawn_reoffer_server(relations_body).await;
    let wp_base = format!("{}/wp-json/wptsall/v2/secret/client", base);
    let log_file = temp.path().join("worker-b.log").display().to_string();

    // Pass 1: the row executes once (no component → failure → retry-ack with
    // backoff hint) and the re-offer hold is armed for the hinted window.
    let report1 =
        run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::WpmmccAts)
            .await
            .expect("pass 1 should complete");
    assert_eq!(report1.processed, 1, "pass 1 executes the row once");
    {
        let seen = requests.lock().await.clone();
        let acks = seen
            .iter()
            .filter(|line| line.contains("/content-changes/501/ack"))
            .count();
        assert_eq!(acks, 1, "pass 1 acks the row exactly once: {seen:?}");
    }
    assert!(
        crate::task_engine::backoff::outbox_hold_remaining(&wp_base, 501) > 0,
        "the retry-ack must arm the re-offer hold"
    );

    let seen_after_pass1 = requests.lock().await.len();

    // Pass 2: the site re-offers the SAME row, but the hold skips it — no
    // re-execution, no second ack. This is the run-once convergence guard:
    // a site that ignores acks/available_at cannot keep the loop burning
    // iterations on one row.
    let report2 =
        run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::WpmmccAts)
            .await
            .expect("pass 2 should complete");
    assert_eq!(
        report2.processed, 0,
        "pass 2 must not re-execute the held row"
    );
    assert_eq!(
        report2.pulled, 1,
        "the held row is counted as pulled/skipped"
    );
    {
        let seen = requests.lock().await.clone();
        let fresh = &seen[seen_after_pass1..];
        assert!(
            fresh.iter().any(|line| line.contains("/content-changes?")),
            "pass 2 still lists the outbox (lane alive): {fresh:?}"
        );
        let acks_total = seen
            .iter()
            .filter(|line| line.contains("/content-changes/501/ack"))
            .count();
        assert_eq!(
            acks_total, 1,
            "the held row must not be re-acked in pass 2: {seen:?}"
        );
    }

    // Cross-domain regression (SIM-lane FL-2b bug): outbox ids are per-site
    // integers, so a different site reuses the SAME id 501. The first site's
    // active hold must not suppress the second site's fresh row — that
    // exactly reproduced the sim-04/sim-07/sim-08/sim-13 failures (every
    // mock site numbers its outbox from 7000, so earlier journeys' holds
    // silenced later sites entirely).
    let (base2, _requests2) = spawn_reoffer_server(relations_body).await;
    let wp_base2 = format!("{}/wp-json/wptsall/v2/secret/client", base2);
    let report3 = run_discover_against(
        &wp_base2,
        &log_file,
        crate::types::PluginIdentity::WpmmccAts,
    )
    .await
    .expect("cross-domain pass should complete");
    assert_eq!(
        report3.processed, 1,
        "another domain's row with the same outbox id must NOT inherit the hold"
    );
    let d2_hold = crate::task_engine::backoff::outbox_hold_remaining(&wp_base2, 501);
    let d1_hold = crate::task_engine::backoff::outbox_hold_remaining(&wp_base, 501);
    assert!(
        d2_hold > 0,
        "the second domain arms its OWN hold from its own ack (d2={d2_hold})"
    );
    assert!(
        d1_hold > 0,
        "the first domain's hold is still active (unaffected by domain 2; d1={d1_hold}, d2={d2_hold})"
    );
}

#[tokio::test]
async fn lane_entry_identity_mismatch_fails_closed_and_warns_once() {
    let _transport_guard = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let temp = tempfile::tempdir().expect("temp dir");
    let _data_dir_guard = crate::db::TestEnvVarGuard::set(
        "WPTSALL_DATA_DIR",
        temp.path().join("data").display().to_string(),
    );
    // Share the logging tests' global lock: this test flips LOG_ENABLED to
    // read the guard's warn lines off disk, and that global mutation must
    // stay serialized with logging::tests.
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(true, "info");

    // No mock server: the lane-entry guard must return BEFORE any network
    // I/O, so an unroutable loopback base proves fail-closed-by-design (a
    // dispatch attempt here would fail slowly, not return empty fast).
    let wp_base = "http://127.0.0.1:1/wp-json/wptsall/v2/none/client".to_string();
    let log_file = temp.path().join("guard.log").display().to_string();

    // Two calls = the pre-FL-3 storm shape (a standing wpmmcc binding
    // behind a multi-iteration run-once loop). Both must fail closed with
    // an EMPTY report; the identity_mismatch warn fires exactly ONCE.
    let report1 = run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::Wpmmcc)
        .await
        .expect("guard pass 1 should complete");
    let report2 = run_discover_against(&wp_base, &log_file, crate::types::PluginIdentity::Wpmmcc)
        .await
        .expect("guard pass 2 should complete");
    for (label, report) in [("pass 1", &report1), ("pass 2", &report2)] {
        assert_eq!(report.pulled, 0, "{label} pulls nothing");
        assert_eq!(report.processed, 0, "{label} processes nothing");
        assert_eq!(report.completed, 0, "{label} completes nothing");
        assert_eq!(report.failed, 0, "{label} fails nothing");
    }

    // Warn-and-above flush synchronously, so the file is authoritative here.
    let content = std::fs::read_to_string(&log_file).expect("guard log file");
    let mismatches = content
        .lines()
        .filter(|line| line.contains("\"identity_mismatch\""))
        .count();
    assert_eq!(
        mismatches, 1,
        "the standing mismatch warns exactly once across repeated calls; log:\n{content}"
    );
}
