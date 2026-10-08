//! E2E tests: Client signing algorithms against mock-translate-api.
//!
//! Uses WPTSALL_TEST_MOCK_API_BASE for an owned loopback mock, default :9090.
//! Run: `cargo test --test sign_e2e -- --nocapture`

use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;

fn mock_api_base() -> &'static str {
    static BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BASE.get_or_init(|| {
        let base = std::env::var("WPTSALL_TEST_MOCK_API_BASE")
            .unwrap_or_else(|_| "http://127.0.0.1:9090".into());
        let parsed = url::Url::parse(&base).expect("owned mock base URL");
        assert_eq!(parsed.scheme(), "http");
        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
        assert!(parsed.username().is_empty() && parsed.password().is_none());
        assert!(parsed.query().is_none() && parsed.fragment().is_none());
        assert_eq!(parsed.path(), "/");
        base.trim_end_matches('/').to_string()
    })
    .as_str()
}

/// Helper: send POST JSON and return response body.
async fn post_json(url: &str, body: &Value, headers: Vec<(&str, &str)>) -> (u16, Value) {
    let client = Client::new();
    let mut req = client.post(url).json(body);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await.expect("request failed");
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.expect("json parse failed");
    (status, body)
}

/// Helper: send POST form and return response body.
async fn post_form(
    url: &str,
    params: &[(String, String)],
    headers: Vec<(&str, &str)>,
) -> (u16, Value) {
    let client = Client::new();
    let mut req = client.post(url).form(params);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await.expect("request failed");
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.expect("json parse failed");
    (status, body)
}

fn assert_translated(body: &Value, expected_text: &str, expected_target: &str) {
    let translated = body
        .get("translated_text")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let marker = format!(
        "\u{3010}{}\u{3011}{}\u{3010}/{}\u{3011}",
        expected_target, expected_text, expected_target
    );
    assert_eq!(
        translated, marker,
        "Expected translation marker, got: {}",
        translated
    );
}

/// Provider-native response shapes (catalog parity): extract the translated
/// marker from a nested JSON path instead of the unified top-level
/// `translated_text` field. Tencent -> Response.TargetText, AWS ->
/// TranslatedText, Volcengine -> Translation, Alibaba -> Data.TranslatedText.
fn assert_translated_at(body: &Value, expected_text: &str, expected_target: &str, path: &[&str]) {
    let mut current = body;
    for key in path {
        current = current.get(*key).unwrap_or(&Value::Null);
    }
    let translated = current.as_str().unwrap_or("");
    let marker = format!(
        "\u{3010}{}\u{3011}{}\u{3010}/{}\u{3011}",
        expected_target, expected_text, expected_target
    );
    assert_eq!(
        translated,
        marker,
        "Expected translation marker at {}, got: {}",
        path.join("."),
        translated
    );
}

fn assert_auth_failed(body: &Value, expected_algorithm: &str) {
    assert_eq!(
        body.get("error").and_then(|v| v.as_str()),
        Some("auth_failed")
    );
    assert_eq!(
        body.get("algorithm").and_then(|v| v.as_str()),
        Some(expected_algorithm)
    );
}

// ─── Category A: Concat+Hash ────────────────────────────────────────────────

#[tokio::test]
async fn test_baidu_md5_sign() {
    let appid = "mock-appid-001";
    let secret = "mock-secret-baidu";
    let salt = "test-salt-123";
    let q = "Hello World";

    // Compute MD5 sign: md5(appid + q + salt + secret)
    let concat = format!("{}{}{}{}", appid, q, salt, secret);
    let sign = format!("{:x}", md5::compute(concat.as_bytes()));

    let body = serde_json::json!({
        "q": q, "from": "en", "to": "zh",
        "appid": appid, "salt": salt, "sign": sign
    });
    let (status, resp) = post_json(
        &format!("{}/api/baidu/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "Baidu MD5: {}", resp);
    assert_translated(&resp, q, "zh");
}

#[tokio::test]
async fn test_baidu_md5_wrong_sign() {
    let body = serde_json::json!({
        "q": "test", "from": "en", "to": "zh",
        "appid": "mock-appid-001", "salt": "123", "sign": "wrong"
    });
    let (status, resp) = post_json(
        &format!("{}/api/baidu/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 401);
    assert_auth_failed(&resp, "md5");
}

#[tokio::test]
async fn test_youdao_sha256_sign() {
    use sha2::{Digest, Sha256};

    let app_key = "mock-appkey-youdao";
    let app_secret = "mock-secret-youdao";
    let q = "Hello";
    let salt = "test-uuid-1234";
    let curtime = "1700000000";

    // Youdao: sha256(app_key + q + salt + curtime + app_secret) (q < 20 chars, no truncation)
    let concat = format!("{}{}{}{}{}", app_key, q, salt, curtime, app_secret);
    let hash = Sha256::digest(concat.as_bytes());
    let sign: String = hash.iter().map(|b| format!("{:02x}", b)).collect();

    let body = serde_json::json!({
        "q": q, "from": "en", "to": "zh",
        "appKey": app_key, "salt": salt, "sign": sign,
        "signType": "v3", "curtime": curtime
    });
    let (status, resp) = post_json(
        &format!("{}/api/youdao/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "Youdao SHA256: {}", resp);
    assert_translated(&resp, q, "zh");
}

#[tokio::test]
async fn test_youdao_sha256_long_text_truncation() {
    use sha2::{Digest, Sha256};

    let app_key = "mock-appkey-youdao";
    let app_secret = "mock-secret-youdao";
    let q = "abcdefghijklmnopqrstuvwxyz"; // 26 chars, triggers truncation
    let salt = "uuid-trunc-test";
    let curtime = "1700000000";

    // Youdao truncation: first10 + len + last10
    let truncated = format!("abcdefghij{}qrstuvwxyz", q.len());
    let concat = format!("{}{}{}{}{}", app_key, truncated, salt, curtime, app_secret);
    let hash = Sha256::digest(concat.as_bytes());
    let sign: String = hash.iter().map(|b| format!("{:02x}", b)).collect();

    let body = serde_json::json!({
        "q": q, "from": "en", "to": "zh",
        "appKey": app_key, "salt": salt, "sign": sign,
        "signType": "v3", "curtime": curtime
    });
    let (status, resp) = post_json(
        &format!("{}/api/youdao/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "Youdao truncation: {}", resp);
    assert_translated(&resp, q, "zh");
}

#[tokio::test]
async fn test_hmac_sha256_sign() {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let api_key = "mock-key-hmac";
    let secret = "mock-secret-hmac";
    let text = "Hello HMAC";
    let salt = "54321";

    let concat = format!("{}{}{}", api_key, text, salt);
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(concat.as_bytes());
    let sign: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    let body = serde_json::json!({
        "text": text, "source_lang": "en", "target_lang": "zh",
        "api_key": api_key, "salt": salt, "sign": sign
    });
    let (status, resp) = post_json(
        &format!("{}/api/hmac/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "HMAC-SHA256: {}", resp);
    assert_translated(&resp, text, "zh");
}

// ─── Category B: Canonical Request ──────────────────────────────────────────

fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn format_unix_date(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let days = secs / 86400;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}{:02}{:02}", y, m, d)
}

#[tokio::test]
async fn test_tc3_hmac_sha256_sign() {
    use sha2::{Digest, Sha256};

    let secret_id = "mock-tc-id";
    let secret_key = "mock-tc-secret";
    let host = mock_api_base().trim_start_matches("http://");
    let service = "tmt";

    let body_str = r#"{"SourceText":"Hello TC3","Source":"en","Target":"zh","ProjectId":0}"#;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let timestamp = now.to_string();
    let date = format_unix_date(now);

    let payload_hash = to_hex(&Sha256::digest(body_str.as_bytes()));
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
        host, payload_hash
    );
    let credential_scope = format!("{}/{}/tc3_request", date, service);
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{}\n{}\n{}",
        timestamp,
        credential_scope,
        to_hex(&Sha256::digest(canonical_request.as_bytes()))
    );

    let secret_date_key =
        hmac_sha256_bytes(format!("TC3{}", secret_key).as_bytes(), date.as_bytes());
    let secret_service_key = hmac_sha256_bytes(&secret_date_key, service.as_bytes());
    let signing_key = hmac_sha256_bytes(&secret_service_key, b"tc3_request");
    let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

    let authorization = format!(
        "TC3-HMAC-SHA256 Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
        secret_id, credential_scope, signature
    );

    let client = Client::new();
    let resp = client
        .post(&format!("{}/api/tencent/translate", mock_api_base()))
        .header("Content-Type", "application/json")
        .header("Host", host)
        .header("Authorization", &authorization)
        .header("X-TC-Timestamp", &timestamp)
        .body(body_str.to_string())
        .send()
        .await
        .expect("TC3 request failed");

    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "TC3-HMAC-SHA256: {}", body);
    assert_translated_at(&body, "Hello TC3", "zh", &["Response", "TargetText"]);
}

#[tokio::test]
async fn test_aws_sigv4_sign() {
    use sha2::{Digest, Sha256};

    let access_key = "mock-aws-key";
    let secret_key = "mock-aws-secret";
    let host = mock_api_base().trim_start_matches("http://");
    let service = "translate";
    let region = "us-east-1";

    let body_str = r#"{"Text":"Hello AWS","SourceLanguageCode":"en","TargetLanguageCode":"zh"}"#;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let date = format_unix_date(now);

    let payload_hash = to_hex(&Sha256::digest(body_str.as_bytes()));
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
        host, payload_hash
    );
    let credential_scope = format!("{}/{}/{}/aws4_request", date, region, service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}T000000Z\n{}\n{}",
        date,
        credential_scope,
        to_hex(&Sha256::digest(canonical_request.as_bytes()))
    );

    let date_key = hmac_sha256_bytes(format!("AWS4{}", secret_key).as_bytes(), date.as_bytes());
    let region_key = hmac_sha256_bytes(&date_key, region.as_bytes());
    let service_key = hmac_sha256_bytes(&region_key, service.as_bytes());
    let signing_key = hmac_sha256_bytes(&service_key, b"aws4_request");
    let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
        access_key, credential_scope, signature
    );

    let client = Client::new();
    let resp = client
        .post(&format!("{}/api/aws/translate", mock_api_base()))
        .header("Content-Type", "application/json")
        .header("Host", host)
        .header("Authorization", &authorization)
        .header("X-Amz-Date", &format!("{}T000000Z", date))
        .body(body_str.to_string())
        .send()
        .await
        .expect("AWS request failed");

    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "AWS SigV4: {}", body);
    assert_translated_at(&body, "Hello AWS", "zh", &["TranslatedText"]);
}

#[tokio::test]
async fn test_volcengine_hmac_sha256_sign() {
    use sha2::{Digest, Sha256};

    let access_key = "mock-volc-key";
    let secret_key = "mock-volc-secret";
    let host = mock_api_base().trim_start_matches("http://");
    let service = "translate";
    let region = "cn-north-1";

    let body_str = r#"{"SourceText":"Hello Volc","SourceLanguage":"en","TargetLanguage":"zh"}"#;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let date = format_unix_date(now);

    let payload_hash = to_hex(&Sha256::digest(body_str.as_bytes()));
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json\nhost:{}\n\ncontent-type;host\n{}",
        host, payload_hash
    );
    let credential_scope = format!("{}/{}/{}/aws4_request", date, region, service);
    let string_to_sign = format!(
        "HMAC-SHA256\n{}T000000Z\n{}\n{}",
        date,
        credential_scope,
        to_hex(&Sha256::digest(canonical_request.as_bytes()))
    );

    let date_key = hmac_sha256_bytes(format!("AWS4{}", secret_key).as_bytes(), date.as_bytes());
    let region_key = hmac_sha256_bytes(&date_key, region.as_bytes());
    let service_key = hmac_sha256_bytes(&region_key, service.as_bytes());
    let signing_key = hmac_sha256_bytes(&service_key, b"aws4_request");
    let signature = to_hex(&hmac_sha256_bytes(&signing_key, string_to_sign.as_bytes()));

    let authorization = format!(
        "HMAC-SHA256 Credential={}/{}, SignedHeaders=content-type;host, Signature={}",
        access_key, credential_scope, signature
    );

    let client = Client::new();
    let resp = client
        .post(&format!("{}/api/volcengine/translate", mock_api_base()))
        .header("Content-Type", "application/json")
        .header("Host", host)
        .header("Authorization", &authorization)
        .header("X-Amz-Date", &format!("{}T000000Z", date))
        .body(body_str.to_string())
        .send()
        .await
        .expect("Volcengine request failed");

    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "Volcengine: {}", body);
    assert_translated_at(&body, "Hello Volc", "zh", &["Translation"]);
}

// ─── Category C: Header Injection ───────────────────────────────────────────

#[tokio::test]
async fn test_azure_subscription_key() {
    let body = serde_json::json!({
        "text": "Hello Azure", "source_lang": "en", "target_lang": "zh"
    });
    let (status, resp) = post_json(
        &format!("{}/api/azure/translate", mock_api_base()),
        &body,
        vec![("Ocp-Apim-Subscription-Key", "mock-azure-sub-key")],
    )
    .await;
    assert_eq!(status, 200, "Azure: {}", resp);
}

#[tokio::test]
async fn test_azure_wrong_key() {
    let body = serde_json::json!({ "text": "test", "source_lang": "en", "target_lang": "zh" });
    let (status, resp) = post_json(
        &format!("{}/api/azure/translate", mock_api_base()),
        &body,
        vec![("Ocp-Apim-Subscription-Key", "wrong")],
    )
    .await;
    assert_eq!(status, 401);
    assert_auth_failed(&resp, "azure_subscription_key");
}

#[tokio::test]
async fn test_kakao_api_key() {
    let body = serde_json::json!({
        "query": "Hello Kakao", "source_lang": "en", "target_lang": "zh"
    });
    let (status, resp) = post_json(
        &format!("{}/api/kakao/translate", mock_api_base()),
        &body,
        vec![("Authorization", "KakaoAK mock-kakao-key")],
    )
    .await;
    assert_eq!(status, 200, "Kakao: {}", resp);
    assert_translated(&resp, "Hello Kakao", "zh");
}

#[tokio::test]
async fn test_kakao_wrong_key() {
    let body = serde_json::json!({ "query": "test", "source_lang": "en", "target_lang": "zh" });
    let (status, resp) = post_json(
        &format!("{}/api/kakao/translate", mock_api_base()),
        &body,
        vec![("Authorization", "KakaoAK wrong")],
    )
    .await;
    assert_eq!(status, 401);
    assert_auth_failed(&resp, "kakao_api_key");
}

// ─── Category D: Token Generation ───────────────────────────────────────────

#[tokio::test]
async fn test_oauth_flow() {
    // Step 1: Get token
    let token_body = serde_json::json!({
        "grant_type": "client_credentials",
        "client_id": "mock-oauth-client",
        "client_secret": "mock-oauth-secret"
    });
    let (status, resp) = post_json(
        &format!("{}/api/oauth/token", mock_api_base()),
        &token_body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "OAuth token: {}", resp);

    let token = resp
        .get("access_token")
        .and_then(|v| v.as_str())
        .expect("no access_token");
    assert!(!token.is_empty(), "token should not be empty");
    assert_eq!(
        resp.get("token_type").and_then(|v| v.as_str()),
        Some("Bearer")
    );

    // Step 2: Use token
    let auth_header = format!("Bearer {}", token);
    let body = serde_json::json!({
        "text": "Hello OAuth", "source_lang": "en", "target_lang": "zh"
    });
    let (status, resp) = post_json(
        &format!("{}/api/oauth/translate", mock_api_base()),
        &body,
        vec![("Authorization", &auth_header)],
    )
    .await;
    assert_eq!(status, 200, "OAuth translate: {}", resp);
    assert_translated(&resp, "Hello OAuth", "zh");
}

#[tokio::test]
async fn test_oauth_bad_credentials() {
    let body = serde_json::json!({
        "grant_type": "client_credentials",
        "client_id": "wrong",
        "client_secret": "wrong"
    });
    let (status, resp) = post_json(
        &format!("{}/api/oauth/token", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(
        resp.get("error").and_then(|v| v.as_str()),
        Some("invalid_client")
    );
}

// ─── WASM Plugin Tests ──────────────────────────────────────────────────────

/// Test WASM sign plugin produces correct MD5 signature.
/// Then verify it against mock-translate-api.
#[tokio::test]
async fn test_wasm_plugin_md5_e2e() {
    let wasm_path = concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/wptsall-sign.wasm");
    if !std::path::Path::new(wasm_path).exists() {
        eprintln!("WASM plugin not found at {}, skipping", wasm_path);
        return;
    }

    // Load plugin in a separate instance (to avoid global state conflicts)
    let wasm_bytes = std::fs::read(wasm_path).unwrap();
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let mut plugin = extism::Plugin::new(&manifest, [], true).expect("failed to load WASM plugin");

    let config = serde_json::json!({
        "algorithm": "md5",
        "salt_type": "none",
        "concat": ["auth.appid", "input.text", "computed.salt", "auth.secret_key"]
    });

    let mut ctx = HashMap::new();
    ctx.insert("auth.appid".to_string(), "mock-appid-001".to_string());
    ctx.insert(
        "auth.secret_key".to_string(),
        "mock-secret-baidu".to_string(),
    );
    ctx.insert("input.text".to_string(), "Hello WASM".to_string());
    ctx.insert("computed.salt".to_string(), "wasm-salt-1".to_string());

    let input = serde_json::json!({
        "algorithm": "md5",
        "config": config,
        "context": ctx,
    });
    let input_bytes = serde_json::to_vec(&input).unwrap();
    let output_bytes = plugin
        .call::<&[u8], Vec<u8>>("process_sign", &input_bytes)
        .expect("WASM call failed");
    let output: Value = serde_json::from_slice(&output_bytes).unwrap();

    assert_eq!(
        output.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "WASM output: {}",
        output
    );

    let computed_sign = output
        .get("computed")
        .and_then(|v| v.as_object())
        .and_then(|m| m.get("computed.sign"))
        .and_then(|v| v.as_str())
        .expect("no computed.sign in output");

    // Now verify against mock API
    let body = serde_json::json!({
        "q": "Hello WASM", "from": "en", "to": "zh",
        "appid": "mock-appid-001", "salt": "wasm-salt-1", "sign": computed_sign
    });
    let (status, resp) = post_json(
        &format!("{}/api/baidu/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "WASM MD5 E2E: {}", resp);
    assert_translated(&resp, "Hello WASM", "zh");
}

/// Test WASM sign plugin produces correct HMAC-SHA256 signature.
#[tokio::test]
async fn test_wasm_plugin_hmac_sha256_e2e() {
    let wasm_path = concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/wptsall-sign.wasm");
    if !std::path::Path::new(wasm_path).exists() {
        eprintln!("WASM plugin not found, skipping");
        return;
    }

    let wasm_bytes = std::fs::read(wasm_path).unwrap();
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let mut plugin = extism::Plugin::new(&manifest, [], true).expect("failed to load WASM plugin");

    let config = serde_json::json!({
        "algorithm": "hmac_sha256",
        "salt_type": "none",
        "concat": ["auth.api_key", "input.text", "computed.salt"]
    });

    let mut ctx = HashMap::new();
    ctx.insert("auth.api_key".to_string(), "mock-key-hmac".to_string());
    ctx.insert(
        "auth.secret_key".to_string(),
        "mock-secret-hmac".to_string(),
    );
    ctx.insert("input.text".to_string(), "Hello WASM HMAC".to_string());
    ctx.insert("computed.salt".to_string(), "hmac-salt".to_string());

    let input = serde_json::json!({ "algorithm": "hmac_sha256", "config": config, "context": ctx });
    let input_bytes = serde_json::to_vec(&input).unwrap();
    let output_bytes = plugin
        .call::<&[u8], Vec<u8>>("process_sign", &input_bytes)
        .expect("WASM call failed");
    let output: Value = serde_json::from_slice(&output_bytes).unwrap();

    assert_eq!(
        output.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "WASM output: {}",
        output
    );

    let computed_sign = output
        .get("computed")
        .and_then(|v| v.as_object())
        .and_then(|m| m.get("computed.sign"))
        .and_then(|v| v.as_str())
        .expect("no computed.sign");

    let body = serde_json::json!({
        "text": "Hello WASM HMAC", "source_lang": "en", "target_lang": "zh",
        "api_key": "mock-key-hmac", "salt": "hmac-salt", "sign": computed_sign
    });
    let (status, resp) = post_json(
        &format!("{}/api/hmac/translate", mock_api_base()),
        &body,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "WASM HMAC E2E: {}", resp);
    assert_translated(&resp, "Hello WASM HMAC", "zh");
}

/// Test WASM plugin lists supported algorithms.
#[tokio::test]
async fn test_wasm_plugin_list_algorithms() {
    let wasm_path = concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/wptsall-sign.wasm");
    if !std::path::Path::new(wasm_path).exists() {
        eprintln!("WASM plugin not found, skipping");
        return;
    }

    let wasm_bytes = std::fs::read(wasm_path).unwrap();
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let mut plugin = extism::Plugin::new(&manifest, [], true).expect("load");

    let output_bytes = plugin
        .call::<&[u8], Vec<u8>>("list_algorithms", b"")
        .expect("list call");
    let algorithms: Vec<String> = serde_json::from_slice(&output_bytes).unwrap();

    assert!(
        algorithms.contains(&"md5".to_string()),
        "should contain md5"
    );
    assert!(
        algorithms.contains(&"sha256".to_string()),
        "should contain sha256"
    );
    assert!(
        algorithms.contains(&"hmac_sha256".to_string()),
        "should contain hmac_sha256"
    );
    assert!(
        algorithms.contains(&"tc3_hmac_sha256".to_string()),
        "should contain tc3"
    );
    assert!(
        algorithms.contains(&"aws_sigv4".to_string()),
        "should contain aws"
    );
    assert!(
        algorithms.contains(&"bearer".to_string()),
        "should contain bearer"
    );
    assert!(
        algorithms.len() >= 13,
        "should have at least 13 algorithms, got {}",
        algorithms.len()
    );
}

/// Test WASM plugin version.
#[tokio::test]
async fn test_wasm_plugin_version() {
    let wasm_path = concat!(env!("CARGO_MANIFEST_DIR"), "/plugins/wptsall-sign.wasm");
    if !std::path::Path::new(wasm_path).exists() {
        return;
    }

    let wasm_bytes = std::fs::read(wasm_path).unwrap();
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let mut plugin = extism::Plugin::new(&manifest, [], true).expect("load");

    let output_bytes = plugin
        .call::<&[u8], Vec<u8>>("version", b"")
        .expect("version call");
    // Version output may be raw string bytes or JSON
    let version_str = if let Ok(v) = serde_json::from_slice::<Value>(&output_bytes) {
        match v {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            _ => String::from_utf8_lossy(&output_bytes).to_string(),
        }
    } else {
        String::from_utf8_lossy(&output_bytes).to_string()
    };
    assert!(!version_str.is_empty(), "version should not be empty");
    eprintln!("WASM plugin version: {}", version_str);
}

/// Test bearer auth (original mock API endpoint)
#[tokio::test]
async fn test_bearer_auth_original() {
    let body = serde_json::json!({
        "text": "Hello Bearer", "source_lang": "en", "target_lang": "zh"
    });
    let (status, resp) = post_json(
        &format!("{}/api/v1/translate/text", mock_api_base()),
        &body,
        vec![("Authorization", "Bearer mock-translate-dev-key-2026")],
    )
    .await;
    assert_eq!(status, 200, "Bearer: {}", resp);
    assert_translated(&resp, "Hello Bearer", "zh");
}

/// Test Alibaba V1 signing (form body with HMAC-SHA1 + Base64)
#[tokio::test]
async fn test_alibaba_v1_sign() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use hmac::{Hmac, Mac};

    let secret_key = "mock-ali-secret";

    let params = vec![
        ("AccessKeyId".to_string(), "mock-ali-key".to_string()),
        ("Action".to_string(), "TranslateGeneral".to_string()),
        ("Format".to_string(), "JSON".to_string()),
        ("FormatType".to_string(), "text".to_string()),
        ("SourceLanguage".to_string(), "en".to_string()),
        ("SourceText".to_string(), "Hello Alibaba".to_string()),
        ("TargetLanguage".to_string(), "zh".to_string()),
    ];

    // Sort params and build canonical string
    let mut sorted = params.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = sorted
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    fn url_encode(input: &str) -> String {
        let mut result = String::new();
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    result.push(byte as char);
                }
                _ => result.push_str(&format!("%{:02X}", byte)),
            }
        }
        result
    }

    let string_to_sign = format!("POST&%2F&{}", url_encode(&canonical));
    let sign_key = format!("{}&", secret_key);
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(sign_key.as_bytes()).unwrap();
    mac.update(string_to_sign.as_bytes());
    let signature = STANDARD.encode(mac.finalize().into_bytes());

    let mut form_params: Vec<(String, String)> = params;
    form_params.push(("Signature".to_string(), signature));

    let (status, resp) = post_form(
        &format!("{}/api/alibaba/translate", mock_api_base()),
        &form_params,
        vec![],
    )
    .await;
    assert_eq!(status, 200, "Alibaba V1: {}", resp);
    assert_translated_at(&resp, "Hello Alibaba", "zh", &["Data", "TranslatedText"]);
}
