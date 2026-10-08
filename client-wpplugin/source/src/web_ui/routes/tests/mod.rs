use super::*;
use crate::web_ui::AccessControl;
use reqwest::Client;
use serde_json::json;
use std::convert::Infallible;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

fn build_test_web_ui_state(
    server_base: &str,
    session_token: Option<&str>,
) -> Arc<Mutex<WebUiState>> {
    let http_client = Client::builder().no_proxy().build().unwrap();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    // Real DBs always run through open_db → schema::create_tables; a bare
    // in-memory connection silently lacks system_config, which used to hide
    // behind `let _ =` result-swallowing in the settings handlers.
    crate::db::schema::create_tables(&db).unwrap();
    Arc::new(Mutex::new(WebUiState {
        server_base: server_base.to_string(),
        device_id: "device-test".to_string(),
        session_token: session_token.map(|s| s.to_string()),
        oauth_code_verifier: None,
        oauth_state: None,
        domains: Vec::new(),
        components: Vec::new(),
        component_bindings_path: "/tmp/test-component-bindings.json".to_string(),
        component_bindings: ComponentBindingsDoc::default(),
        domain_token_bindings_path: "/tmp/test-domain-token-bindings.json".to_string(),
        domain_token_bindings: DomainTokenBindingsDoc::default(),
        task_type_component_bindings_path: "/tmp/test-task-type-bindings.json".to_string(),
        task_type_component_bindings: TaskTypeComponentBindingsDoc::default(),
        rule_component_bindings_path: "/tmp/test-rule-bindings.json".to_string(),
        rule_component_bindings: RuleComponentBindingsDoc::default(),
        worker_loop_running: false,
        worker_status: "idle".to_string(),
        worker_loop_poll_seconds: 20,
        worker_last_summary: json!({}),
        worker_recent_runs: Vec::new(),
        local_components_backfilled: 0,
        local_components_backfill_error: String::new(),
        last_error: String::new(),
        last_event: "test.ready".to_string(),
        updated_at: 0,
        log_enabled: false,
        log_min_level: "info".to_string(),
        vendor_oauth_pending: std::collections::HashMap::new(),
        db: Arc::new(Mutex::new(db)),
        http_client,
        update_in_progress: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }))
}

struct ComponentsEnvLock {
    inner: StdMutex<()>,
}

struct ComponentsEnvScopeGuard<'a> {
    _web_ui_guard: EnvVarGuard,
    _bindings_key_guard: EnvVarGuard,
    _data_dir_guard: EnvVarGuard,
    _data_root: tempfile::TempDir,
    _lock_guard: std::sync::MutexGuard<'a, ()>,
}

impl ComponentsEnvLock {
    fn lock(&self) -> Result<ComponentsEnvScopeGuard<'_>, Infallible> {
        let lock_guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Keep tests deterministic even if shell/service exports WPTSALL_WEB_UI=true.
        // Tests that need sqlite runtime mode can still override this explicitly.
        let web_ui_guard = EnvVarGuard::set("WPTSALL_WEB_UI", "false".to_string());
        let bindings_key_guard = crate::db::owned_mock_bindings_key();
        // A lock does not make configuration left by an earlier fixture ours.
        // Strict readers must not see its files or encryption identity.
        let data_root = tempfile::tempdir().expect("owned component configuration root");
        let data_dir_guard =
            EnvVarGuard::set("WPTSALL_DATA_DIR", data_root.path().display().to_string());
        Ok(ComponentsEnvScopeGuard {
            _web_ui_guard: web_ui_guard,
            _bindings_key_guard: bindings_key_guard,
            _data_dir_guard: data_dir_guard,
            _data_root: data_root,
            _lock_guard: lock_guard,
        })
    }
}

fn components_env_lock() -> &'static ComponentsEnvLock {
    static LOCK: OnceLock<ComponentsEnvLock> = OnceLock::new();
    LOCK.get_or_init(|| ComponentsEnvLock {
        inner: StdMutex::new(()),
    })
}

type EnvVarGuard = crate::db::TestEnvVarGuard;

fn mark_json_migration_done(conn: &rusqlite::Connection) {
    conn.execute(
        "INSERT OR REPLACE INTO system_config (key, value) VALUES ('json_migration_done', '1')",
        [],
    )
    .unwrap();
}

async fn seed_translation_item_for_web_ui_test(
    state: &Arc<Mutex<WebUiState>>,
    domain: &str,
    translated_path: &str,
    status: &str,
    client_task_id: &str,
) -> i64 {
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    let conn = db_arc.lock().await;
    crate::db::schema::create_tables(&conn).unwrap();
    let job_id = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: domain.to_string(),
            relation_id: 1,
            business_line: "post_content".to_string(),
            triggered_by: "manual".to_string(),
        },
    )
    .unwrap();
    let item_id = crate::db::jobs::create_item(
        &conn,
        &crate::db::jobs::CreateItemRequest {
            job_id,
            domain: domain.to_string(),
            relation_id: 1,
            business_line: "post_content".to_string(),
            object_type: "post".to_string(),
            wp_object_id: 88,
            wp_object_subtype: "post".to_string(),
            task_type: "text".to_string(),
            source_lang: "en_US".to_string(),
            target_lang: "zh_CN".to_string(),
            component_id: "comp-test".to_string(),
            component_ids: vec!["comp-test".to_string()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: "/tmp/raw-test.json".to_string(),
            client_task_id: client_task_id.to_string(),
            max_retries: 3,
        },
    )
    .unwrap();
    crate::db::jobs::update_item_translated_path(&conn, item_id, translated_path).unwrap();
    crate::db::jobs::update_item_status(&conn, item_id, status, None).unwrap();
    item_id
}

fn write_test_translation_envelope(path: &str, client_task_id: &str) {
    let envelope = json!({
        "idempotency_key": format!("idem-{}", client_task_id),
        "payload": {
            "schema_version": crate::config::TASK_CALLBACK_SCHEMA_VERSION,
            "relation_id": 1,
            "business_line": "post_content",
            "object_type": "post",
            "post_type": "post",
            "object_id": 88,
            "translated_fields": {
                "post_title": "你好"
            },
            "translated_meta": {},
            "media_mappings": [],
            "client_task_id": client_task_id,
            "worker_id": "device-test",
            "source_lang": "en_US",
            "target_lang": "zh_CN",
            "execution_time_ms": 12
        }
    });
    std::fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
}

fn parse_http_json_body(response: &str) -> serde_json::Value {
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or("");
    serde_json::from_str(body).unwrap()
}

fn make_test_server_component(
    template_id: &str,
    component_type: &str,
    supported_type: &str,
) -> ComponentItem {
    serde_json::from_value(json!({
        "id": template_id,
        "name": format!("{} template", template_id),
        "type": component_type,
        "supported_types": [supported_type],
        "vendor_id": "test-vendor",
        "version": "1.0.0",
        "status": "active",
        "updated_at": "2026-03-07T00:00:00Z"
    }))
    .expect("valid server component")
}

// -----------------------------------------------------------------------
// CSRF Origin check tests (audit fix: CSRF adapted for external mode)
// -----------------------------------------------------------------------

mod access_guards;
mod auth;
mod bindings;
mod bindings_local_first;
mod components;
mod components_local_first;
mod csrf;
mod endpoint_coverage;
mod integrations;
mod job_accounting;
mod legacy_routes;
mod media_recovery;
mod operations;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/physical_capacity_routes.rs"]
mod physical_capacity_routes;
mod provider_catalog;
mod provider_recovery;
#[path = "../../../../../../tests/modules/client-wpplugin/unit/retained_state_reads.rs"]
mod retained_state_reads;
mod review;
mod review_ownership;
mod rule_discovery_local;
mod settings;
mod site_connections;
mod status;
mod sync_pairs;
mod sync_review;
mod worker;

#[tokio::test]
async fn csrf_no_origin_allowed_in_local_mode() {
    // Local mode: loopback-bound server; local tooling and the test lanes
    // speak plain HTTP without browser headers.
    let ac = AccessControl::new(false, &[]);
    assert!(check_csrf_origin(None, &ac).await);
}

#[tokio::test]
async fn csrf_no_origin_rejected_in_external_mode() {
    // S1/Y-2 (tasks/5.3falsh2/12 批 A1): external mode must fail closed — a
    // non-browser client that simply strips the Origin header cannot bypass
    // the CSRF face.
    let ip: std::net::IpAddr = "192.168.1.100".parse().unwrap();
    let ac = AccessControl::new(true, &[ip]);
    assert!(!check_csrf_origin(None, &ac).await);
}

#[tokio::test]
async fn csrf_localhost_origins_allowed() {
    let ac = AccessControl::new(false, &[]);
    let allowed = vec![
        "http://127.0.0.1:8977",
        "http://localhost:8977",
        "https://127.0.0.1:8977",
        "https://localhost:3000",
        "http://127.0.0.1",
        "http://localhost",
    ];
    for origin in allowed {
        assert!(
            check_csrf_origin(Some(origin), &ac).await,
            "expected allowed for origin: {}",
            origin
        );
    }
}

#[tokio::test]
async fn csrf_external_ip_rejected_when_not_external_mode() {
    let ac = AccessControl::new(false, &[]);
    assert!(!check_csrf_origin(Some("http://192.168.1.100:8977"), &ac).await);
}

#[tokio::test]
async fn csrf_external_whitelisted_ip_allowed() {
    let ip: std::net::IpAddr = "192.168.1.100".parse().unwrap();
    let ac = AccessControl::new(true, &[ip]);
    assert!(check_csrf_origin(Some("http://192.168.1.100:8977"), &ac).await);
}

#[tokio::test]
async fn csrf_external_non_whitelisted_ip_rejected() {
    let ip: std::net::IpAddr = "192.168.1.100".parse().unwrap();
    let ac = AccessControl::new(true, &[ip]);
    assert!(!check_csrf_origin(Some("http://192.168.1.200:8977"), &ac).await);
}

#[tokio::test]
async fn csrf_hostname_origin_rejected() {
    // Non-IP hostnames are always rejected (even in external mode)
    let ip: std::net::IpAddr = "10.0.0.1".parse().unwrap();
    let ac = AccessControl::new(true, &[ip]);
    assert!(!check_csrf_origin(Some("http://evil.com"), &ac).await);
    assert!(!check_csrf_origin(Some("http://my-server.local:8977"), &ac).await);
}

#[tokio::test]
async fn csrf_malformed_origin_rejected() {
    let ac = AccessControl::new(true, &[]);
    assert!(!check_csrf_origin(Some("not-a-url"), &ac).await);
    assert!(!check_csrf_origin(Some(""), &ac).await);
}

#[tokio::test]
async fn csrf_prefix_spoof_origins_rejected() {
    // S1 (tasks/5.3falsh2/12 批 A1): `starts_with` prefix matching let
    // attacker domains that merely START with the loopback literal through;
    // exact host equality closes it.
    let ac = AccessControl::new(false, &[]);
    for origin in [
        "http://127.0.0.1.evil.com",
        "http://localhost.evil.com",
        "https://127.0.0.1.evil.com",
        "http://127.0.0.1@evil.com", // userinfo trick: real host is evil.com
        "http://localhost:8080.evil.com", // non-numeric port
        "http://127.0.0.1:8977.evil.com",
    ] {
        assert!(
            !check_csrf_origin(Some(origin), &ac).await,
            "spoofed origin must be rejected: {}",
            origin
        );
    }
    // Exact loopback hosts (any port, either scheme, any case) stay allowed.
    for origin in ["http://127.0.0.1:8977", "http://LOCALHOST:3000"] {
        assert!(
            check_csrf_origin(Some(origin), &ac).await,
            "exact loopback origin must be allowed: {}",
            origin
        );
    }
}

#[tokio::test]
async fn csrf_ipv6_loopback_origin_allowed() {
    let ac = AccessControl::new(false, &[]);
    assert!(check_csrf_origin(Some("http://[::1]:8977"), &ac).await);
    assert!(check_csrf_origin(Some("http://[::1]"), &ac).await);
    // Mapped-v4 of loopback is not the ::1 literal — rejected (conservative,
    // same verdict as before the fix).
    assert!(!check_csrf_origin(Some("http://[::ffff:127.0.0.1]:8977"), &ac).await);
}

#[test]
fn component_exists_in_local_doc_uses_normalized_id() {
    let doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-a": {
                "name": "Comp A",
                "template_id": "tpl-a",
                "kind": "text",
                "created_at": "1"
            }
        }
    }))
    .expect("valid components local doc");

    assert!(component_exists_in_local_doc(&doc, "comp-a"));
    assert!(component_exists_in_local_doc(&doc, "  comp-a  "));
    assert!(!component_exists_in_local_doc(&doc, ""));
    assert!(!component_exists_in_local_doc(&doc, "comp-missing"));
}

#[tokio::test]
async fn validate_local_component_runtime_ready_for_task_rejects_disabled_component() {
    let _env_guard = components_env_lock().lock().unwrap();
    let components_path = format!(
        "/tmp/wptsall-local-components-disabled-{}.json",
        uuid::Uuid::new_v4()
    );
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let local_doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-disabled": {
                "name": "Disabled Component",
                "template_id": "official-text-v1",
                "kind": "text",
                "enabled": false,
                "created_at": "1",
                "versions": {},
                "template_json": {
                    "id": "official-text-v1",
                    "name": "Disabled Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/translate",
                        "body": {}
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .expect("valid local components doc");
    crate::bindings::save_components_local(&components_path, &local_doc).unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let err = validate_local_component_runtime_ready_for_task(&state, "comp-disabled", None)
        .await
        .expect_err("disabled local component must be rejected before task save");
    assert!(
        format!("{:#}", err).contains("disabled"),
        "unexpected error: {:#}",
        err
    );

    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn build_local_component_runtime_for_task_accepts_version_key_auth() {
    let _env_guard = components_env_lock().lock().unwrap();
    let config_dir = format!("/tmp/wptsall-runtime-version-auth-{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(&config_dir).unwrap();
    let components_path = format!("{}/components-local.json", config_dir);
    let vendor_keys_path = format!("{}/vendor-keys.json", config_dir);
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _vendor_keys_guard = EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", vendor_keys_path.clone());

    let local_doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-version-auth": {
                "name": "Wizard Component",
                "template_id": "official-text-v1",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "active_version": "v1",
                "versions": {
                    "v1": {
                        "version": "v1",
                        "remarks": "",
                        "key_ids": ["vk-wizard-1"],
                        "key_selection_strategy": "round_robin",
                        "auth_type": "key",
                        "config_overrides": {},
                        "created_at": "1"
                    }
                },
                "template_json": {
                    "id": "official-text-v1",
                    "name": "Wizard Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "auth": {
                        "fields": [
                            { "name": "api_key", "required": true, "secret": true }
                        ]
                    },
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/translate",
                        "body": {}
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .expect("valid local components doc");
    crate::bindings::save_components_local(&components_path, &local_doc).unwrap();

    let keys_doc: crate::types::VendorKeysDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "keys": {
            "vk-wizard-1": {
                "vendor_id": "",
                "label": "wizard key",
                "auth_values": { "api_key": "version-secret" },
                "enabled": true
            }
        }
    }))
    .expect("valid vendor keys doc");
    crate::bindings::save_vendor_keys(&vendor_keys_path, &keys_doc).unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    // The legacy component_bindings doc has no entry for this component, so
    // auth must be resolved through the selected version's vendor key.
    let runtime = build_local_component_runtime_for_task(&state, "comp-version-auth", None)
        .await
        .expect("wizard-track component with version key auth must pass validation");
    assert_eq!(
        runtime.auth_values.get("auth.api_key").map(String::as_str),
        Some("version-secret"),
        "auth must come from the version-linked vendor key"
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[tokio::test]
async fn validate_local_component_runtime_ready_for_task_rejects_component_without_any_auth() {
    let _env_guard = components_env_lock().lock().unwrap();
    let config_dir = format!("/tmp/wptsall-runtime-no-auth-{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(&config_dir).unwrap();
    let components_path = format!("{}/components-local.json", config_dir);
    let vendor_keys_path = format!("{}/vendor-keys.json", config_dir);
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _vendor_keys_guard = EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", vendor_keys_path.clone());

    let local_doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-no-auth": {
                "name": "Unconfigured Component",
                "template_id": "official-text-v1",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "versions": {
                    "v1": {
                        "version": "v1",
                        "remarks": "",
                        "key_ids": ["vk-missing"],
                        "key_selection_strategy": "round_robin",
                        "auth_type": "key",
                        "config_overrides": {},
                        "created_at": "1"
                    }
                },
                "template_json": {
                    "id": "official-text-v1",
                    "name": "Unconfigured Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "auth": {
                        "fields": [
                            { "name": "api_key", "required": true, "secret": true }
                        ]
                    },
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/translate",
                        "body": {}
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .expect("valid local components doc");
    crate::bindings::save_components_local(&components_path, &local_doc).unwrap();
    // Vendor-key store exists but holds no enabled key for the component.
    crate::bindings::save_vendor_keys(&vendor_keys_path, &crate::types::VendorKeysDoc::default())
        .unwrap();

    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let err = validate_local_component_runtime_ready_for_task(&state, "comp-no-auth", None)
        .await
        .expect_err("component with required auth and no credentials anywhere must be rejected");
    let message = format!("{:#}", err);
    assert!(
        message.contains("no usable auth"),
        "unexpected error: {}",
        message
    );

    let _ = std::fs::remove_dir_all(&config_dir);
}

#[test]
fn find_server_component_by_template_id_uses_normalized_id() {
    let components: Vec<ComponentItem> = serde_json::from_value(serde_json::json!([
        {
            "id": "official-openai-text-v1",
            "type": "text_translation",
            "vendor_id": "openai"
        },
        {
            "id": "official-azure-video-v1",
            "type": "video_translation",
            "vendor_id": "azure"
        }
    ]))
    .expect("valid server components");

    let first = find_server_component_by_template_id(&components, "official-openai-text-v1")
        .expect("component exists");
    assert_eq!(first.id, "official-openai-text-v1");
    assert_eq!(first.vendor_id.as_deref(), Some("openai"));

    let second = find_server_component_by_template_id(&components, "  official-azure-video-v1  ")
        .expect("component exists with normalized id");
    assert_eq!(second.id, "official-azure-video-v1");
    assert_eq!(second.vendor_id.as_deref(), Some("azure"));

    assert!(find_server_component_by_template_id(&components, "").is_none());
    assert!(find_server_component_by_template_id(&components, "missing-template").is_none());
}

#[test]
fn backfill_local_components_from_server_fills_vendor_and_kind() {
    let server_components: Vec<ComponentItem> = serde_json::from_value(serde_json::json!([
        {
            "id": "official-openai-text-v1",
            "type": "text_translation",
            "vendor_id": "official-vendor-openai"
        },
        {
            "id": "official-azure-video-v1",
            "type": "video_translation",
            "vendor_id": "official-vendor-azure"
        }
    ]))
    .expect("valid server components");

    let mut local_doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-text": {
                "name": "Comp Text",
                "template_id": "official-openai-text-v1",
                "vendor_id": "",
                "kind": "text",
                "created_at": "1"
            },
            "comp-video": {
                "name": "Comp Video",
                "template_id": "official-azure-video-v1",
                "vendor_id": "",
                "kind": "text",
                "created_at": "1"
            },
            "comp-openai": {
                "name": "Comp OpenAI",
                "template_id": "official-openai-text-v1",
                "source_template_id": "official-openai-text-v1",
                "source_template_api_version": "1.0.0",
                "vendor_id": "",
                "kind": "openai_compatible",
                "created_at": "1"
            },
            "comp-missing": {
                "name": "Comp Missing",
                "template_id": "missing-template",
                "vendor_id": "",
                "kind": "text",
                "created_at": "1"
            }
        }
    }))
    .expect("valid local components doc");

    let updated = backfill_local_components_from_server(&mut local_doc, &server_components);
    assert_eq!(updated, 3);

    let comp_text = local_doc
        .components
        .get("comp-text")
        .expect("comp-text exists");
    assert_eq!(comp_text.vendor_id, "official-vendor-openai");
    assert_eq!(comp_text.kind, "text");

    let comp_video = local_doc
        .components
        .get("comp-video")
        .expect("comp-video exists");
    assert_eq!(comp_video.vendor_id, "official-vendor-azure");
    assert_eq!(comp_video.kind, "video");

    let comp_openai = local_doc
        .components
        .get("comp-openai")
        .expect("comp-openai exists");
    assert_eq!(comp_openai.vendor_id, "");
    assert_eq!(
        comp_openai.kind, "openai_compatible",
        "openai_compatible kind must remain unchanged"
    );
    assert_eq!(comp_openai.template_id, "");
    assert_eq!(comp_openai.source_template_id, "");
    assert_eq!(comp_openai.source_template_api_version, None);

    let comp_missing = local_doc
        .components
        .get("comp-missing")
        .expect("comp-missing exists");
    assert_eq!(comp_missing.vendor_id, "");
    assert_eq!(comp_missing.kind, "text");
}

#[test]
fn patch_template_snapshot_for_local_kind_sets_runtime_type() {
    let mut template_json = serde_json::json!({
        "id": "official-any-v1",
        "name": "Any Template",
        "type": "text_translation",
        "supported_types": ["text", "image"]
    });

    patch_template_snapshot_for_local_kind(&mut template_json, "video");
    assert_eq!(
        template_json.get("type").and_then(|v| v.as_str()),
        Some("video_translation")
    );
    assert_eq!(
        template_json.get("supported_types"),
        Some(&serde_json::json!(["video"]))
    );
}

#[test]
fn backfill_local_components_from_server_keeps_kind_when_snapshot_exists() {
    let server_components: Vec<ComponentItem> = serde_json::from_value(serde_json::json!([
        {
            "id": "official-azure-video-v1",
            "type": "video_translation",
            "vendor_id": "official-vendor-azure"
        }
    ]))
    .expect("valid server components");

    let mut local_doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-video-snapshot": {
                "name": "Comp Video Snapshot",
                "template_id": "official-azure-video-v1",
                "vendor_id": "",
                "kind": "text",
                "created_at": "1",
                "template_json": {
                    "id": "official-azure-video-v1",
                    "name": "Azure Video",
                    "type": "text_translation",
                    "request": {"method":"POST","url":"https://example.com"},
                    "response": {"translated_text_path":"data.text"}
                }
            }
        }
    }))
    .expect("valid local components doc");

    let updated = backfill_local_components_from_server(&mut local_doc, &server_components);
    assert_eq!(updated, 1, "only vendor_id should be backfilled");

    let comp = local_doc
        .components
        .get("comp-video-snapshot")
        .expect("snapshot component exists");
    assert_eq!(comp.vendor_id, "official-vendor-azure");
    assert_eq!(
        comp.kind, "text",
        "snapshot-backed local component kind must remain independent from server template kind"
    );
}

#[test]
fn local_component_source_fields_roundtrip() {
    let doc: ComponentsLocalDoc = serde_json::from_value(serde_json::json!({
        "version": 1,
        "components": {
            "comp-text": {
                "name": "Comp Text",
                "template_id": "official-openai-text-v1",
                "source_template_id": "official-openai-text-v1",
                "source_template_updated_at": "2026-03-06T12:00:00Z",
                "vendor_id": "openai",
                "vendor_name": "OpenAI",
                "kind": "text",
                "remarks": "snapshot-backed",
                "enabled": true,
                "created_at": "1",
                "updated_at": "2",
                "versions": {},
                "template_json": {
                    "id": "official-openai-text-v1",
                    "name": "OpenAI Text",
                    "type": "text_translation",
                    "supported_types": ["text"],
                    "request": {"method":"POST","url":"https://api.openai.com/v1/chat/completions"},
                    "response": {"translated_text_path":"choices.0.message.content"}
                }
            }
        }
    }))
    .expect("valid local components doc");

    let comp = doc.components.get("comp-text").expect("component exists");
    assert_eq!(comp.source_template_id, "official-openai-text-v1");
    assert_eq!(
        comp.source_template_updated_at.as_deref(),
        Some("2026-03-06T12:00:00Z")
    );
    assert!(
        comp.template_json.is_some(),
        "snapshot-backed component must persist template_json"
    );
}

#[test]
fn invalid_task_override_paths_respects_template_editable_params() {
    let template: ComponentTemplate = serde_json::from_value(serde_json::json!({
        "id": "official-openai-text-v1",
        "name": "OpenAI Text",
        "version": "1.0.0",
        "type": "text_translation",
        "request": {"method":"POST","url":"https://api.openai.com/v1/chat/completions"},
        "response": {"translated_text_path":"choices.0.message.content"},
        "editable_params": [
            {"path":"default_values.*"},
            {"path":"request.body"},
            {"path":"constraints.rate_limit_qps"}
        ]
    }))
    .expect("valid template");

    let invalid = invalid_task_override_paths(
        &template,
        &serde_json::json!({
            "default_values.system_prompt": "Translate this",
            "request.body.temperature": 0.2,
            "constraints.rate_limit_qps": 5,
            "response.translated_text_path": "bad",
            "auth.api_key": "bad"
        }),
    );

    assert_eq!(
        invalid,
        vec![
            "auth.api_key".to_string(),
            "response.translated_text_path".to_string()
        ]
    );
}

#[test]
fn status_helpers_redact_route_secrets_and_component_credentials() {
    let domains = vec![DomainStatusItem {
        api_base_url: "https://example.test".to_string(),
        site_status: "active".to_string(),
        route_secret: Some("route-secret-must-not-leak".to_string()),
        max_relations: Some(2),
        plan_expires_at: None,
    }];
    let public_domains = redacted_domain_status_items(&domains, &DomainTokenBindingsDoc::default());
    let public_domains_text = serde_json::to_string(&public_domains).unwrap();
    assert!(public_domains_text.contains("route_secret_set"));
    assert!(!public_domains_text.contains("route-secret-must-not-leak"));
    assert_eq!(public_domains[0]["route_secret_set"], true);
    // No binding entry → lane identity stays unset (never a guessed default).
    assert!(public_domains[0]["plugin_identity"].is_null());

    // §67: a bound domain surfaces its lane identity + verification timestamp,
    // while token material stays out of the status projection.
    let mut domain_token_bindings = DomainTokenBindingsDoc::default();
    domain_token_bindings.domains.insert(
        "https://example.test".to_string(),
        DomainTokenBindingEntry {
            wp_client_token: "client-token-must-not-leak".to_string(),
            route_secret: "route-secret-must-not-leak".to_string(),
            plugin_identity: Some(PluginIdentity::WpmmccAts),
            identity_verified_at: Some("2026-09-29T00:00:00Z".to_string()),
            identity_capabilities: None,
        },
    );
    let public_domains = redacted_domain_status_items(&domains, &domain_token_bindings);
    let public_domains_text = serde_json::to_string(&public_domains).unwrap();
    assert_eq!(public_domains[0]["plugin_identity"], "wpmmcc_ats");
    assert_eq!(
        public_domains[0]["identity_verified_at"],
        "2026-09-29T00:00:00Z"
    );
    assert!(!public_domains_text.contains("client-token-must-not-leak"));
    assert!(!public_domains_text.contains("route-secret-must-not-leak"));

    let mut bindings = ComponentBindingsDoc::default();
    bindings.components.insert(
        "component-test".to_string(),
        ComponentBindingEntry {
            auth: [("api_key".to_string(), "sk-live-must-not-leak".to_string())]
                .into_iter()
                .collect(),
            request_overrides: Some(ComponentRequestOverrides {
                method: None,
                url: None,
                headers: Some(
                    [(
                        "Authorization".to_string(),
                        "Bearer must-not-leak".to_string(),
                    )]
                    .into_iter()
                    .collect(),
                ),
                body: Some(serde_json::json!({ "access_token": "token-must-not-leak" })),
            }),
            ..Default::default()
        },
    );
    let public_bindings = redacted_component_bindings(&bindings);
    let public_bindings_text = serde_json::to_string(&public_bindings).unwrap();
    assert!(!public_bindings_text.contains("sk-live-must-not-leak"));
    assert!(!public_bindings_text.contains("Bearer must-not-leak"));
    assert!(!public_bindings_text.contains("token-must-not-leak"));
    assert_eq!(
        public_bindings["components"]["component-test"]["auth"]["api_key"],
        "[redacted]"
    );
}

#[test]
fn invalid_task_override_paths_rejects_blank_path() {
    let template: ComponentTemplate = serde_json::from_value(serde_json::json!({
        "id": "tmpl-1",
        "name": "Template 1",
        "version": "1.0.0",
        "type": "text_translation",
        "request": {"method":"POST","url":"https://example.com"},
        "response": {"translated_text_path":"data.text"},
        "editable_params": [{"path":"default_values.*"}]
    }))
    .expect("valid template");

    let invalid = invalid_task_override_paths(
        &template,
        &serde_json::json!({
            "   ": "bad"
        }),
    );

    assert_eq!(invalid, vec!["   ".to_string()]);
}

#[path = "../../../../../../tests/modules/client-wpplugin/unit/editable_constraint_limits.rs"]
mod editable_constraint_limits;
