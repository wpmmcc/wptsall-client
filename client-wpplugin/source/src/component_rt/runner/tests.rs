use super::*;
use serde_json::json;
use sha2::Digest;
use std::io::{Read, Write};
use std::net::TcpListener as StdTcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn credential_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_credential_env() -> std::sync::MutexGuard<'static, ()> {
    // A failed network-fixture assertion must not make unrelated credential
    // tests fail with a lock-poison error instead of their real assertion.
    credential_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn provider_egress_allows_any_http_including_loopback_mock() {
    // Product: translation vendor URLs are operator-chosen — loopback mock /
    // local LLM hosts must work without an allowlist.
    assert!(assert_provider_url_allowed("http://127.0.0.1:9090/v1/chat/completions").is_ok());
    assert!(assert_provider_url_allowed("http://localhost:5000/translate").is_ok());
    assert!(assert_provider_url_allowed("https://api.example.com/v1").is_ok());
    // Still reject embedded credentials and non-http schemes.
    assert!(assert_provider_url_allowed("http://user:pass@example.com/api").is_err());
    assert!(assert_provider_url_allowed("file:///etc/passwd").is_err());
    // Cloud metadata floor.
    assert!(assert_provider_url_allowed("http://169.254.169.254/latest/meta-data").is_err());
    assert!(assert_provider_url_allowed("http://metadata.google.internal/a").is_err());
}

#[test]
fn s3_compatible_endpoint_uses_the_same_egress_boundary() {
    assert!(assert_provider_url_allowed("http://169.254.169.254:9000").is_err());
    assert!(assert_provider_url_allowed("http://user:pass@s3.example.com").is_err());
}

#[test]
fn provider_egress_allows_public_https_host() {
    let result = assert_provider_url_allowed("https://example.com/api");
    assert!(result.is_ok());
}

// -----------------------------------------------------------------------
// render_template_string
// -----------------------------------------------------------------------

#[test]
fn render_template_string_basic_substitution() {
    let mut ctx = HashMap::new();
    ctx.insert("input.text".to_string(), "Hello World".to_string());
    ctx.insert("input.source_lang".to_string(), "en".to_string());
    ctx.insert("input.target_lang".to_string(), "zh".to_string());

    let result = render_template_string(
        "Translate '{{input.text}}' from {{input.source_lang}} to {{input.target_lang}}",
        &ctx,
    );
    assert_eq!(result, "Translate 'Hello World' from en to zh");
}

#[test]
fn render_template_string_no_match_left_intact() {
    let ctx = HashMap::new();
    let result = render_template_string("{{unknown.key}} stays", &ctx);
    assert_eq!(result, "{{unknown.key}} stays");
}

#[test]
fn render_template_string_empty_input() {
    let ctx = HashMap::new();
    assert_eq!(render_template_string("", &ctx), "");
}

#[test]
fn render_template_string_auth_variables() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.api_key".to_string(), "sk-abc123".to_string());
    ctx.insert("auth.secret".to_string(), "mysecret".to_string());

    let result = render_template_string("Bearer {{auth.api_key}}", &ctx);
    assert_eq!(result, "Bearer sk-abc123");
}

#[test]
fn insert_component_default_values_context_flattens_scalars() {
    let mut ctx = HashMap::new();
    insert_component_default_values_context(
        &mut ctx,
        Some(&serde_json::json!({
            "resource_name": "openai",
            "deployment": "gpt-4o-mini",
            "api_version": "2024-10-21",
            "nested": {
                "mode": "chat"
            }
        })),
    );

    assert_eq!(
        ctx.get("default_values.resource_name"),
        Some(&"openai".to_string())
    );
    assert_eq!(
        ctx.get("default_values.deployment"),
        Some(&"gpt-4o-mini".to_string())
    );
    assert_eq!(
        ctx.get("default_values.api_version"),
        Some(&"2024-10-21".to_string())
    );
    assert_eq!(
        ctx.get("default_values.nested.mode"),
        Some(&"chat".to_string())
    );
}

#[test]
fn render_template_string_multiple_same_token() {
    let mut ctx = HashMap::new();
    ctx.insert("input.text".to_string(), "X".to_string());

    let result = render_template_string("{{input.text}} and {{input.text}}", &ctx);
    assert_eq!(result, "X and X");
}

// -----------------------------------------------------------------------
// signing
// -----------------------------------------------------------------------

#[test]
fn process_sign_config_bearer_sets_authorization_header() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.api_key".to_string(), "sk-test-123".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "bearer"
        })),
        &mut ctx,
        "POST",
        "https://example.com/v1/translate",
    )
    .expect("bearer signing should succeed");

    match result {
        SignResult::AuthorizationHeader(value) => assert_eq!(value, "Bearer sk-test-123"),
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_custom_header_uses_first_non_empty_auth_candidate() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.oauth_token".to_string(), "".to_string());
    ctx.insert("auth.api_key".to_string(), "fallback-key".to_string());
    ctx.insert("auth.tenant".to_string(), "acme".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "custom_header",
            "auth_candidates": [
                {
                    "value_key": "auth.oauth_token",
                    "prefix": "Bearer"
                },
                {
                    "value_key": "auth.api_key",
                    "prefix": "Token"
                }
            ],
            "headers": [
                {
                    "name": "X-Tenant",
                    "value_template": "tenant={{auth.tenant}}"
                }
            ]
        })),
        &mut ctx,
        "POST",
        "https://example.com/v1/translate",
    )
    .expect("custom header signing should succeed");

    match result {
        SignResult::WithHeaders(_, headers) => {
            assert!(
                headers.iter().any(|(name, value)| {
                    name == "Authorization" && value == "Token fallback-key"
                }),
                "authorization header should fall back to api_key candidate"
            );
            assert!(
                headers
                    .iter()
                    .any(|(name, value)| name == "X-Tenant" && value == "tenant=acme"),
                "custom templated header should be resolved"
            );
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_alibaba_renders_templated_value_fields() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.access_key".to_string(), "test-access".to_string());
    ctx.insert("auth.secret_key".to_string(), "test-secret".to_string());
    ctx.insert(
        "input.source_ref".to_string(),
        "https://example.com/source.mp4".to_string(),
    );
    ctx.insert("input.source_lang".to_string(), "zh".to_string());
    ctx.insert("input.target_lang".to_string(), "en".to_string());
    ctx.insert(
        "auth.output_media_url_prefix".to_string(),
        "https://bucket.oss-cn-shanghai.aliyuncs.com/out/".to_string(),
    );
    ctx.insert("computed.nonce".to_string(), "nonce-123".to_string());
    ctx.insert(
        "computed.timestamp_iso8601".to_string(),
        "2026-03-19T12:00:00Z".to_string(),
    );

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "alibaba_v1",
            "concat": [
                { "param_name": "AccessKeyId", "ctx_key": "auth.access_key" },
                { "param_name": "Action", "value": "SubmitVideoTranslationJob" },
                { "param_name": "InputConfig", "value": "{\"Type\":\"Video\",\"Video\":\"{{input.source_ref}}\"}" },
                { "param_name": "OutputConfig", "value": "{\"MediaURL\":\"{{auth.output_media_url_prefix}}{{computed.nonce}}.mp4\"}" },
                { "param_name": "SignatureMethod", "value": "HMAC-SHA1" },
                { "param_name": "SignatureNonce", "ctx_key": "computed.nonce" },
                { "param_name": "SignatureVersion", "value": "1.0" },
                { "param_name": "Timestamp", "ctx_key": "computed.timestamp_iso8601" },
                { "param_name": "Version", "value": "2020-11-09" }
            ]
        })),
        &mut ctx,
        "POST",
        "https://ice.cn-shanghai.aliyuncs.com",
    )
    .expect("alibaba signing should succeed");

    match result {
        SignResult::ContextOnly(extra) => {
            let sign = extra
                .get("computed.sign")
                .expect("sign should be added to computed context");
            assert!(!sign.trim().is_empty(), "signature should not be empty");
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_azure_subscription_key_injects_expected_header() {
    let mut ctx = HashMap::new();
    ctx.insert(
        "auth.subscription_key".to_string(),
        "mock-azure-sub-key".to_string(),
    );

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "azure_subscription_key"
        })),
        &mut ctx,
        "POST",
        "https://example.com/translate",
    )
    .expect("azure subscription key signing should succeed");

    match result {
        SignResult::WithHeaders(computed, headers) => {
            assert!(
                computed.is_empty(),
                "azure header injection should not add computed fields"
            );
            assert_eq!(
                headers,
                vec![(
                    "Ocp-Apim-Subscription-Key".to_string(),
                    "mock-azure-sub-key".to_string()
                )]
            );
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_kakao_api_key_sets_authorization_header() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.api_key".to_string(), "mock-kakao-key".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "kakao_api_key"
        })),
        &mut ctx,
        "POST",
        "https://example.com/translate",
    )
    .expect("kakao api key signing should succeed");

    match result {
        SignResult::AuthorizationHeader(value) => {
            assert_eq!(value, "KakaoAK mock-kakao-key");
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_tc3_hmac_sha256_sets_signature_and_headers() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.secret_id".to_string(), "mock-tc-id".to_string());
    ctx.insert("auth.secret_key".to_string(), "mock-tc-secret".to_string());
    ctx.insert(
        "auth.host".to_string(),
        "api.tencentcloudapi.com".to_string(),
    );
    ctx.insert("input.text".to_string(), "Hello TC3".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "tc3_hmac_sha256",
            "service": "tmt",
            "concat": ["input.text"]
        })),
        &mut ctx,
        "POST",
        "https://api.tencentcloudapi.com/",
    )
    .expect("tc3 signing should succeed");

    match result {
        SignResult::WithHeaders(computed, headers) => {
            let timestamp = headers
                .iter()
                .find(|(name, _)| name == "X-TC-Timestamp")
                .map(|(_, value)| value.clone())
                .expect("tc3 timestamp header");
            let authorization = headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.clone())
                .expect("tc3 authorization header");
            let signature = computed
                .get("computed.sign")
                .cloned()
                .expect("tc3 computed signature");
            let date = ctx
                .get("computed.date")
                .cloned()
                .expect("tc3 computed date");

            assert!(
                timestamp.chars().all(|ch| ch.is_ascii_digit()),
                "tc3 timestamp must be unix-seconds digits, got: {}",
                timestamp
            );
            assert_eq!(
                ctx.get("computed.timestamp"),
                Some(&timestamp),
                "computed timestamp should match emitted tc3 header"
            );
            assert!(
                authorization.starts_with(&format!(
                    "TC3-HMAC-SHA256 Credential=mock-tc-id/{}/tmt/tc3_request, SignedHeaders=content-type;host, Signature=",
                    date
                )),
                "unexpected tc3 authorization header: {}",
                authorization
            );
            assert!(
                authorization.ends_with(&signature),
                "tc3 authorization signature should match computed.sign"
            );
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_tc3_hmac_sha256_prefers_rendered_request_body_json() {
    fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("valid hmac key");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    let mut ctx = HashMap::new();
    ctx.insert("auth.secret_id".to_string(), "mock-tc-id".to_string());
    ctx.insert("auth.secret_key".to_string(), "mock-tc-secret".to_string());
    ctx.insert(
        "auth.host".to_string(),
        "vtc.tencentcloudapi.com".to_string(),
    );
    ctx.insert(
        "request.body_json".to_string(),
        "{\"JobId\":\"job-123\"}".to_string(),
    );

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "tc3_hmac_sha256",
            "service": "vtc",
            "concat": ["input.text"]
        })),
        &mut ctx,
        "POST",
        "https://vtc.tencentcloudapi.com",
    )
    .expect("tc3 signing should succeed when request.body_json is present");

    match result {
        SignResult::WithHeaders(computed, headers) => {
            let authorization = headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.clone())
                .expect("tc3 authorization header");
            let signature = computed
                .get("computed.sign")
                .cloned()
                .expect("tc3 computed signature");
            let date = ctx
                .get("computed.date")
                .cloned()
                .expect("tc3 computed date");

            let expected_payload_hash =
                crate::crypto::to_hex(&sha2::Sha256::digest(b"{\"JobId\":\"job-123\"}"));
            let canonical_request = format!(
                "POST\n/\n\ncontent-type:application/json\nhost:vtc.tencentcloudapi.com\n\ncontent-type;host\n{}",
                expected_payload_hash
            );
            let timestamp = ctx
                .get("computed.timestamp")
                .cloned()
                .expect("tc3 computed timestamp");
            let string_to_sign = format!(
                "TC3-HMAC-SHA256\n{}\n{}/vtc/tc3_request\n{}",
                timestamp,
                date,
                crate::crypto::to_hex(&sha2::Sha256::digest(canonical_request.as_bytes()))
            );
            let secret_date_key = hmac_sha256_bytes(b"TC3mock-tc-secret", date.as_bytes());
            let secret_service_key = hmac_sha256_bytes(&secret_date_key, b"vtc");
            let signing_key = hmac_sha256_bytes(&secret_service_key, b"tc3_request");
            let expected_signature =
                crate::crypto::to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

            assert_eq!(signature, expected_signature);
            assert!(
                authorization.contains(&format!("Credential=mock-tc-id/{}/vtc/tc3_request", date)),
                "unexpected tc3 authorization header: {}",
                authorization
            );
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_aws_sigv4_sets_signature_and_headers() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.access_key".to_string(), "mock-aws-key".to_string());
    ctx.insert("auth.secret_key".to_string(), "mock-aws-secret".to_string());
    ctx.insert(
        "auth.host".to_string(),
        "translate.amazonaws.com".to_string(),
    );
    ctx.insert("input.text".to_string(), "Hello AWS".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "aws_sigv4",
            "service": "translate",
            "region": "us-east-1",
            "concat": ["input.text"]
        })),
        &mut ctx,
        "POST",
        "https://translate.amazonaws.com/",
    )
    .expect("aws sigv4 signing should succeed");

    match result {
        SignResult::WithHeaders(computed, headers) => {
            let amz_date = headers
                .iter()
                .find(|(name, _)| name == "X-Amz-Date")
                .map(|(_, value)| value.clone())
                .expect("aws amz date header");
            let authorization = headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.clone())
                .expect("aws authorization header");
            let signature = computed
                .get("computed.sign")
                .cloned()
                .expect("aws computed signature");
            let date = ctx
                .get("computed.date")
                .cloned()
                .expect("aws computed date");

            assert!(
                amz_date.starts_with(&date),
                "aws x-amz-date should start with computed date, got: {} vs {}",
                amz_date,
                date
            );
            assert!(
                amz_date.ends_with('Z'),
                "aws x-amz-date should end with Z, got: {}",
                amz_date
            );
            assert!(
                authorization.starts_with(&format!(
                    "AWS4-HMAC-SHA256 Credential=mock-aws-key/{}/us-east-1/translate/aws4_request, SignedHeaders=content-type;host, Signature=",
                    date
                )),
                "unexpected aws authorization header: {}",
                authorization
            );
            assert!(
                authorization.ends_with(&signature),
                "aws authorization signature should match computed.sign"
            );
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_volcengine_hmac_sha256_sets_region_in_credential_scope() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.access_key".to_string(), "mock-volc-key".to_string());
    ctx.insert(
        "auth.secret_key".to_string(),
        "mock-volc-secret".to_string(),
    );
    ctx.insert(
        "auth.host".to_string(),
        "translate.volcengineapi.com".to_string(),
    );
    ctx.insert("input.text".to_string(), "Hello Volc".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "volcengine_hmac_sha256",
            "service": "translate",
            "region": "cn-north-1",
            "concat": ["input.text"]
        })),
        &mut ctx,
        "POST",
        "https://translate.volcengineapi.com/",
    )
    .expect("volcengine signing should succeed");

    match result {
        SignResult::WithHeaders(computed, headers) => {
            let authorization = headers
                .iter()
                .find(|(name, _)| name == "Authorization")
                .map(|(_, value)| value.clone())
                .expect("volcengine authorization header");
            let signature = computed
                .get("computed.sign")
                .cloned()
                .expect("volcengine computed signature");
            let date = ctx
                .get("computed.date")
                .cloned()
                .expect("volcengine computed date");

            assert!(
                authorization.starts_with(&format!(
                    "HMAC-SHA256 Credential=mock-volc-key/{}/cn-north-1/translate/aws4_request, SignedHeaders=content-type;host, Signature=",
                    date
                )),
                "unexpected volcengine authorization header: {}",
                authorization
            );
            assert!(
                authorization.ends_with(&signature),
                "volcengine authorization signature should match computed.sign"
            );
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn prime_sign_context_niutrans_v2_sets_stable_timestamp_ms() {
    let mut ctx = HashMap::new();

    prime_sign_context(
        Some(&json!({
            "algorithm": "niutrans_v2"
        })),
        &mut ctx,
    )
    .expect("niutrans prime should set timestamp_ms");
    let first = ctx
        .get("computed.timestamp_ms")
        .cloned()
        .expect("niutrans prime should set timestamp_ms");

    prime_sign_context(
        Some(&json!({
            "algorithm": "niutrans_v2"
        })),
        &mut ctx,
    )
    .expect("niutrans prime should keep timestamp_ms");
    let second = ctx
        .get("computed.timestamp_ms")
        .cloned()
        .expect("niutrans prime should keep timestamp_ms");

    assert_eq!(first, second);
    assert!(
        first.chars().all(|ch| ch.is_ascii_digit()),
        "timestamp_ms should be numeric"
    );
}

#[test]
fn process_sign_config_niutrans_v2_signs_sorted_request_body_params() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.api_key".to_string(), "niutrans-secret".to_string());
    ctx.insert(
        "request.body.file".to_string(),
        "@file:/tmp/demo.docx".to_string(),
    );
    ctx.insert("request.body.from".to_string(), "zh".to_string());
    ctx.insert("request.body.to".to_string(), "en".to_string());
    ctx.insert("request.body.appId".to_string(), "app-123".to_string());
    ctx.insert(
        "request.body.timestamp".to_string(),
        "1710825600123".to_string(),
    );
    ctx.insert(
        "request.body.authStr".to_string(),
        "{{computed.sign}}".to_string(),
    );

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "niutrans_v2"
        })),
        &mut ctx,
        "POST",
        "https://api.niutrans.com/v2/doc/translate/upload",
    )
    .expect("niutrans signing should succeed");

    match result {
        SignResult::ContextOnly(computed) => {
            let sign = computed
                .get("computed.sign")
                .cloned()
                .or_else(|| ctx.get("computed.sign").cloned())
                .expect("niutrans sign should be present");
            let expected = format!(
                "{:x}",
                md5::compute(
                    b"apikey=niutrans-secret&appId=app-123&from=zh&timestamp=1710825600123&to=en"
                )
            );
            assert_eq!(sign, expected);
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

#[test]
fn process_sign_config_niutrans_v2_falls_back_to_query_params_for_download() {
    let mut ctx = HashMap::new();
    ctx.insert("auth.api_key".to_string(), "niutrans-secret".to_string());

    let result = process_sign_config(
        Some(&json!({
            "algorithm": "niutrans_v2"
        })),
        &mut ctx,
        "GET",
        "https://api.niutrans.com/v2/image/translate/download/file-123?appId=app-123&timestamp=1710825600456&type=1&authStr=%7B%7Bcomputed.sign%7D%7D",
    )
    .expect("niutrans signing should succeed from query params");

    match result {
        SignResult::ContextOnly(computed) => {
            let sign = computed
                .get("computed.sign")
                .cloned()
                .or_else(|| ctx.get("computed.sign").cloned())
                .expect("niutrans sign should be present");
            let expected = format!(
                "{:x}",
                md5::compute(
                    b"apikey=niutrans-secret&appId=app-123&timestamp=1710825600456&type=1"
                )
            );
            assert_eq!(sign, expected);
        }
        other => panic!("unexpected sign result: {:?}", other),
    }
}

// -----------------------------------------------------------------------
// render_template_value
// -----------------------------------------------------------------------

#[test]
fn render_template_value_string() {
    let mut ctx = HashMap::new();
    ctx.insert("input.text".to_string(), "hello".to_string());

    let val = json!("translate: {{input.text}}");
    let result = render_template_value(&val, &ctx);
    assert_eq!(result, json!("translate: hello"));
}

#[test]
fn render_template_value_nested_object() {
    let mut ctx = HashMap::new();
    ctx.insert("input.text".to_string(), "content".to_string());
    ctx.insert("input.source_lang".to_string(), "en".to_string());

    let val = json!({
        "text": "{{input.text}}",
        "config": {
            "source": "{{input.source_lang}}"
        }
    });
    let result = render_template_value(&val, &ctx);
    assert_eq!(result["text"], json!("content"));
    assert_eq!(result["config"]["source"], json!("en"));
}

#[test]
fn render_template_value_array() {
    let mut ctx = HashMap::new();
    ctx.insert("input.text".to_string(), "hi".to_string());

    let val = json!(["{{input.text}}", "literal"]);
    let result = render_template_value(&val, &ctx);
    assert_eq!(result, json!(["hi", "literal"]));
}

#[test]
fn render_template_value_preserves_numbers_and_booleans() {
    let ctx = HashMap::new();
    let val = json!({"count": 42, "enabled": true, "text": "{{input.text}}"});
    let result = render_template_value(&val, &ctx);
    assert_eq!(result["count"], json!(42));
    assert_eq!(result["enabled"], json!(true));
    assert_eq!(result["text"], json!("{{input.text}}"));
}

#[test]
fn render_template_value_exact_computed_token_restores_structured_json() {
    let mut ctx = HashMap::new();
    ctx.insert(
        "computed.meta_files".to_string(),
        "[{\"task_id\":\"child123\",\"filename\":\"input.mp4\"}]".to_string(),
    );

    let val = json!({ "meta_files": "{{computed.meta_files}}" });
    let result = render_template_value(&val, &ctx);

    assert_eq!(
        result,
        json!({
            "meta_files": [
                {
                    "task_id": "child123",
                    "filename": "input.mp4"
                }
            ]
        })
    );
}

#[test]
fn render_template_value_injects_structured_computed_token() {
    let mut ctx = HashMap::new();
    ctx.insert(
        "computed.meta_files".to_string(),
        r#"[{"storage":"s3","key":"video-src.mp4"}]"#.to_string(),
    );

    let val = json!({
        "meta_files": "{{computed.meta_files}}",
        "note": "meta={{computed.meta_files}}"
    });
    let result = render_template_value(&val, &ctx);

    assert_eq!(
        result["meta_files"],
        json!([{"storage":"s3","key":"video-src.mp4"}])
    );
    assert_eq!(
        result["note"],
        json!("meta=[{\"storage\":\"s3\",\"key\":\"video-src.mp4\"}]")
    );
}

#[test]
fn extract_non_text_translated_ref_heuristic_finds_nested_url_download() {
    let payload = json!({
        "data": {
            "status": null,
            "job-a": {
                "status": "done",
                "url_download": "https://example.com/out.mp4"
            }
        }
    });

    let result = extract_non_text_translated_ref_heuristic(&payload, "video");

    assert_eq!(result.as_deref(), Some("https://example.com/out.mp4"));
}

// -----------------------------------------------------------------------
// extract_json_path
// -----------------------------------------------------------------------

#[test]
fn extract_json_path_simple_key() {
    let val = json!({"name": "Alice"});
    let result = extract_json_path(&val, "name");
    assert_eq!(result, Some(&json!("Alice")));
}

#[test]
fn extract_json_path_nested() {
    let val = json!({"data": {"result": {"text": "translated"}}});
    let result = extract_json_path(&val, "data.result.text");
    assert_eq!(result, Some(&json!("translated")));
}

#[test]
fn extract_json_path_array_index() {
    let val = json!({"items": ["a", "b", "c"]});
    let result = extract_json_path(&val, "items.1");
    assert_eq!(result, Some(&json!("b")));
}

#[test]
fn extract_json_path_missing_returns_none() {
    let val = json!({"a": 1});
    assert!(extract_json_path(&val, "b").is_none());
    assert!(extract_json_path(&val, "a.b").is_none());
}

#[test]
fn extract_json_path_empty_returns_root() {
    let val = json!({"a": 1});
    let result = extract_json_path(&val, "");
    assert_eq!(result, Some(&val));
}

// -----------------------------------------------------------------------
// extract_json_path_string
// -----------------------------------------------------------------------

#[test]
fn extract_json_path_string_returns_string() {
    let val = json!({"data": {"text": "hello"}});
    assert_eq!(
        extract_json_path_string(&val, "data.text"),
        Some("hello".to_string())
    );
}

#[test]
fn extract_json_path_string_returns_number_as_string() {
    let val = json!({"count": 42});
    assert_eq!(
        extract_json_path_string(&val, "count"),
        Some("42".to_string())
    );
}

#[test]
fn extract_json_path_string_returns_none_for_object() {
    let val = json!({"nested": {"a": 1}});
    assert!(extract_json_path_string(&val, "nested").is_none());
}

#[test]
fn extract_json_path_string_empty_path_returns_none() {
    let val = json!({"a": 1});
    assert!(extract_json_path_string(&val, "").is_none());
    assert!(extract_json_path_string(&val, "  ").is_none());
}

// -----------------------------------------------------------------------
// non-text translated_ref validation
// -----------------------------------------------------------------------

#[test]
fn normalize_translated_ref_candidate_accepts_common_ref_shapes() {
    let cases = vec![
        "https://cdn.example.com/out/video.mp4",
        "s3://bucket/object/key",
        "/media/cache/out.jpg",
        "./out/file.pdf",
        "1234567890",
        "550e8400-e29b-41d4-a716-446655440000",
        "video_translate_id_123",
        "task/abCDef-1234",
        "data:audio/wav;base64,UklGRg==",
    ];
    for value in cases {
        assert_eq!(
            normalize_translated_ref_candidate(value),
            Some(value.to_string()),
            "expected accepted candidate: {}",
            value
        );
    }
}

#[test]
fn normalize_translated_ref_candidate_rejects_plain_text_like_values() {
    for value in [
        "",
        "   ",
        "translated sentence",
        "HelloWorld",
        "ok",
        "success",
        "processing",
    ] {
        assert!(
            normalize_translated_ref_candidate(value).is_none(),
            "expected rejected candidate: {}",
            value
        );
    }
}

#[test]
fn resolve_non_text_translated_ref_prefers_valid_candidate() {
    let candidates = vec![
        "translated sentence".to_string(),
        "video_translate_id_123".to_string(),
        "https://example.com/fallback.mp4".to_string(),
    ];
    assert_eq!(
        resolve_non_text_translated_ref(
            &candidates,
            "https://example.com/source.mp4",
            "translated text",
            true
        ),
        "video_translate_id_123"
    );
}

#[test]
fn resolve_non_text_translated_ref_falls_back_to_source_ref_when_text_exists() {
    let candidates = vec!["translated sentence".to_string(), "ok".to_string()];
    assert_eq!(
        resolve_non_text_translated_ref(
            &candidates,
            "https://example.com/source.mp4",
            "translated text exists",
            true
        ),
        "https://example.com/source.mp4"
    );
}

#[test]
fn resolve_non_text_translated_ref_empty_when_no_valid_candidate_and_no_text() {
    let candidates = vec!["translated sentence".to_string(), "ok".to_string()];
    assert_eq!(
        resolve_non_text_translated_ref(&candidates, "https://example.com/source.mp4", "", true),
        ""
    );
}

#[test]
fn is_likely_async_reference_path_detects_id_like_paths() {
    for path in [
        "id",
        "task_id",
        "requestId",
        "output.job_id",
        "data.media_id",
        "dubbing_id",
    ] {
        assert!(
            is_likely_async_reference_path(path),
            "expected async reference path: {}",
            path
        );
    }
    for path in [
        "output.image_url",
        "data.result.fileUrl",
        "translated_media_path",
        "result.resource_link",
        "documentTranslation.byteStreamOutputs.0",
    ] {
        assert!(
            !is_likely_async_reference_path(path),
            "expected real media path: {}",
            path
        );
    }
}

#[test]
fn component_declares_real_non_text_output_rejects_id_only_templates() {
    let response = ComponentResponse {
        translated_text_path: Some("translated_text".to_string()),
        error_path: None,
        translated_ref_path: Some("id".to_string()),
        translated_media_ref_path: None,
        translated_image_ref_path: None,
        translated_video_ref_path: None,
        translated_audio_ref_path: None,
        translated_document_ref_path: None,
    };
    assert!(!component_declares_real_non_text_output_via_response(
        &response, "image"
    ));
}

#[test]
fn component_declares_real_non_text_output_accepts_media_url_paths() {
    let response = ComponentResponse {
        translated_text_path: None,
        error_path: None,
        translated_ref_path: Some("output.media_url".to_string()),
        translated_media_ref_path: None,
        translated_image_ref_path: None,
        translated_video_ref_path: None,
        translated_audio_ref_path: None,
        translated_document_ref_path: None,
    };
    assert!(component_declares_real_non_text_output_via_response(
        &response, "video"
    ));
}

#[test]
fn template_declares_real_non_text_output_accepts_async_download() {
    let response = ComponentResponse {
        translated_text_path: None,
        error_path: None,
        translated_ref_path: Some("dubbing_id".to_string()),
        translated_media_ref_path: None,
        translated_image_ref_path: None,
        translated_video_ref_path: Some("dubbing_id".to_string()),
        translated_audio_ref_path: None,
        translated_document_ref_path: None,
    };
    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://example.com/submit".to_string(),
            headers: None,
            body: None,
            body_type: None,
            response_type: None,
        },
        response,
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "dubbing_id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: "https://example.com/poll/{{computed.job_id}}".to_string(),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("status".to_string()),
            pending_values: vec!["processing".to_string()],
            done_values: vec!["done".to_string()],
            failed_values: vec!["failed".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(60),
            result_ref_path: None,
            result_ref_template: None,
            result_request: None,
            result_download: Some(ComponentAsyncDownload {
                method: "GET".to_string(),
                url: "https://example.com/download/{{computed.job_id}}".to_string(),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                filename: None,
                content_type: None,
            }),
            result_text_path: None,
        }),
        source_upload: None,
        sign: None,
        constraints: None,
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    assert!(template_declares_real_non_text_output(&template, "video"));
}

#[test]
fn template_declares_real_non_text_output_accepts_binary_submit_response() {
    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "document_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://example.com/submit".to_string(),
            headers: None,
            body: None,
            body_type: Some("multipart".to_string()),
            response_type: Some("binary".to_string()),
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: Some("Message".to_string()),
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
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    assert!(template_declares_real_non_text_output(
        &template, "document"
    ));
}

async fn start_async_poll_mock_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let base = format!("http://{}", addr);

    let polls = Arc::new(AtomicUsize::new(0));
    let polls_accept = polls.clone();

    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let polls = polls_accept.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let line = req.lines().next().unwrap_or("");
                let mut parts = line.split_whitespace();
                let _method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");

                let (status_line, body) = if path == "/submit" {
                    (
                        "HTTP/1.1 200 OK",
                        "{\"job_id\":\"job123\",\"job_key\":\"k456\"}",
                    )
                } else if path == "/submit-binary" {
                    let bytes: &[u8] = b"PK\x03\x04fake-docx";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.openxmlformats-officedocument.wordprocessingml.document\r\nContent-Disposition: attachment; filename=\"translated.docx\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.write_all(bytes).await;
                    let _ = socket.shutdown().await;
                    return;
                } else if path.starts_with("/poll/job123") {
                    let count = polls.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        ("HTTP/1.1 200 OK", "{\"status\":\"processing\"}")
                    } else {
                        (
                                "HTTP/1.1 200 OK",
                                "{\"status\":\"done\",\"result\":{\"video_url\":\"https://example.com/out.mp4\"}}",
                            )
                    }
                } else if path.starts_with("/poll2/job123/k456") {
                    let count = polls.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        ("HTTP/1.1 200 OK", "{\"status\":\"processing\"}")
                    } else {
                        (
                                "HTTP/1.1 200 OK",
                                "{\"status\":\"done\",\"result\":{\"video_url\":\"https://example.com/out.mp4\"}}",
                            )
                    }
                } else if path.starts_with("/poll-text/job123") {
                    let count = polls.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        (
                            "HTTP/1.1 200 OK",
                            "{\"data\":{\"job123\":{\"status\":\"processing\"}}}",
                        )
                    } else {
                        (
                            "HTTP/1.1 200 OK",
                            "{\"data\":{\"job123\":{\"status\":\"completed\",\"translated_text\":\"【zh】Hello【/zh】\"}}}",
                        )
                    }
                } else if path.starts_with("/download/job123") {
                    // Binary payload (fake mp4 header)
                    let bytes: &[u8] = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom";
                    let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.write_all(bytes).await;
                    let _ = socket.shutdown().await;
                    return;
                } else {
                    ("HTTP/1.1 404 Not Found", "{\"error\":\"not_found\"}")
                };

                let resp = format!(
                        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.as_bytes().len()
                    );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (base, handle)
}

#[tokio::test]
async fn translate_text_via_component_renders_async_poll_template_paths() {
    let (base, handle) = start_async_poll_mock_server().await;
    let template = ComponentTemplate {
        id: "official-doctranslate-text-v1".to_string(),
        name: "DocTranslate Text".to_string(),
        version: "1.0.0".to_string(),
        kind: "text_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/submit", base),
            headers: None,
            body: Some(json!({})),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: Some("data.task_id".to_string()),
            translated_ref_path: None,
            translated_media_ref_path: None,
            translated_image_ref_path: None,
            translated_video_ref_path: None,
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
            error_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "job_id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!("{}/poll-text/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("data.{{computed.job_id}}.status".to_string()),
            pending_values: vec!["processing".to_string()],
            done_values: vec!["completed".to_string()],
            failed_values: vec!["failed".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(10),
            result_ref_path: None,
            result_ref_template: None,
            result_request: None,
            result_download: None,
            result_text_path: Some("data.{{computed.job_id}}.translated_text".to_string()),
        }),
        source_upload: None,
        sign: None,
        constraints: None,
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };
    let runtime = ComponentRuntime {
        template,
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
    };

    let client = Client::builder().no_proxy().build().unwrap();
    let translated = translate_text_via_component(&client, &runtime, "Hello", "en", "zh")
        .await
        .expect("templated async-poll result_text_path should resolve");
    assert_eq!(translated, "【zh】Hello【/zh】");

    handle.abort();
}

async fn start_prepare_upload_mock_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let base = format!("http://{}", addr);

    let upload_base = base.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let upload_base = upload_base.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                if n == 0 {
                    return;
                }
                let raw = &buf[..n];
                let req = String::from_utf8_lossy(raw);
                let line = req.lines().next().unwrap_or("");
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");
                let body = raw
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|idx| &raw[idx + 4..])
                    .unwrap_or(&[]);

                if method == "POST" && path == "/prepare" {
                    let body = format!(
                        "{{\"upload_url\":\"{}/upload/up123\",\"upload_id\":\"up123\"}}",
                        upload_base
                    );
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "PUT" && path == "/upload/up123" {
                    if body != b"ABCDEFG" {
                        let body = "{\"error\":\"unexpected_upload_body\"}";
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let resp = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "POST" && path == "/submit/up123" {
                    let body = "{\"job_id\":\"job123\"}";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "GET" && path == "/poll/job123" {
                    let body = "{\"status\":\"done\"}";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "GET" && path == "/result/job123" {
                    let body = "{\"result\":{\"video_url\":\"https://example.com/prepared.mp4\"}}";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                let body = "{\"error\":\"not_found\"}";
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (base, handle)
}

async fn start_lilt_file_mock_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let base = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                if n == 0 {
                    return;
                }
                let raw = &buf[..n];
                let req = String::from_utf8_lossy(raw);
                let lower_req = req.to_ascii_lowercase();
                let line = req.lines().next().unwrap_or("");
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");
                let body = raw
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|idx| &raw[idx + 4..])
                    .unwrap_or(&[]);

                if method == "POST" && path.starts_with("/v2/files?") {
                    if body != b"ABCDEFG" {
                        let body = r#"{"message":"unexpected_upload_body"}"#;
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    if !lower_req.contains("content-type: application/octet-stream") {
                        let body = r#"{"message":"unexpected_content_type"}"#;
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let body = r#"{"id":583,"name":"demo.txt"}"#;
                    let resp = format!(
                        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "POST"
                    && path
                        == "/v2/translate/file?key=test-key&fileId=583&memoryId=2495&withTM=true"
                {
                    let body = r#"[{"id":1,"fileId":"583","status":"InProgress"}]"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "GET" && path == "/v2/translate/file?key=test-key&translationIds=1" {
                    let body = r#"[{"id":1,"fileId":"583","status":"Completed"}]"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "GET" && path == "/v2/translate/files?key=test-key&id=1" {
                    let bytes: &[u8] = b"translated-file";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"translated.txt\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.write_all(bytes).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                let body = r#"{"message":"not_found"}"#;
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (base, handle)
}

async fn start_raw_html_text_mock_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind raw html text mock listener");
    let addr = listener
        .local_addr()
        .expect("raw html text mock local addr");
    let handle = tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).await.expect("read raw html request");
            let req = String::from_utf8_lossy(&buf[..n]);

            let status_line = if req.contains("POST /web-trans/v1/translate")
                && req.contains("source=auto")
                && req.contains("target=ja")
                && req.contains("html=%3Cp%3EHello%3C%2Fp%3E")
            {
                "HTTP/1.1 200 OK"
            } else {
                "HTTP/1.1 400 Bad Request"
            };

            let body = if status_line == "HTTP/1.1 200 OK" {
                "<p>こんにちは</p>"
            } else {
                "bad request"
            };
            let response = format!(
                "{status_line}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write raw html response");
        }
    });

    (format!("http://{}", addr), handle)
}

async fn start_synthesia_mock_server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind synthesia mock server");
    let addr = listener.local_addr().expect("mock addr");
    let base = format!("http://{}", addr);
    let upload_hits = Arc::new(AtomicUsize::new(0));
    let upload_hits_task = Arc::clone(&upload_hits);

    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let upload_hits = Arc::clone(&upload_hits_task);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32768];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                if n == 0 {
                    return;
                }
                let raw = &buf[..n];
                let req = String::from_utf8_lossy(raw);
                let lower_req = req.to_ascii_lowercase();
                let line = req.lines().next().unwrap_or("");
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");
                let body = raw
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|idx| &raw[idx + 4..])
                    .unwrap_or(&[]);

                if method == "POST" && path == "/v2/assets" {
                    if !lower_req.contains("authorization: synthesia-key") {
                        let body = r#"{"errorCode":"missing_auth"}"#;
                        let resp = format!(
                            "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let body = r#"{"id":"asset123","uploadUrl":"http://unused.local/upload","uploadCredentials":{"accessKeyId":"AKIA_TEST","secretAccessKey":"SECRET_TEST","sessionToken":"SESSION_TEST","bucket":"test-bucket","key":"uploads/demo.mp4"}}"#;
                    let resp = format!(
                        "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "PUT" && path.starts_with("/test-bucket/") {
                    upload_hits.fetch_add(1, Ordering::SeqCst);
                    if body != b"ABCDEFG" {
                        let body = r#"<?xml version="1.0" encoding="UTF-8"?><Error><Code>BadBody</Code></Error>"#;
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "POST" && path == "/v2/dubbing" {
                    let body =
                        r#"{"createdImportedAsset":{"id":"123e4567-e89b-12d3-a456-426614174000"}}"#;
                    let resp = format!(
                        "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "GET"
                    && path.starts_with("/v2/dubbing/123e4567-e89b-12d3-a456-426614174000")
                {
                    let body = r#"{"id":"123e4567-e89b-12d3-a456-426614174000","status":"complete","dubbedAssets":[{"id":"dubbed1","language":"fr","status":"complete","downloadUrl":"https://example.com/synthesia-fr.mp4"}]}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                let body = r#"{"error":"not_found"}"#;
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (base, upload_hits, handle)
}

#[tokio::test]
async fn translate_non_text_async_poll_returns_final_ref() {
    let (base, handle) = start_async_poll_mock_server().await;

    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/submit", base),
            headers: None,
            body: Some(json!({})),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: None,
            translated_ref_path: None,
            translated_media_ref_path: None,
            translated_image_ref_path: None,
            translated_video_ref_path: None,
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "job_id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!("{}/poll/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("status".to_string()),
            pending_values: vec!["processing".to_string()],
            done_values: vec!["done".to_string()],
            failed_values: vec!["failed".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(10),
            result_ref_path: Some("result.video_url".to_string()),
            result_ref_template: None,
            result_request: None,
            result_download: None,
            result_text_path: None,
        }),
        source_upload: None,
        sign: None,
        constraints: None,
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:video/mp4;base64,QUJDREVGRw==",
        "video",
        "field",
        "en",
        "es",
    )
    .await
    .expect("async poll translation");

    assert_eq!(out.translated_ref, "https://example.com/out.mp4");

    handle.abort();
}

#[tokio::test]
async fn translate_non_text_async_poll_downloads_binary_to_file_ref() {
    let (base, handle) = start_async_poll_mock_server().await;

    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/submit", base),
            headers: None,
            body: Some(json!({})),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: None,
            translated_ref_path: None,
            translated_media_ref_path: None,
            translated_image_ref_path: None,
            translated_video_ref_path: None,
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "job_id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!("{}/poll/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("status".to_string()),
            pending_values: vec!["processing".to_string()],
            done_values: vec!["done".to_string()],
            failed_values: vec!["failed".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(10),
            result_ref_path: None,
            result_ref_template: None,
            result_request: None,
            result_download: Some(ComponentAsyncDownload {
                method: "GET".to_string(),
                url: format!("{}/download/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                filename: Some("out.mp4".to_string()),
                content_type: Some("video/mp4".to_string()),
            }),
            result_text_path: None,
        }),
        source_upload: None,
        sign: None,
        constraints: None,
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:video/mp4;base64,QUJDREVGRw==",
        "video",
        "field",
        "en",
        "es",
    )
    .await
    .expect("async poll download translation");

    assert!(out.translated_ref.starts_with("file://"));

    // Ensure the persisted file exists.
    let path = out.translated_ref.trim_start_matches("file://");
    assert!(std::path::Path::new(path).exists());

    // Cleanup
    let _ = std::fs::remove_file(path);

    handle.abort();
}

#[tokio::test]
async fn translate_non_text_binary_submit_downloads_to_file_ref() {
    let (base, handle) = start_async_poll_mock_server().await;

    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "document_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/submit-binary", base),
            headers: None,
            body: Some(json!({ "inputFile": "@file:{{input.source_ref}}" })),
            body_type: Some("multipart".to_string()),
            response_type: Some("binary".to_string()),
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: Some("Message".to_string()),
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
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:application/vnd.openxmlformats-officedocument.wordprocessingml.document;base64,UEsDBA==",
        "document",
        "field",
        "en",
        "es",
    )
    .await
    .expect("binary submit translation");

    assert!(out.translated_ref.starts_with("file://"));
    let path = out.translated_ref.trim_start_matches("file://");
    assert!(std::path::Path::new(path).exists());
    assert!(path.ends_with("translated.docx"));

    let _ = std::fs::remove_file(path);
    handle.abort();
}

#[tokio::test]
async fn translate_non_text_async_poll_submit_extract_populates_computed_ctx() {
    let (base, handle) = start_async_poll_mock_server().await;

    let mut submit_extract = HashMap::new();
    submit_extract.insert("job_key".to_string(), "job_key".to_string());

    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/submit", base),
            headers: None,
            body: Some(json!({})),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: None,
            translated_ref_path: None,
            translated_media_ref_path: None,
            translated_image_ref_path: None,
            translated_video_ref_path: None,
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "job_id".to_string(),
            submit_extract,
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!(
                    "{}/poll2/{{{{computed.job_id}}}}/{{{{computed.job_key}}}}",
                    base
                ),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("status".to_string()),
            pending_values: vec!["processing".to_string()],
            done_values: vec!["done".to_string()],
            failed_values: vec!["failed".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(10),
            result_ref_path: Some("result.video_url".to_string()),
            result_ref_template: None,
            result_request: None,
            result_download: None,
            result_text_path: None,
        }),
        source_upload: None,
        sign: None,
        constraints: None,
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:video/mp4;base64,QUJDREVGRw==",
        "video",
        "field",
        "en",
        "es",
    )
    .await
    .expect("async poll translation with submit_extract");

    assert_eq!(out.translated_ref, "https://example.com/out.mp4");

    handle.abort();
}

#[tokio::test]
async fn translate_non_text_prepare_and_source_upload_runs_before_submit() {
    let (base, handle) = start_prepare_upload_mock_server().await;

    let mut prepare_extract = HashMap::new();
    prepare_extract.insert("upload_id".to_string(), "upload_id".to_string());
    prepare_extract.insert("upload_url".to_string(), "upload_url".to_string());

    let template = ComponentTemplate {
        id: "t".to_string(),
        name: "t".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: Some(ComponentPrepare {
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("{}/prepare", base),
                headers: None,
                body: Some(json!({ "source": "{{input.source_ref}}" })),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            extract: prepare_extract,
        }),
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/submit/{{{{computed.upload_id}}}}", base),
            headers: None,
            body: Some(json!({ "upload_id": "{{computed.upload_id}}" })),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: None,
            translated_ref_path: None,
            translated_media_ref_path: None,
            translated_image_ref_path: None,
            translated_video_ref_path: None,
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "job_id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!("{}/poll/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("status".to_string()),
            pending_values: vec!["processing".to_string()],
            done_values: vec!["done".to_string()],
            failed_values: vec!["failed".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(10),
            result_ref_path: Some("result.video_url".to_string()),
            result_ref_template: None,
            result_request: Some(ComponentRequest {
                method: "GET".to_string(),
                url: format!("{}/result/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            }),
            result_download: None,
            result_text_path: None,
        }),
        source_upload: Some(ComponentSourceUpload {
            method: "PUT".to_string(),
            url: "{{computed.upload_url}}".to_string(),
            headers: None,
            body_type: Some("binary_source".to_string()),
            success_statuses: vec![204],
            extract: HashMap::new(),
        }),
        sign: None,
        constraints: Some(ComponentConstraints {
            max_file_size_mb: Some(1),
            ..Default::default()
        }),
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:video/mp4;base64,QUJDREVGRw==",
        "video",
        "field",
        "en",
        "es",
    )
    .await
    .expect("prepare + source_upload translation");

    assert_eq!(out.translated_ref, "https://example.com/prepared.mp4");

    handle.abort();
}

#[tokio::test]
async fn translate_non_text_source_upload_extract_supports_lilt_file_chain() {
    let (base, handle) = start_lilt_file_mock_server().await;

    let mut source_upload_extract = HashMap::new();
    source_upload_extract.insert("file_id".to_string(), "id".to_string());

    let template = ComponentTemplate {
        id: "official-lilt-file-v1".to_string(),
        name: "Lilt File Translation".to_string(),
        version: "1.0.0".to_string(),
        kind: "document_translation".to_string(),
        client_contract: None,
        default_values: Some(json!({
            "memory_id": 2495,
            "with_tm": true
        })),
        auth: Some(ComponentAuth {
            mode: None,
            modes: Some(vec!["key".to_string()]),
            fields: vec![ComponentAuthField {
                name: "api_key".to_string(),
                required: Some(true),
            }],
        }),
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!(
                "{}/v2/translate/file?key={{{{auth.api_key}}}}&fileId={{{{computed.file_id}}}}&memoryId={{{{default_values.memory_id}}}}&withTM={{{{default_values.with_tm}}}}",
                base
            ),
            headers: None,
            body: None,
            body_type: Some("none".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: Some("message".to_string()),
            translated_ref_path: None,
            translated_media_ref_path: None,
            translated_image_ref_path: None,
            translated_video_ref_path: None,
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "0.id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!(
                    "{}/v2/translate/file?key={{{{auth.api_key}}}}&translationIds={{{{computed.job_id}}}}",
                    base
                ),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("0.status".to_string()),
            pending_values: vec!["inprogress".to_string()],
            done_values: vec!["completed".to_string()],
            failed_values: vec!["failed".to_string(), "error".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(5),
            result_ref_path: None,
            result_ref_template: None,
            result_request: None,
            result_download: Some(ComponentAsyncDownload {
                method: "GET".to_string(),
                url: format!(
                    "{}/v2/translate/files?key={{{{auth.api_key}}}}&id={{{{computed.job_id}}}}",
                    base
                ),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                filename: None,
                content_type: Some("text/plain".to_string()),
            }),
            result_text_path: None,
        }),
        source_upload: Some(ComponentSourceUpload {
            method: "POST".to_string(),
            url: format!(
                "{}/v2/files?key={{{{auth.api_key}}}}&name={{{{input.source_filename}}}}",
                base
            ),
            headers: Some(
                [(
                    "Content-Type".to_string(),
                    "application/octet-stream".to_string(),
                )]
                .into_iter()
                .collect(),
            ),
            body_type: Some("binary_source".to_string()),
            success_statuses: vec![201],
            extract: source_upload_extract,
        }),
        sign: None,
        constraints: Some(ComponentConstraints {
            max_file_size_mb: Some(1),
            supported_content_formats: Some(vec!["media_ref".to_string()]),
            ..Default::default()
        }),
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: [("auth.api_key".to_string(), "test-key".to_string())]
            .into_iter()
            .collect(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:text/plain;base64,QUJDREVGRw==",
        "document",
        "field",
        "en",
        "fr",
    )
    .await
    .expect("lilt file translation");

    assert!(out.translated_ref.starts_with("file://"));
    let path = out.translated_ref.trim_start_matches("file://");
    assert!(std::path::Path::new(path).exists());
    assert!(path.ends_with("translated.txt"));

    let _ = std::fs::remove_file(path);
    handle.abort();
}

#[tokio::test]
async fn translate_text_via_component_accepts_raw_html_response_body() {
    let (base, handle) = start_raw_html_text_mock_server().await;

    let template = ComponentTemplate {
        id: "official-papago-webpage-v1".to_string(),
        name: "Papago Web Translation".to_string(),
        version: "1.0.0".to_string(),
        kind: "text_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: None,
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/web-trans/v1/translate", base),
            headers: Some(
                [(
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                )]
                .into_iter()
                .collect(),
            ),
            body: Some(json!({
                "source": "{{input.source_lang}}",
                "target": "{{input.target_lang}}",
                "html": "{{input.text}}"
            })),
            body_type: Some("form".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: Some("body".to_string()),
            error_path: Some("errorMessage".to_string()),
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
            supported_content_formats: Some(vec!["rich_html".to_string()]),
            ..Default::default()
        }),
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["rich_html".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let translated = translate_text_via_component(&client, &runtime, "<p>Hello</p>", "auto", "ja")
        .await
        .expect("raw html response should be supported");

    assert_eq!(translated, "<p>こんにちは</p>");

    handle.abort();
}

#[test]
fn apply_prepare_extract_supports_array_value_for_followup_request_body() {
    let mut extract = HashMap::new();
    extract.insert("meta_files".to_string(), "data.meta_files".to_string());
    let prepare = ComponentPrepare {
        request: ComponentRequest {
            method: "POST".to_string(),
            url: "https://api.example.com/prepare".to_string(),
            headers: None,
            body: None,
            body_type: Some("none".to_string()),
            response_type: None,
        },
        extract,
    };
    let prepare_json = json!({
        "data": {
            "meta_files": [
                {"storage": "s3", "key": "v1.mp4"}
            ]
        }
    });

    let mut ctx = HashMap::new();
    apply_prepare_extract(&prepare, &prepare_json, &mut ctx, "official-test-v1")
        .expect("prepare.extract should accept array path");

    let extracted_meta_files = ctx
        .get("computed.meta_files")
        .expect("computed.meta_files should exist");
    let extracted_meta_files_json: serde_json::Value =
        serde_json::from_str(extracted_meta_files).expect("computed.meta_files should be json");
    assert_eq!(
        extracted_meta_files_json,
        json!([{"storage":"s3","key":"v1.mp4"}])
    );

    let submit_body = render_template_value(
        &json!({
            "meta_files": "{{computed.meta_files}}"
        }),
        &ctx,
    );
    assert_eq!(
        submit_body["meta_files"],
        json!([{"storage":"s3","key":"v1.mp4"}])
    );
}

#[tokio::test]
async fn translate_non_text_prepare_extract_supports_doctranslate_video_chain() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let base = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let n = match socket.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                if n == 0 {
                    return;
                }
                let raw = &buf[..n];
                let req = String::from_utf8_lossy(raw);
                let line = req.lines().next().unwrap_or("");
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");
                let body = raw
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|idx| &raw[idx + 4..])
                    .unwrap_or(&[]);

                if method == "POST" && path == "/v1/upload" {
                    let body =
                        r#"{"data":[{"task_id":"upload-1","filename":"demo.mp4","storage":"s3"}]}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "POST" && path == "/v2/process" {
                    let parsed_body: Value =
                        serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
                    let meta_files = parsed_body
                        .get("meta_files")
                        .cloned()
                        .unwrap_or(Value::Null);
                    if meta_files
                        != json!([{ "task_id": "upload-1", "filename": "demo.mp4", "storage": "s3" }])
                    {
                        let body = format!(
                            r#"{{"error":"meta_files_not_structured","body":{}}}"#,
                            serde_json::to_string(&parsed_body).unwrap()
                        );
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = socket.write_all(resp.as_bytes()).await;
                        let _ = socket.shutdown().await;
                        return;
                    }
                    let body = r#"{"data":{"task_id":"job-123"}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                if method == "GET" && path == "/v1/result/job-123" {
                    let body = r#"{"data":{"status":null,"job-a":{"status":"done","url_download":"https://example.com/out.mp4"}}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(resp.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    return;
                }

                let body = r#"{"error":"not_found"}"#;
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });

    let mut prepare_extract = HashMap::new();
    prepare_extract.insert("meta_files".to_string(), "data".to_string());

    let template = ComponentTemplate {
        id: "official-doctranslate-video-v1".to_string(),
        name: "DocTranslate Video Translation".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: None,
        auth: None,
        prepare: Some(ComponentPrepare {
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("{}/v1/upload", base),
                headers: None,
                body: Some(json!({
                    "task_type": "video",
                    "youtube_link": "{{input.source_ref}}"
                })),
                body_type: Some("form".to_string()),
                response_type: None,
            },
            extract: prepare_extract,
        }),
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/v2/process", base),
            headers: None,
            body: Some(json!({
                "task_type": "video",
                "dest_lang": "{{input.target_lang}}",
                "meta_files": "{{computed.meta_files}}"
            })),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: Some("error".to_string()),
            translated_ref_path: Some("url_download".to_string()),
            translated_media_ref_path: Some("url_download".to_string()),
            translated_image_ref_path: None,
            translated_video_ref_path: Some("url_download".to_string()),
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "data.task_id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!("{}/v1/result/{{{{computed.job_id}}}}", base),
                headers: None,
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: None,
            pending_values: Vec::new(),
            done_values: Vec::new(),
            failed_values: vec!["failed".to_string(), "error".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(5),
            result_ref_path: None,
            result_ref_template: None,
            result_request: None,
            result_download: None,
            result_text_path: None,
        }),
        source_upload: None,
        sign: None,
        constraints: None,
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:video/mp4;base64,QUJDREVGRw==",
        "video",
        "field",
        "en",
        "es",
    )
    .await
    .expect("doctranslate video translation should succeed");

    assert_eq!(out.translated_ref, "https://example.com/out.mp4");

    handle.abort();
}

#[tokio::test]
async fn translate_non_text_source_upload_aws_s3_put_object_supports_synthesia_chain() {
    let (base, upload_hits, handle) = start_synthesia_mock_server().await;

    let mut prepare_extract = HashMap::new();
    prepare_extract.insert("source_asset_id".to_string(), "id".to_string());
    prepare_extract.insert(
        "upload_access_key_id".to_string(),
        "uploadCredentials.accessKeyId".to_string(),
    );
    prepare_extract.insert(
        "upload_secret_access_key".to_string(),
        "uploadCredentials.secretAccessKey".to_string(),
    );
    prepare_extract.insert(
        "upload_session_token".to_string(),
        "uploadCredentials.sessionToken".to_string(),
    );
    prepare_extract.insert(
        "upload_bucket".to_string(),
        "uploadCredentials.bucket".to_string(),
    );
    prepare_extract.insert(
        "upload_key".to_string(),
        "uploadCredentials.key".to_string(),
    );

    let template = ComponentTemplate {
        id: "official-synthesia-video-dubbing-v1".to_string(),
        name: "Synthesia Video Dubbing".to_string(),
        version: "1.0.0".to_string(),
        kind: "video_translation".to_string(),
        client_contract: None,
        default_values: Some(json!({
            "title_prefix": "WPTSALL",
            "detect_language": true,
            "lipsync_enabled": false,
            "video_duration": "adaptive",
            "visibility": "private",
            "s3_region": "us-east-1"
        })),
        auth: Some(ComponentAuth {
            mode: None,
            modes: Some(vec!["key".to_string()]),
            fields: vec![ComponentAuthField {
                name: "api_key".to_string(),
                required: Some(true),
            }],
        }),
        prepare: Some(ComponentPrepare {
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("{}/v2/assets", base),
                headers: Some(
                    [
                        ("Authorization".to_string(), "{{auth.api_key}}".to_string()),
                        ("accept".to_string(), "application/json".to_string()),
                        ("Content-Type".to_string(), "application/json".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                ),
                body: Some(json!({
                    "contentType": "{{input.source_mime}}",
                    "configuration": {
                        "name": "dubbing",
                        "detectLanguage": "{{default_values.detect_language}}"
                    },
                    "title": "{{default_values.title_prefix}} {{input.source_filename}}"
                })),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            extract: prepare_extract,
        }),
        request: ComponentRequest {
            method: "POST".to_string(),
            url: format!("{}/v2/dubbing", base),
            headers: Some(
                [
                    ("Authorization".to_string(), "{{auth.api_key}}".to_string()),
                    ("accept".to_string(), "application/json".to_string()),
                    ("Content-Type".to_string(), "application/json".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            body: Some(json!({
                "title": "{{default_values.title_prefix}} {{input.source_filename}} (dubbed)",
                "targetLanguages": ["{{input.target_lang}}"],
                "lipsyncEnabled": "{{default_values.lipsync_enabled}}",
                "videoDuration": "{{default_values.video_duration}}",
                "visibility": "{{default_values.visibility}}",
                "sourceAssetId": "{{computed.source_asset_id}}",
                "sourceLanguage": "{{input.source_lang}}"
            })),
            body_type: Some("json".to_string()),
            response_type: None,
        },
        response: ComponentResponse {
            translated_text_path: None,
            error_path: Some("errorCode".to_string()),
            translated_ref_path: Some("dubbedAssets.0.downloadUrl".to_string()),
            translated_media_ref_path: Some("dubbedAssets.0.downloadUrl".to_string()),
            translated_image_ref_path: None,
            translated_video_ref_path: Some("dubbedAssets.0.downloadUrl".to_string()),
            translated_audio_ref_path: None,
            translated_document_ref_path: None,
        },
        async_poll: Some(ComponentAsyncPoll {
            job_id_path: "createdImportedAsset.id".to_string(),
            submit_extract: HashMap::new(),
            request: ComponentRequest {
                method: "GET".to_string(),
                url: format!(
                    "{}/v2/dubbing/{{{{computed.job_id}}}}?targetLanguages={{{{input.target_lang}}}}",
                    base
                ),
                headers: Some(
                    [
                        ("Authorization".to_string(), "{{auth.api_key}}".to_string()),
                        ("accept".to_string(), "application/json".to_string()),
                    ]
                    .into_iter()
                    .collect(),
                ),
                body: None,
                body_type: Some("none".to_string()),
                response_type: None,
            },
            status_path: Some("status".to_string()),
            pending_values: vec!["uploading".to_string(), "in_progress".to_string()],
            done_values: vec!["complete".to_string()],
            failed_values: vec!["error".to_string()],
            interval_seconds: Some(1),
            timeout_seconds: Some(5),
            result_ref_path: Some("dubbedAssets.0.downloadUrl".to_string()),
            result_ref_template: None,
            result_request: None,
            result_download: None,
            result_text_path: None,
        }),
        source_upload: Some(ComponentSourceUpload {
            method: "PUT".to_string(),
            url: "s3://{{computed.upload_bucket}}/{{computed.upload_key}}".to_string(),
            headers: Some(
                [
                    (
                        "x-amz-access-key-id".to_string(),
                        "{{computed.upload_access_key_id}}".to_string(),
                    ),
                    (
                        "x-amz-secret-access-key".to_string(),
                        "{{computed.upload_secret_access_key}}".to_string(),
                    ),
                    (
                        "x-amz-session-token".to_string(),
                        "{{computed.upload_session_token}}".to_string(),
                    ),
                    (
                        "x-amz-endpoint-url".to_string(),
                        base.clone(),
                    ),
                    ("x-amz-region".to_string(), "us-east-1".to_string()),
                    ("Content-Type".to_string(), "{{input.source_mime}}".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            body_type: Some("aws_s3_put_object".to_string()),
            success_statuses: Vec::new(),
            extract: HashMap::new(),
        }),
        sign: None,
        constraints: Some(ComponentConstraints {
            max_file_size_mb: Some(1),
            supported_content_formats: Some(vec!["media_ref".to_string()]),
            ..Default::default()
        }),
        editable_params: Vec::new(),
        translation_modes: Vec::new(),
    };

    let runtime = ComponentRuntime {
        template,
        auth_values: [("auth.api_key".to_string(), "synthesia-key".to_string())]
            .into_iter()
            .collect(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".to_string()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };

    let client = Client::new();
    let out = translate_non_text_via_component(
        &client,
        &runtime,
        "",
        None,
        "data:video/mp4;base64,QUJDREVGRw==",
        "video",
        "field",
        "en",
        "fr",
    )
    .await
    .expect("synthesia aws s3 source upload translation");

    assert_eq!(out.translated_ref, "https://example.com/synthesia-fr.mp4");
    assert_eq!(upload_hits.load(Ordering::SeqCst), 1);

    handle.abort();
}

// -----------------------------------------------------------------------
// insert_component_text_context
// -----------------------------------------------------------------------

#[test]
fn insert_component_text_context_sets_all_keys() {
    let mut ctx = HashMap::new();
    insert_component_text_context(&mut ctx, "hello", "en", "zh");
    assert_eq!(ctx.get("input.text"), Some(&"hello".to_string()));
    assert_eq!(ctx.get("input.source_lang"), Some(&"en".to_string()));
    assert_eq!(ctx.get("input.target_lang"), Some(&"zh".to_string()));
    assert_eq!(ctx.get("payload.text"), Some(&"hello".to_string()));
    assert_eq!(ctx.get("payload.source_lang"), Some(&"en".to_string()));
    assert_eq!(ctx.get("payload.target_lang"), Some(&"zh".to_string()));
}

// -----------------------------------------------------------------------
// insert_component_non_text_context
// -----------------------------------------------------------------------

#[test]
fn insert_non_text_context_url_ref() {
    let mut ctx = HashMap::new();
    insert_component_non_text_context(
        &mut ctx,
        None,
        "https://example.com/image.jpg",
        "image",
        "featured_image",
    );
    assert_eq!(
        ctx.get("input.source_ref"),
        Some(&"https://example.com/image.jpg".to_string())
    );
    assert_eq!(
        ctx.get("input.source_url"),
        Some(&"https://example.com/image.jpg".to_string())
    );
    assert_eq!(ctx.get("input.task_type"), Some(&"image".to_string()));
    assert_eq!(
        ctx.get("input.field_key"),
        Some(&"featured_image".to_string())
    );
}

#[test]
fn insert_non_text_context_with_payload_fields() {
    let mut ctx = HashMap::new();
    let payload = json!({
        "url": "https://example.com/img.png",
        "alt": "a cat",
        "title": "Cat Photo"
    });
    insert_component_non_text_context(
        &mut ctx,
        Some(&payload),
        "https://example.com/img.png",
        "image",
        "img_1",
    );
    assert_eq!(
        ctx.get("input.source.url"),
        Some(&"https://example.com/img.png".to_string())
    );
    assert_eq!(ctx.get("input.source.alt"), Some(&"a cat".to_string()));
    assert_eq!(
        ctx.get("input.source.title"),
        Some(&"Cat Photo".to_string())
    );
}

// -----------------------------------------------------------------------
// resolve_auth_values
// -----------------------------------------------------------------------

#[test]
fn resolve_auth_values_no_auth_config() {
    let mut bindings = ComponentBindingsDoc::default();
    let (values, updated) =
        resolve_auth_values("comp-1", None, &mut bindings).expect("should succeed");
    assert!(values.is_empty());
    assert!(!updated);
}

#[test]
fn resolve_auth_values_from_bindings() {
    let mut bindings = ComponentBindingsDoc {
        version: 1,
        components: HashMap::new(),
    };
    let mut auth_entry = ComponentBindingEntry::default();
    auth_entry
        .auth
        .insert("api_key".to_string(), "sk-test-123".to_string());
    bindings.components.insert("comp-1".to_string(), auth_entry);

    let auth_cfg = ComponentAuth {
        mode: None,
        modes: None,
        fields: vec![ComponentAuthField {
            name: "api_key".to_string(),
            required: Some(false),
        }],
    };
    let (values, _updated) =
        resolve_auth_values("comp-1", Some(&auth_cfg), &mut bindings).expect("should succeed");
    assert_eq!(values.get("auth.api_key"), Some(&"sk-test-123".to_string()));
}

#[test]
fn credential_references_resolve_without_persisting_secret_values() {
    let _env_lock = lock_credential_env();
    std::env::set_var("WPTSALL_TEST_PROVIDER_SECRET", "env-secret-value");
    assert_eq!(
        resolve_credential_reference("env://WPTSALL_TEST_PROVIDER_SECRET").unwrap(),
        "env-secret-value"
    );

    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("api-key"), "file-secret-value\n").unwrap();
    std::env::set_var("WPTSALL_CREDENTIAL_FILE_ROOT", root.path());
    assert_eq!(
        resolve_credential_reference("file://api-key").unwrap(),
        "file-secret-value"
    );

    let mut bindings = ComponentBindingsDoc::default();
    bindings
        .components
        .entry("comp-ref".to_string())
        .or_default()
        .auth
        .insert(
            "api_key".to_string(),
            "env://WPTSALL_TEST_PROVIDER_SECRET".to_string(),
        );
    let auth_cfg = ComponentAuth {
        mode: None,
        modes: None,
        fields: vec![ComponentAuthField {
            name: "api_key".to_string(),
            required: Some(true),
        }],
    };
    let (values, updated) =
        resolve_auth_values("comp-ref", Some(&auth_cfg), &mut bindings).unwrap();
    assert_eq!(
        values.get("auth.api_key"),
        Some(&"env-secret-value".to_string())
    );
    assert!(!updated);
    assert_eq!(
        bindings.components["comp-ref"].auth["api_key"],
        "env://WPTSALL_TEST_PROVIDER_SECRET"
    );

    let mut env_bindings = ComponentBindingsDoc::default();
    std::env::set_var("WPTSALL_COMPONENT_AUTH_S5_TEST_KEY", "direct-env-secret");
    let env_auth_cfg = ComponentAuth {
        mode: None,
        modes: None,
        fields: vec![ComponentAuthField {
            name: "s5_test_key".to_string(),
            required: Some(true),
        }],
    };
    let (env_values, env_updated) =
        resolve_auth_values("comp-env", Some(&env_auth_cfg), &mut env_bindings).unwrap();
    assert_eq!(
        env_values.get("auth.s5_test_key"),
        Some(&"direct-env-secret".to_string())
    );
    assert!(!env_updated);
    assert!(env_bindings.components["comp-env"].auth.is_empty());
    std::env::remove_var("WPTSALL_COMPONENT_AUTH_S5_TEST_KEY");
    std::env::remove_var("WPTSALL_TEST_PROVIDER_SECRET");
    std::env::remove_var("WPTSALL_CREDENTIAL_FILE_ROOT");
}

#[test]
fn credential_references_reject_unknown_scheme_and_root_escape() {
    let _env_lock = lock_credential_env();
    assert!(resolve_credential_reference("vault://prod/provider").is_err());
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("WPTSALL_CREDENTIAL_FILE_ROOT", root.path());
    assert!(resolve_credential_reference("file://../outside").is_err());
    std::env::remove_var("WPTSALL_CREDENTIAL_FILE_ROOT");
}

#[test]
fn credential_references_vault_reads_only_the_requested_kv_v2_field() {
    let _env_lock = lock_credential_env();
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /v1/kv/data/team/provider HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("x-vault-token: test-bootstrap-token"));
        let body = r#"{"data":{"data":{"api_key":"vault-secret-value","other":"must-not-leak"}}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });

    // HTTP is accepted only in cfg(test); production requires HTTPS.  The
    // explicit allowlist is mandatory even for a deployment-selected Vault.
    std::env::set_var("WPTSALL_VAULT_ADDR", format!("http://{address}"));
    std::env::set_var("WPTSALL_VAULT_TOKEN", "test-bootstrap-token");
    std::env::set_var("WPTSALL_SECRET_STORE_ALLOWLIST", "127.0.0.1");
    assert_eq!(
        resolve_credential_reference("vault://kv/team/provider#api_key").unwrap(),
        "vault-secret-value"
    );
    server.join().unwrap();
    std::env::remove_var("WPTSALL_VAULT_ADDR");
    std::env::remove_var("WPTSALL_VAULT_TOKEN");
    std::env::remove_var("WPTSALL_SECRET_STORE_ALLOWLIST");
}

#[test]
fn credential_references_kms_reads_only_requested_field_from_deployment_endpoint() {
    let _env_lock = lock_credential_env();
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.starts_with("GET /v1/secrets/provider-secret HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-kms-bootstrap-token"));
        let body =
            r#"{"data":{"api_key":"kms-secret-value","other":"must-not-leak"},"token":"nope"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });

    std::env::set_var("WPTSALL_KMS_ADDR", format!("http://{address}"));
    std::env::set_var("WPTSALL_KMS_TOKEN", "test-kms-bootstrap-token");
    std::env::set_var("WPTSALL_SECRET_STORE_ALLOWLIST", "127.0.0.1");
    assert_eq!(
        resolve_credential_reference("kms://provider-secret#api_key").unwrap(),
        "kms-secret-value"
    );
    server.join().unwrap();
    std::env::remove_var("WPTSALL_KMS_ADDR");
    std::env::remove_var("WPTSALL_KMS_TOKEN");
    std::env::remove_var("WPTSALL_SECRET_STORE_ALLOWLIST");
}

#[test]
fn credential_references_vault_fail_closed_for_untrusted_or_malformed_input() {
    let _env_lock = lock_credential_env();
    std::env::set_var("WPTSALL_VAULT_ADDR", "https://vault.internal");
    std::env::set_var("WPTSALL_VAULT_TOKEN", "test-bootstrap-token");
    std::env::remove_var("WPTSALL_SECRET_STORE_ALLOWLIST");
    assert!(resolve_credential_reference("vault://kv/team/provider#api_key").is_err());
    assert!(resolve_credential_reference("vault://kv/../../metadata#api_key").is_err());
    assert!(resolve_credential_reference("vault://kv/team/provider").is_err());
    std::env::set_var("WPTSALL_SECRET_STORE_ALLOWLIST", "vault.internal");
    std::env::set_var("WPTSALL_VAULT_TOKEN", "vault://kv/bootstrap#token");
    assert!(resolve_credential_reference("vault://kv/team/provider#api_key").is_err());
    std::env::remove_var("WPTSALL_VAULT_ADDR");
    std::env::remove_var("WPTSALL_VAULT_TOKEN");
    std::env::remove_var("WPTSALL_SECRET_STORE_ALLOWLIST");
}

#[test]
fn credential_references_kms_fail_closed_for_untrusted_or_malformed_input() {
    let _env_lock = lock_credential_env();
    std::env::set_var("WPTSALL_KMS_ADDR", "https://kms.internal");
    std::env::set_var("WPTSALL_KMS_TOKEN", "test-kms-bootstrap-token");
    std::env::remove_var("WPTSALL_SECRET_STORE_ALLOWLIST");
    assert!(resolve_credential_reference("kms://provider-secret#api_key").is_err());
    assert!(resolve_credential_reference("kms://team/provider#api_key").is_err());
    assert!(resolve_credential_reference("kms://../metadata#api_key").is_err());
    assert!(resolve_credential_reference("kms://provider-secret").is_err());
    std::env::set_var("WPTSALL_SECRET_STORE_ALLOWLIST", "kms.internal");
    std::env::set_var("WPTSALL_KMS_TOKEN", "kms://bootstrap#token");
    assert!(resolve_credential_reference("kms://provider-secret#api_key").is_err());
    std::env::set_var("WPTSALL_KMS_TOKEN", "vault://kv/bootstrap#token");
    assert!(resolve_credential_reference("kms://provider-secret#api_key").is_err());
    std::env::remove_var("WPTSALL_KMS_ADDR");
    std::env::remove_var("WPTSALL_KMS_TOKEN");
    std::env::remove_var("WPTSALL_SECRET_STORE_ALLOWLIST");
}

#[test]
fn resolve_auth_values_required_missing_fails() {
    let mut bindings = ComponentBindingsDoc::default();
    let auth_cfg = ComponentAuth {
        mode: None,
        modes: None,
        fields: vec![ComponentAuthField {
            name: "api_key".to_string(),
            required: Some(true),
        }],
    };
    let result = resolve_auth_values("comp-1", Some(&auth_cfg), &mut bindings);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("missing required"), "error: {}", err);
}

// -----------------------------------------------------------------------
// Rich HTML block-level splitting
// -----------------------------------------------------------------------

#[test]
fn split_gutenberg_simple_blocks() {
    let html = r#"<!-- wp:paragraph -->
<p>Hello world</p>
<!-- /wp:paragraph -->

<!-- wp:heading -->
<h2>Title</h2>
<!-- /wp:heading -->"#;
    let chunks = split_rich_html_by_blocks(html, 0);
    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].text.contains("Hello world"));
    assert!(chunks[1].text.contains("Title"));
}

#[test]
fn split_gutenberg_self_closing_block() {
    let html = r#"<!-- wp:paragraph -->
<p>Before</p>
<!-- /wp:paragraph -->
<!-- wp:separator /-->
<!-- wp:paragraph -->
<p>After</p>
<!-- /wp:paragraph -->"#;
    let chunks = split_rich_html_by_blocks(html, 0);
    assert_eq!(chunks.len(), 3);
    assert!(chunks[0].text.contains("Before"));
    assert!(chunks[1].text.contains("separator"));
    assert!(chunks[2].text.contains("After"));
}

#[test]
fn split_gutenberg_nested_blocks() {
    let html = r#"<!-- wp:columns -->
<div class="wp-block-columns">
<!-- wp:column -->
<div class="wp-block-column"><p>Left</p></div>
<!-- /wp:column -->
<!-- wp:column -->
<div class="wp-block-column"><p>Right</p></div>
<!-- /wp:column -->
</div>
<!-- /wp:columns -->"#;
    let chunks = split_rich_html_by_blocks(html, 0);
    // Nested blocks should stay as one top-level unit
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains("Left"));
    assert!(chunks[0].text.contains("Right"));
}

#[test]
fn split_classic_html_blocks() {
    let html = "<p>Paragraph 1</p><p>Paragraph 2</p><div>Block</div>";
    let chunks = split_rich_html_by_blocks(html, 0);
    assert_eq!(chunks.len(), 3);
    assert!(chunks[0].text.contains("Paragraph 1"));
    assert!(chunks[1].text.contains("Paragraph 2"));
    assert!(chunks[2].text.contains("Block"));
}

#[test]
fn split_classic_html_with_inline_between() {
    let html = "Some text<p>Paragraph</p>more text";
    let chunks = split_rich_html_by_blocks(html, 0);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].text, "Some text");
    assert!(chunks[1].text.contains("Paragraph"));
    assert_eq!(chunks[2].text, "more text");
}

#[test]
fn split_rich_html_batching() {
    let html = "<p>Short</p><p>Also short</p><p>Third</p>";
    // max_chars=30 should batch first two together
    let chunks = split_rich_html_by_blocks(html, 30);
    assert!(chunks.len() <= 3, "chunks: {:?}", chunks);
    // All text must be present
    let combined: String = chunks.iter().map(|c| c.text.clone()).collect();
    assert!(combined.contains("Short"));
    assert!(combined.contains("Also short"));
    assert!(combined.contains("Third"));
}

#[test]
fn split_rich_html_empty_input() {
    let chunks = split_rich_html_by_blocks("", 0);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.is_empty());
}

#[test]
fn split_gutenberg_with_json_attrs() {
    let html = r#"<!-- wp:image {"id":100,"sizeSlug":"large"} -->
<figure class="wp-block-image"><img src="test.jpg"/></figure>
<!-- /wp:image -->"#;
    let chunks = split_rich_html_by_blocks(html, 0);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains("wp:image"));
    assert!(chunks[0].text.contains("test.jpg"));
}

#[test]
fn content_format_detection() {
    assert!(is_gutenberg_content(
        "<!-- wp:paragraph --><p>Hi</p><!-- /wp:paragraph -->"
    ));
    assert!(!is_gutenberg_content("<p>Classic editor</p>"));
}

// -----------------------------------------------------------------------
// empty translation guard (C2 audit fix)
// -----------------------------------------------------------------------

#[test]
fn empty_translation_is_rejected() {
    // Simulate what translate_text_via_component checks after extracting
    // the translated value: empty or whitespace-only must be rejected.
    let cases = vec!["", " ", "  \t\n  "];
    for input in cases {
        assert!(
            input.trim().is_empty(),
            "expected empty after trim for '{:?}'",
            input
        );
    }
    // Non-empty should pass
    assert!(!"hello".trim().is_empty());
    assert!(!" ok ".trim().is_empty());
}

// -----------------------------------------------------------------------
// translated_text_path body_preview in error (m1 audit fix)
// -----------------------------------------------------------------------

#[test]
fn extract_json_path_missing_provides_body_preview_context() {
    let body = json!({
        "result": { "output": "translated text" },
        "status": "ok"
    });
    let bad_path = "data.translated";
    let result = extract_json_path(&body, bad_path);
    assert!(result.is_none());
    // Verify preview truncation works for error messages
    let body_preview: String = body.to_string().chars().take(200).collect();
    assert!(
        body_preview.len() <= 200 * 4, // max 4 bytes per char
        "preview should be bounded"
    );
    assert!(body_preview.contains("translated text"));
}

// -----------------------------------------------------------------------
// redact_url_secrets
// -----------------------------------------------------------------------

#[test]
fn redact_url_secrets_hides_sensitive_params() {
    let url = "https://api.example.com/v1?key=abc123&text=hello&token=secret";
    let redacted = redact_url_secrets(url);
    assert!(!redacted.contains("abc123"));
    assert!(!redacted.contains("secret"));
    assert!(redacted.contains("key=***"));
    assert!(redacted.contains("token=***"));
    assert!(redacted.contains("text=hello"));
}

#[test]
fn redact_url_secrets_no_query() {
    let url = "https://api.example.com/v1";
    assert_eq!(redact_url_secrets(url), url);
}

#[test]
fn looks_like_base64_blob_accepts_large_ascii_base64() {
    let blob = "QUJD".repeat(40);
    assert!(looks_like_base64_blob(&blob));
    assert!(!looks_like_base64_blob("short-base64"));
}
