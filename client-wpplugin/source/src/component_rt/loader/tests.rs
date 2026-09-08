use super::*;
use reqwest::Client;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type EnvVarGuard = crate::db::TestEnvVarGuard;

fn env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn mark_json_migration_done(conn: &rusqlite::Connection) {
    conn.execute(
        "INSERT OR REPLACE INTO system_config (key, value) VALUES ('json_migration_done', '1')",
        [],
    )
    .unwrap();
}

#[test]
fn signed_revocation_catalog_rejects_missing_signature_without_dev_bypass() {
    let data = ComponentsData {
        items: Vec::new(),
        page: Some(1),
        per_page: Some(200),
        total: Some(0),
        total_pages: Some(1),
        revocation_catalog_version: Some("component-revocation-v1".to_string()),
        revocations: vec![ComponentRevocationItem {
            component_id: "official-revoked-v1".to_string(),
            version: "1.0.0".to_string(),
            content_digest: "a".repeat(64),
            key_id: "server-key".to_string(),
            revoked_at: "2026-08-27T00:00:00Z".to_string(),
            reason: "test".to_string(),
        }],
        revocation_signature: None,
        revocation_signing_key_id: Some("server-key".to_string()),
        revocation_signature_algorithm: Some("RSA-PKCS1v15-RAW-SHA256".to_string()),
        revocation_signature_scope: Some("component-revocation-json-v1".to_string()),
        revocation_signature_contract_version: Some("component-signature-v1".to_string()),
    };
    let err = verified_revoked_component_ids(&data, None, false)
        .expect_err("unsigned revocation catalog must be rejected");
    assert!(format!("{err:#}").contains("signature is missing"));
}

#[test]
fn signed_revocation_catalog_accepts_a_valid_signature() {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::signature::{SignatureEncoding, SignerMut};

    let revocations = vec![ComponentRevocationItem {
        component_id: "official-revoked-v1".to_string(),
        version: "1.0.0".to_string(),
        content_digest: "b".repeat(64),
        key_id: "server-key".to_string(),
        revoked_at: "2026-08-27T00:00:00Z".to_string(),
        reason: "test".to_string(),
    }];
    let payload = serde_json::to_vec(&revocations).unwrap();
    let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let public_pem = rsa::RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let mut signing_key = SigningKey::<sha2::Sha256>::new_unprefixed(private);
    let signature = B64.encode(signing_key.sign(&payload).to_bytes());
    let data = ComponentsData {
        items: Vec::new(),
        page: Some(1),
        per_page: Some(200),
        total: Some(0),
        total_pages: Some(1),
        revocation_catalog_version: Some("component-revocation-v1".to_string()),
        revocations,
        revocation_signature: Some(signature),
        revocation_signing_key_id: Some("server-key".to_string()),
        revocation_signature_algorithm: Some("RSA-PKCS1v15-RAW-SHA256".to_string()),
        revocation_signature_scope: Some("component-revocation-json-v1".to_string()),
        revocation_signature_contract_version: Some("component-signature-v1".to_string()),
    };

    let ids = verified_revoked_component_ids(&data, Some(&public_pem), false).unwrap();
    assert!(ids.contains("official-revoked-v1"));
}

// -----------------------------------------------------------------------
// normalize_component_runtime_kind
// -----------------------------------------------------------------------

#[test]
fn normalize_kind_text() {
    assert_eq!(
        normalize_component_runtime_kind("text"),
        Some("text".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("Text"),
        Some("text".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("text_translation"),
        Some("text".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("field"),
        Some("text".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("fields"),
        Some("text".to_string())
    );
}

#[test]
fn normalize_kind_image() {
    assert_eq!(
        normalize_component_runtime_kind("image"),
        Some("image".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("images"),
        Some("image".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("image_translation"),
        Some("image".to_string())
    );
}

#[test]
fn normalize_kind_video() {
    assert_eq!(
        normalize_component_runtime_kind("video"),
        Some("video".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("videos"),
        Some("video".to_string())
    );
}

#[test]
fn normalize_kind_audio() {
    assert_eq!(
        normalize_component_runtime_kind("audio"),
        Some("audio".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("audios"),
        Some("audio".to_string())
    );
}

#[test]
fn normalize_kind_document() {
    assert_eq!(
        normalize_component_runtime_kind("document"),
        Some("document".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("doc"),
        Some("document".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("file"),
        Some("document".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("files"),
        Some("document".to_string())
    );
}

#[test]
fn normalize_kind_openai_compatible() {
    assert_eq!(
        normalize_component_runtime_kind("openai_compatible"),
        Some("openai_compatible".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("openai-compatible"),
        Some("openai_compatible".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("openai"),
        Some("openai_compatible".to_string())
    );
    assert_eq!(
        normalize_component_runtime_kind("llm"),
        Some("openai_compatible".to_string())
    );
}

#[test]
fn normalize_kind_unknown_returns_none() {
    assert_eq!(normalize_component_runtime_kind("unknown"), None);
    assert_eq!(normalize_component_runtime_kind(""), None);
    assert_eq!(normalize_component_runtime_kind("  "), None);
}

#[test]
fn api_version_compatibility_major_one_supported() {
    assert!(is_component_api_version_compatible(Some("1.0.0")));
    assert!(is_component_api_version_compatible(Some("1.2")));
    assert!(is_component_api_version_compatible(Some("v1")));
    assert!(is_component_api_version_compatible(None));
}

#[test]
fn api_version_compatibility_rejects_invalid_or_new_major() {
    assert!(!is_component_api_version_compatible(Some("x.y.z")));
    assert!(!is_component_api_version_compatible(Some("1.2.3.4")));
    assert!(is_component_api_version_compatible(Some("2.0.0")));
    assert!(!is_component_api_version_compatible(Some("3.0.0")));
}

#[test]
fn build_component_pools_filters_vendor_mismatches() {
    let binding = ComponentBindingEntry {
        key_ids: vec!["azure-key".to_string()],
        oauth_ids: vec!["azure-oauth".to_string()],
        auth_strategy: KeySelectionStrategy::RoundRobin,
        ..Default::default()
    };
    let vendor_keys_doc = VendorKeysDoc {
        version: 1,
        keys: HashMap::from([(
            "azure-key".to_string(),
            VendorKey {
                vendor_id: "azure".to_string(),
                label: "Azure".to_string(),
                auth_values: HashMap::from([("api_key".to_string(), "sk-azure".to_string())]),
                max_concurrent: 5,
                requests_per_second: 3.0,
                weight: 1,
                enabled: true,
                max_input_chars: 0,
                max_file_size_mb: 0.0,
            },
        )]),
    };
    let vendor_oauth_doc = VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([(
            "azure-oauth".to_string(),
            OAuthConfig {
                vendor_id: "azure".to_string(),
                label: "Azure OAuth".to_string(),
                grant_type: "client_credentials".to_string(),
                auth_url: String::new(),
                token_url: "https://login.microsoft.com/token".to_string(),
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                scopes: String::new(),
                extra_params: HashMap::new(),
                auth_extra_params: HashMap::new(),
                cached_token: Some("tok".to_string()),
                cached_token_expires_at: 0,
                refresh_token: None,
                max_concurrent: 5,
                weight: 1,
                max_input_chars: 0,
                max_file_size_mb: 0.0,
                token_field: "access_token".to_string(),
            },
        )]),
    };

    let (key_pool, oauth_pool) = build_component_pools(
        "comp-openai",
        Some("openai"),
        Some(&binding),
        &vendor_keys_doc,
        &vendor_oauth_doc,
    )
    .unwrap();

    assert!(
        key_pool.is_none(),
        "mismatched vendor keys must be filtered out of the runtime pool"
    );
    assert!(
        oauth_pool.is_none(),
        "mismatched vendor oauth configs must be filtered out of the runtime pool"
    );
}

#[test]
fn supported_types_contract_matches_kind() {
    assert!(component_kind_matches_supported_types(
        "image",
        &["image".to_string()]
    ));
    assert!(!component_kind_matches_supported_types(
        "image",
        &["text".to_string()]
    ));
    assert!(component_kind_matches_supported_types(
        "text",
        &["text".to_string(), "document".to_string()]
    ));
    assert!(component_kind_matches_supported_types("text", &[]));
}

// -----------------------------------------------------------------------
// normalize_component_supported_business_lines
// -----------------------------------------------------------------------

#[test]
fn normalize_business_lines_empty() {
    assert!(normalize_component_supported_business_lines(&[]).is_empty());
}

#[test]
fn normalize_business_lines_basic() {
    let lines = vec!["post".to_string(), "theme".to_string()];
    let result = normalize_component_supported_business_lines(&lines);
    assert_eq!(result, vec!["post_content", "theme_i18n"]);
}

#[test]
fn normalize_business_lines_all_returns_empty() {
    let lines = vec!["post".to_string(), "all".to_string()];
    let result = normalize_component_supported_business_lines(&lines);
    assert!(result.is_empty(), "'all' should clear the list");
}

#[test]
fn normalize_business_lines_star_returns_empty() {
    let lines = vec!["*".to_string()];
    let result = normalize_component_supported_business_lines(&lines);
    assert!(result.is_empty(), "'*' should clear the list");
}

#[test]
fn normalize_business_lines_deduplicates() {
    let lines = vec![
        "post".to_string(),
        "post_type".to_string(),
        "post_content".to_string(),
    ];
    let result = normalize_component_supported_business_lines(&lines);
    assert_eq!(result, vec!["post_content"]);
}

#[test]
fn normalize_business_lines_skips_empty() {
    let lines = vec!["".to_string(), "  ".to_string(), "post".to_string()];
    let result = normalize_component_supported_business_lines(&lines);
    assert_eq!(result, vec!["post_content"]);
}

#[test]
fn editable_params_allows_prefix_paths() {
    let template = ComponentTemplate {
        id: "test".to_string(),
        name: "test".to_string(),
        version: "1.0.0".to_string(),
        kind: "text".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://example.com".to_string(),
            headers: None,
            body: None,
            body_type: None,
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: Some("data.text".to_string()),
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
        constraints: None,
        editable_params: vec![
            ComponentEditableParam {
                path: "request.body".to_string(),
                scope: Some("component".to_string()),
                value_type: Some("json".to_string()),
                required: Some(false),
            },
            ComponentEditableParam {
                path: "constraints.max_input_chars".to_string(),
                scope: Some("component".to_string()),
                value_type: Some("integer".to_string()),
                required: Some(false),
            },
        ],
        translation_modes: vec![],
    };

    assert!(template_allows_editable_path(
        &template,
        "request.body.model"
    ));
    assert!(template_allows_editable_path(
        &template,
        "constraints.max_input_chars"
    ));
    assert!(!template_allows_editable_path(
        &template,
        "constraints.rate_limit_qps"
    ));
}

#[test]
fn constraints_override_only_keeps_editable_fields() {
    let template = ComponentTemplate {
        id: "test".to_string(),
        name: "test".to_string(),
        version: "1.0.0".to_string(),
        kind: "text".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://example.com".to_string(),
            headers: None,
            body: None,
            body_type: None,
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: Some("data.text".to_string()),
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
        constraints: None,
        editable_params: vec![ComponentEditableParam {
            path: "constraints.max_input_chars".to_string(),
            scope: Some("component".to_string()),
            value_type: Some("integer".to_string()),
            required: Some(false),
        }],
        translation_modes: vec![],
    };
    let override_constraints = ComponentConstraints {
        max_input_chars: Some(4000),
        rate_limit_qps: Some(10),
        ..ComponentConstraints::default()
    };
    let filtered = filter_constraints_override_by_editable(&template, Some(override_constraints))
        .expect("filtered override");
    assert_eq!(filtered.max_input_chars, Some(4000));
    assert_eq!(filtered.rate_limit_qps, None);
}

#[test]
fn runtime_limits_use_constraints_values() {
    let constraints = ComponentConstraints {
        rate_limit_qps: Some(5),
        max_concurrent_requests: Some(3),
        ..ComponentConstraints::default()
    };
    let (max_concurrent, min_interval_ms, sem, last_at) =
        runtime_limits_from_constraints(Some(&constraints));
    assert_eq!(max_concurrent, 3);
    assert!(min_interval_ms >= 200);
    assert!(sem.is_some());
    assert!(last_at.is_some());
}

#[test]
fn resolve_runtime_for_content_format_applies_translation_mode_overrides() {
    let runtime = ComponentRuntime {
        template: ComponentTemplate {
            id: "test".to_string(),
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            kind: "text".to_string(),
            client_contract: None,
            default_values: Some(serde_json::json!({
                "temperature": 0.1,
                "model": "base-model"
            })),
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: "https://example.com/base".to_string(),
                headers: Some(
                    [("Content-Type".to_string(), "application/json".to_string())]
                        .into_iter()
                        .collect(),
                ),
                body: Some(serde_json::json!({
                    "text": "{{input.text}}",
                    "format": "plain"
                })),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: Some("data.text".to_string()),
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
                max_input_chars: Some(100),
                rate_limit_qps: Some(2),
                ..ComponentConstraints::default()
            }),
            editable_params: vec![],
            translation_modes: vec![ComponentTranslationMode {
                id: "html".to_string(),
                label: "HTML".to_string(),
                description: None,
                supported_content_formats: vec!["rich_html".to_string()],
                request_overrides: Some(ComponentRequestOverrides {
                method: None,
                    url: Some("https://example.com/html".to_string()),
                    headers: Some(
                        [("X-Mode".to_string(), "html".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    body: Some(serde_json::json!({
                        "text": "{{input.text}}",
                        "format": "html"
                    })),
                }),
                constraints_overrides: Some(ComponentConstraints {
                    max_input_chars: Some(500),
                    rate_limit_qps: Some(5),
                    ..ComponentConstraints::default()
                }),
                default_values_overrides: Some(serde_json::json!({
                    "temperature": 0.7
                })),
                api_docs_url: None,
            }],
        },
        auth_values: HashMap::new(),
        supported_business_lines: vec!["post_content".to_string()],
        language_map: HashMap::new(),
        supported_content_formats: vec!["plain_text".to_string(), "rich_html".to_string()],
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

    let resolved = resolve_runtime_for_content_format(&runtime, "rich_html");
    assert_eq!(resolved.template.request.url, "https://example.com/html");
    assert_eq!(
        resolved
            .template
            .request
            .headers
            .as_ref()
            .and_then(|headers| headers.get("X-Mode"))
            .map(|value| value.as_str()),
        Some("html")
    );
    assert_eq!(
        resolved
            .template
            .request
            .body
            .as_ref()
            .and_then(|body| body.get("format"))
            .and_then(|value| value.as_str()),
        Some("html")
    );
    assert_eq!(
        resolved
            .template
            .default_values
            .as_ref()
            .and_then(|value| value.get("temperature")),
        Some(&serde_json::json!(0.7))
    );
    assert_eq!(
        resolved
            .template
            .default_values
            .as_ref()
            .and_then(|value| value.get("model")),
        Some(&serde_json::json!("base-model"))
    );
    assert_eq!(
        resolved
            .template
            .constraints
            .as_ref()
            .and_then(|constraints| constraints.max_input_chars),
        Some(500)
    );
    assert_eq!(
        resolved
            .template
            .constraints
            .as_ref()
            .and_then(|constraints| constraints.rate_limit_qps),
        Some(5)
    );
    assert_eq!(
        resolved.supported_content_formats,
        vec!["rich_html".to_string()]
    );
    assert!(resolved.runtime_min_interval_ms >= 200);

    let plain = resolve_runtime_for_content_format(&runtime, "plain_text");
    assert_eq!(plain.template.request.url, "https://example.com/base");
    assert_eq!(
        plain
            .template
            .request
            .body
            .as_ref()
            .and_then(|body| body.get("format"))
            .and_then(|value| value.as_str()),
        Some("plain")
    );
}

#[test]
fn request_overrides_respect_editable_param_whitelist() {
    let mut template = ComponentTemplate {
        id: "test".to_string(),
        name: "test".to_string(),
        version: "1.0.0".to_string(),
        kind: "text".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://example.com".to_string(),
            headers: Some(
                [("Content-Type".to_string(), "application/json".to_string())]
                    .into_iter()
                    .collect(),
            ),
            body: Some(serde_json::json!({
                "text": "{{input.text}}",
                "temperature": 0.7
            })),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: Some("data.text".to_string()),
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
        constraints: None,
        editable_params: vec![
            ComponentEditableParam {
                path: "request.headers.Authorization".to_string(),
                scope: Some("component".to_string()),
                value_type: Some("string".to_string()),
                required: Some(false),
            },
            ComponentEditableParam {
                path: "request.body.model".to_string(),
                scope: Some("component".to_string()),
                value_type: Some("string".to_string()),
                required: Some(false),
            },
        ],
        translation_modes: vec![],
    };

    let binding_entry = ComponentBindingEntry {
        auth: HashMap::new(),
        template_id: None,
        r#type: None,
        name: None,
        language_map: HashMap::new(),
        constraints_override: None,
        request_overrides: Some(ComponentRequestOverrides {
                method: None,
            url: Some("https://not-allowed.example.com".to_string()),
            headers: Some(
                [
                    ("Authorization".to_string(), "Bearer abc".to_string()),
                    ("X-Blocked".to_string(), "no".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            body: Some(serde_json::json!({
                "model": "gpt-4o-mini",
                "temperature": 0.2
            })),
        }),
        default_values_override: None,
        key_ids: vec![],
        oauth_ids: vec![],
        auth_strategy: KeySelectionStrategy::RoundRobin,
    };

    apply_request_overrides_to_template(&mut template, binding_entry.request_overrides.as_ref());

    assert_eq!(template.request.url, "https://example.com");
    let headers = template.request.headers.unwrap_or_default();
    assert_eq!(
        headers.get("Content-Type").map(|v| v.as_str()),
        Some("application/json")
    );
    assert_eq!(
        headers.get("Authorization").map(|v| v.as_str()),
        Some("Bearer abc")
    );
    assert!(!headers.contains_key("X-Blocked"));

    let body = template
        .request
        .body
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    assert_eq!(
        body.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        "gpt-4o-mini"
    );
    // Existing key remains unchanged because it was not in editable whitelist.
    assert_eq!(
        body.get("temperature")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        0.7
    );
}

#[test]
fn component_instance_default_values_override_respects_editable_whitelist() {
    let mut template = ComponentTemplate {
        id: "test".to_string(),
        name: "test".to_string(),
        version: "1.0.0".to_string(),
        kind: "text".to_string(),
        client_contract: None,
        default_values: Some(serde_json::json!({
            "temperature": 0.1,
            "model": "base-model"
        })),
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://example.com".to_string(),
            headers: None,
            body: None,
            body_type: None,
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: Some("data.text".to_string()),
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
        constraints: None,
        editable_params: vec![ComponentEditableParam {
            path: "default_values.temperature".to_string(),
            scope: Some("component".to_string()),
            value_type: Some("number".to_string()),
            required: Some(false),
        }],
        translation_modes: vec![],
    };

    let overrides = ComponentInstanceOverrides {
        constraints_override: None,
        request_overrides: None,
        default_values_override: Some(serde_json::json!({
            "temperature": 0.3,
            "model": "blocked-model"
        })),
    };

    apply_component_instance_overrides_to_template(&mut template, Some(&overrides));

    let defaults = template.default_values.expect("default values");
    assert_eq!(defaults.get("temperature"), Some(&serde_json::json!(0.3)));
    assert_eq!(
        defaults.get("model"),
        Some(&serde_json::json!("base-model"))
    );
}

#[tokio::test]
async fn load_component_runtimes_preserves_structured_component_list_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 2048];
        let _ = socket.read(&mut request).await;
        let body =
            r#"{"success":false,"error":{"code":"SESSION_EXPIRED","message":"Session expired"}}"#;
        let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let client = Client::builder().no_proxy().build().unwrap();
    let mut component_bindings = ComponentBindingsDoc::default();
    let err = load_component_runtimes(
        &client,
        &format!("http://{}", addr),
        "sess-test-component-list",
        "/tmp/test-component-loader.log",
        &mut component_bindings,
        "/tmp/test-component-bindings.json",
        None,
        Some("-----BEGIN PUBLIC KEY-----\nplaceholder\n-----END PUBLIC KEY-----"),
    )
    .await
    .expect_err("structured component-list auth errors should currently fail");
    let err_text = format!("{:#}", err);

    assert!(
        err_text.contains("SESSION_EXPIRED"),
        "expected structured server error code to survive component list failure: {}",
        err_text
    );
    assert!(
            !err_text.contains("unsupported response shape"),
            "structured component-list errors should not be collapsed into unsupported response shape: {}",
            err_text
        );
}

#[tokio::test]
async fn load_component_runtimes_preserves_component_download_error_details() {
    let _env_lock = env_lock().lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let (status, body) = if step == 0 {
                assert!(
                    request_text.starts_with("GET /api/v1/client/components?"),
                    "first request should fetch component list, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                    "200 OK",
                    r#"{"success":true,"data":{"items":[{"id":"official-test-text-v1","name":"Official Test Text","owner_type":"official","type":"text_translation","supported_types":["text"],"supported_business_lines":["post_content"],"supported_content_formats":["plain_text"],"api_version":"1.0.0","status":"active"}],"page":1,"per_page":200,"total":1,"total_pages":1}}"#,
                )
            } else {
                assert!(
                    request_text.starts_with(
                        "GET /api/v1/client/components/official-test-text-v1/download"
                    ),
                    "second request should download component template, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                    "422 Unprocessable Entity",
                    r#"{"success":false,"error":{"code":"COMPONENT_DISABLED","message":"Component template is disabled"}}"#,
                )
            };
            let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let isolated_components_local = PathBuf::from(format!(
        "/tmp/wptsall-components-local-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let _web_ui_guard = EnvVarGuard::set("WPTSALL_WEB_UI", "false".to_string());
    let _components_guard = EnvVarGuard::set(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        isolated_components_local.display().to_string(),
    );

    let client = Client::builder().no_proxy().build().unwrap();
    let mut component_bindings = ComponentBindingsDoc::default();
    let err = load_component_runtimes(
        &client,
        &format!("http://{}", addr),
        "sess-test-component-download",
        "/tmp/test-component-loader.log",
        &mut component_bindings,
        "/tmp/test-component-bindings.json",
        None,
        Some("-----BEGIN PUBLIC KEY-----\nplaceholder\n-----END PUBLIC KEY-----"),
    )
    .await
    .expect_err("structured component-download errors should currently fail");
    let err_text = format!("{:#}", err);

    assert!(
        err_text.contains("COMPONENT_DISABLED"),
        "expected component download error code to survive runtime load failure: {}",
        err_text
    );
    assert!(
        !err_text.contains("no supported component runtime loaded"),
        "component download failure should not be collapsed into generic no-runtime error: {}",
        err_text
    );
}

#[tokio::test]
async fn load_component_runtimes_reads_local_components_from_runtime_db_in_web_ui_mode() {
    let _env_lock = env_lock().lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let (status, body) = if step == 0 {
                assert!(
                    request_text.starts_with("GET /api/v1/client/components?"),
                    "first request should fetch component list, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                        "200 OK".to_string(),
                        r#"{"success":true,"data":{"items":[{"id":"official-test-text-v1","name":"Official Test Text","owner_type":"official","type":"text_translation","supported_types":["text"],"supported_business_lines":["post_content"],"supported_content_formats":["plain_text"],"api_version":"1.0.0","status":"active"}],"page":1,"per_page":200,"total":1,"total_pages":1}}"#
                            .to_string(),
                    )
            } else {
                assert!(
                    request_text.starts_with(
                        "GET /api/v1/client/components/official-test-text-v1/download"
                    ),
                    "second request should download component template, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                    "200 OK".to_string(),
                    serde_json::to_string(&json!({
                        "success": true,
                        "data": {
                            "component_id": "official-test-text-v1",
                            "version": "1.0.0",
                            "owner_type": "official",
                            "template_json": {
                                "id": "official-test-text-v1",
                                "name": "Official Test Text",
                                "version": "1.0.0",
                                "type": "text_translation",
                                "request": {
                                    "method": "POST",
                                    "url": "https://example.com/official",
                                    "body": {}
                                },
                                "response": {
                                    "translated_text_path": "data.text"
                                }
                            },
                            "encrypted_payload": null,
                            "nonce": null,
                            "algorithm": null,
                            "kdf_version": null,
                            "signature": null,
                            "signing_key_id": null
                        }
                    }))
                    .unwrap(),
                )
            };
            let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let db_path = format!("/tmp/wptsall-loader-runtime-db-{}.db", uuid::Uuid::new_v4());
    let components_local_path = format!(
        "/tmp/wptsall-loader-runtime-local-{}.json",
        uuid::Uuid::new_v4()
    );
    let component_bindings_path = PathBuf::from(format!(
        "/tmp/wptsall-loader-runtime-bindings-{}.json",
        uuid::Uuid::new_v4()
    ));
    let _web_ui_guard = EnvVarGuard::set("WPTSALL_WEB_UI", "true".to_string());
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard = EnvVarGuard::set(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        components_local_path.clone(),
    );
    let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());

    save_components_local(&components_local_path, &ComponentsLocalDoc::default()).unwrap();
    let conn = crate::db::open_db(&db_path).unwrap();
    mark_json_migration_done(&conn);
    let runtime_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-db-component": {
                "name": "Local DB Component",
                "template_id": "official-test-text-v1",
                "source_template_id": "official-test-text-v1",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "updated_at": "2",
                "active_version": "2.0.0",
                "versions": {
                    "1.0.0": {
                        "version": "1.0.0",
                        "created_at": "1",
                        "proxy_profile_id": "old-proxy"
                    },
                    "2.0.0": {
                        "version": "2.0.0",
                        "created_at": "2",
                        "proxy_profile_id": "main-proxy"
                    }
                },
                "template_json": {
                    "id": "official-test-text-v1",
                    "name": "Local DB Template",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/local",
                        "body": {}
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .unwrap();
    crate::db::components::save_local_components_doc(&conn, &runtime_doc).unwrap();
    drop(conn);

    let client = Client::builder().no_proxy().build().unwrap();
    let mut component_bindings = ComponentBindingsDoc::default();
    let registry = load_component_runtimes(
        &client,
        &format!("http://{}", addr),
        "sess-test-runtime-db",
        "/tmp/test-component-loader.log",
        &mut component_bindings,
        component_bindings_path.to_string_lossy().as_ref(),
        None,
        None,
    )
    .await
    .expect("runtime loader should read local components from SQLite in Web UI mode");

    assert!(
        registry.runtimes.contains_key("local-db-component"),
        "local runtime should come from SQLite even when components.json is empty"
    );
    assert!(
        registry
            .ordered_ids
            .contains(&"local-db-component".to_string()),
        "ordered runtime list should include the SQLite-backed local component"
    );
    let local_runtime = registry
        .runtimes
        .get("local-db-component")
        .expect("local runtime exists");
    assert_eq!(local_runtime.template.id, "local-db-component");
    assert_eq!(local_runtime.template.name, "Local DB Template");
    assert_eq!(local_runtime.template.kind, "text_translation");
    assert_eq!(
        local_runtime.template.request.url,
        "https://example.com/local"
    );
    assert_eq!(
        local_runtime.proxy_profile_id.as_deref(),
        Some("main-proxy")
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_local_path);
    let _ = std::fs::remove_file(&component_bindings_path);
}

#[tokio::test]
async fn load_component_runtimes_skips_local_component_missing_required_auth() {
    let _env_lock = env_lock().lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let (status, body) = if step == 0 {
                assert!(
                    request_text.starts_with("GET /api/v1/client/components?"),
                    "first request should fetch component list, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                    "200 OK".to_string(),
                    r#"{"success":true,"data":{"items":[{"id":"official-test-text-v1","name":"Official Test Text","owner_type":"official","type":"text_translation","supported_types":["text"],"supported_business_lines":["post_content"],"supported_content_formats":["plain_text"],"api_version":"1.0.0","status":"active"}],"page":1,"per_page":200,"total":1,"total_pages":1}}"#
                        .to_string(),
                )
            } else {
                assert!(
                    request_text.starts_with(
                        "GET /api/v1/client/components/official-test-text-v1/download"
                    ),
                    "second request should download server component template, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                    "200 OK".to_string(),
                    serde_json::to_string(&json!({
                        "success": true,
                        "data": {
                            "component_id": "official-test-text-v1",
                            "version": "1.0.0",
                            "owner_type": "official",
                            "template_json": {
                                "id": "official-test-text-v1",
                                "name": "Official Test Text",
                                "version": "1.0.0",
                                "type": "text_translation",
                                "request": {
                                    "method": "POST",
                                    "url": "https://example.com/official",
                                    "body": {}
                                },
                                "response": {
                                    "translated_text_path": "data.text"
                                }
                            },
                            "encrypted_payload": null,
                            "nonce": null,
                            "algorithm": null,
                            "kdf_version": null,
                            "signature": null,
                            "signing_key_id": null
                        }
                    }))
                    .unwrap(),
                )
            };
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let components_local_path = PathBuf::from(format!(
        "/tmp/wptsall-components-local-auth-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let component_bindings_path = PathBuf::from(format!(
        "/tmp/wptsall-component-bindings-auth-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-auth-missing": {
                "name": "Local Auth Missing",
                "template_id": "official-test-text-v1",
                "source_template_id": "official-test-text-v1",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "updated_at": "2",
                "versions": {},
                "template_json": {
                    "id": "official-test-text-v1",
                    "name": "Local Auth Missing",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "auth": {
                        "fields": [
                            {
                                "name": "api_key",
                                "required": true,
                                "secret": true
                            }
                        ]
                    },
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/local",
                        "body": {}
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .unwrap();
    save_components_local(components_local_path.to_string_lossy().as_ref(), &local_doc).unwrap();
    let _web_ui_guard = EnvVarGuard::set("WPTSALL_WEB_UI", "false".to_string());
    let _components_guard = EnvVarGuard::set(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        components_local_path.display().to_string(),
    );
    let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());

    let client = Client::builder().no_proxy().build().unwrap();
    let mut component_bindings = ComponentBindingsDoc::default();
    let registry = load_component_runtimes(
        &client,
        &format!("http://{}", addr),
        "sess-test-local-auth-skip",
        "/tmp/test-component-loader.log",
        &mut component_bindings,
        component_bindings_path.to_string_lossy().as_ref(),
        None,
        None,
    )
    .await
    .expect("runtime loader should keep working when a local component is missing auth");

    assert!(
        !registry.runtimes.contains_key("local-auth-missing"),
        "local component missing required auth must be skipped"
    );
    assert!(
        registry.runtimes.contains_key("official-test-text-v1"),
        "server component should still load successfully"
    );

    let _ = std::fs::remove_file(&components_local_path);
    let _ = std::fs::remove_file(&component_bindings_path);
}

#[tokio::test]
async fn load_component_runtimes_skips_local_snapshot_with_incompatible_source_api_version() {
    let _env_lock = env_lock().lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..n]);
            let (status, body) = if step == 0 {
                assert!(
                    request_text.starts_with("GET /api/v1/client/components?"),
                    "first request should fetch component list, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                        "200 OK".to_string(),
                        r#"{"success":true,"data":{"items":[{"id":"official-compatible-v1","name":"Official Compatible","owner_type":"official","type":"text_translation","supported_types":["text"],"supported_business_lines":["post_content"],"supported_content_formats":["plain_text"],"api_version":"1.0.0","status":"active"}],"page":1,"per_page":200,"total":1,"total_pages":1}}"#
                            .to_string(),
                    )
            } else {
                assert!(
                    request_text.starts_with(
                        "GET /api/v1/client/components/official-compatible-v1/download"
                    ),
                    "second request should download the compatible server component, got: {}",
                    request_text.lines().next().unwrap_or("")
                );
                (
                    "200 OK".to_string(),
                    serde_json::to_string(&json!({
                        "success": true,
                        "data": {
                            "component_id": "official-compatible-v1",
                            "version": "1.0.0",
                            "owner_type": "official",
                            "template_json": {
                                "id": "official-compatible-v1",
                                "name": "Official Compatible",
                                "version": "1.0.0",
                                "type": "text_translation",
                                "request": {
                                    "method": "POST",
                                    "url": "https://example.com/official",
                                    "body": {}
                                },
                                "response": {
                                    "translated_text_path": "data.text"
                                }
                            },
                            "encrypted_payload": null,
                            "nonce": null,
                            "algorithm": null,
                            "kdf_version": null,
                            "signature": null,
                            "signing_key_id": null
                        }
                    }))
                    .unwrap(),
                )
            };
            let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let components_local_path = PathBuf::from(format!(
        "/tmp/wptsall-components-local-api-version-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let component_bindings_path = PathBuf::from(format!(
        "/tmp/wptsall-component-bindings-api-version-{}.json",
        uuid::Uuid::new_v4().simple()
    ));
    let local_doc: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "local-incompatible": {
                "name": "Local Incompatible",
                "template_id": "official-future-v2",
                "source_template_id": "official-future-v2",
                "source_template_api_version": "3.0.0",
                "vendor_id": "openai",
                "kind": "text",
                "enabled": true,
                "created_at": "1",
                "updated_at": "2",
                "versions": {},
                "template_json": {
                    "id": "official-future-v2",
                    "name": "Future Local Snapshot",
                    "version": "1.0.0",
                    "type": "text_translation",
                    "request": {
                        "method": "POST",
                        "url": "https://example.com/future",
                        "body": {}
                    },
                    "response": {
                        "translated_text_path": "data.text"
                    }
                }
            }
        }
    }))
    .unwrap();
    save_components_local(components_local_path.to_string_lossy().as_ref(), &local_doc).unwrap();

    let _components_guard = EnvVarGuard::set(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        components_local_path.display().to_string(),
    );
    let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());

    let client = Client::builder().no_proxy().build().unwrap();
    let mut component_bindings = ComponentBindingsDoc::default();
    let registry = load_component_runtimes(
        &client,
        &format!("http://{}", addr),
        "sess-test-local-api-version",
        "/tmp/test-component-loader.log",
        &mut component_bindings,
        component_bindings_path.to_string_lossy().as_ref(),
        None,
        None,
    )
    .await
    .expect("compatible server runtime should still load");

    assert!(
        !registry.runtimes.contains_key("local-incompatible"),
        "snapshot-backed local component with unsupported api_version must be skipped"
    );
    assert!(
        registry.runtimes.contains_key("official-compatible-v1"),
        "compatible official runtime should still be available"
    );

    let _ = std::fs::remove_file(&components_local_path);
    let _ = std::fs::remove_file(&component_bindings_path);
}
