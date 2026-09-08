use std::collections::HashMap;
use std::sync::atomic::AtomicI64;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::types::{
    ComponentEditableParam, ComponentRequest, ComponentResponse, ComponentRuntime,
    ComponentTemplate,
};

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
        task_pull_statuses: vec!["pending".to_string()],
        task_concurrency: 1,
        retry_max: 0,
        retry_base_ms: 1,
        retry_max_ms: 1,
        component_fallback_enabled: true,
        default_max_input_chars: 10_000,
        default_split_strategy: "none".to_string(),
        discovery_mode: true,
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
        entries: vec![I18nCallbackEntry {
            entry_id: 101,
            msgstr: "Hallo".to_string(),
        }],
    };
    let source_items = vec![LanguagePackItem {
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
    }];

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
        item.raw_path
    };
    assert!(
        std::path::Path::new(&raw_path).exists(),
        "raw file should exist"
    );

    let translated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&persisted.translated_path).unwrap())
            .unwrap();
    assert_eq!(translated["payload"]["source_lang"], "fr");
    assert_eq!(translated["payload"]["target_lang"], "de");

    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&raw_path).unwrap()).unwrap();
    assert_eq!(raw["source_lang"], "fr");
    assert_eq!(raw["target_lang"], "de");
    assert_eq!(raw["entries"][0]["source"]["msgid"], "Hello");
}

#[tokio::test]
async fn persist_language_pack_batch_for_sync_preserves_context_and_plural_metadata() {
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
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&raw_path).unwrap()).unwrap();

    assert_eq!(raw["entries"][0]["source"]["msgid"], "%d post");
    assert_eq!(raw["entries"][0]["source"]["msgctxt"], "dashboard counter");
    assert_eq!(raw["entries"][0]["source"]["msgid_plural"], "%d posts");
    assert_eq!(raw["entries"][0]["source"]["plural_index"], 1);
}
