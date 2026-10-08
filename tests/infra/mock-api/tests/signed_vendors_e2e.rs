//! Signed / keyed vendor e2e — real auth params, signatures, secrets vs mock verify.
//!
//! Covers catalog algorithms: MD5(baidu/niutrans/iflytek), SHA256(youdao), HMAC,
//! KakaoAK, Azure Ocp, OAuth token→Bearer, plus official path I/O shapes.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hmac::{Hmac, Mac};
use mock_translate_api::catalog_parity::get_at_path;
use mock_translate_api::config;
use mock_translate_api::{build_router, AppState};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn app() -> axum::Router {
    build_router(AppState::new(), "media/translated")
}

async fn call(
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let response = app().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "_raw": String::from_utf8_lossy(&bytes) }));
    (status, value)
}

fn md5_hex(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

fn sha256_hex(s: &str) -> String {
    let hash = Sha256::digest(s.as_bytes());
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

fn hmac_sha256_hex(secret: &str, data: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(data.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

#[tokio::test]
async fn e2e_baidu_md5_official_path_and_api_route() {
    let q = "hello";
    let salt = "999";
    let sign = md5_hex(&format!(
        "{}{}{}{}",
        config::BAIDU_APPID,
        q,
        salt,
        config::BAIDU_SECRET
    ));
    let body = format!(
        "appid={}&q={}&salt={}&sign={}&from=en&to=zh",
        config::BAIDU_APPID, q, salt, sign
    );

    for path in ["/api/trans/vip/translate", "/api/baidu/translate"] {
        let (st, v) = call(
            "POST",
            path,
            &[("content-type", "application/x-www-form-urlencoded")],
            &body,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{path} {v}");
        if path.starts_with("/api/trans") {
            assert!(
                get_at_path(&v, "trans_result.0.dst")
                    .and_then(Value::as_str)
                    .is_some(),
                "official baidu shape: {v}"
            );
        }
    }

    let bad = format!(
        "appid={}&q={}&salt={}&sign=deadbeef&from=en&to=zh",
        config::BAIDU_APPID, q, salt
    );
    let (st, v) = call(
        "POST",
        "/api/trans/vip/translate",
        &[("content-type", "application/x-www-form-urlencoded")],
        &bad,
    )
    .await;
    // Official Baidu returns HTTP 200 with error_code "54001" for invalid signatures
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["error_code"], "54001");
}

#[tokio::test]
async fn e2e_youdao_sha256_official_and_api() {
    let q = "hello";
    let salt = "salt1";
    let curtime = "1710000000";
    let truncated = q; // len <= 20
    let sign = sha256_hex(&format!(
        "{}{}{}{}{}",
        config::YOUDAO_APP_KEY,
        truncated,
        salt,
        curtime,
        config::YOUDAO_APP_SECRET
    ));
    let body = format!(
        "appKey={}&q={}&salt={}&curtime={}&sign={}&signType=v3&from=en&to=zh",
        config::YOUDAO_APP_KEY, q, salt, curtime, sign
    );

    for path in ["/api", "/api/youdao/translate"] {
        let (st, v) = call(
            "POST",
            path,
            &[("content-type", "application/x-www-form-urlencoded")],
            &body,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{path} {v}");
        if path == "/api" {
            assert!(get_at_path(&v, "translation.0").and_then(Value::as_str).is_some());
            // Official zhiyun envelope (BUG-MCK-03): errorCode "0" (string) =
            // success, `query` echoes the source, `l` is the from-2-to pair.
            assert_eq!(
                v.pointer("/errorCode").and_then(Value::as_str),
                Some("0"),
                "errorCode must be string \"0\": {v}"
            );
            assert_eq!(v.pointer("/query").and_then(Value::as_str), Some("hello"), "{v}");
            assert_eq!(v.pointer("/l").and_then(Value::as_str), Some("en-2-zh"), "{v}");
            assert_eq!(v.pointer("/isWord").and_then(Value::as_bool), Some(false), "{v}");
        }
    }
}

#[tokio::test]
async fn e2e_baidu_open_multiline_trans_result() {
    // Official open API (BUG-MCK-04): `\n`-separated q maps to one
    // trans_result {src, dst} entry per line, in order, plus root from/to.
    let q = "first\nsecond";
    let salt = "777";
    let sign = md5_hex(&format!(
        "{}{}{}{}",
        config::BAIDU_APPID,
        q,
        salt,
        config::BAIDU_SECRET
    ));
    // q is form-encoded (newline -> %0A); the sign covers the decoded q.
    let body = format!(
        "appid={}&q=first%0Asecond&salt={}&sign={}&from=en&to=zh",
        config::BAIDU_APPID, salt, sign
    );
    let (st, v) = call(
        "POST",
        "/api/trans/vip/translate",
        &[("content-type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let trans_result = v.pointer("/trans_result").and_then(Value::as_array);
    let rows = trans_result.unwrap_or_else(|| panic!("trans_result array: {v}"));
    assert_eq!(rows.len(), 2, "one entry per line: {v}");
    assert_eq!(
        rows[0].pointer("/src").and_then(Value::as_str),
        Some("first"),
        "{v}"
    );
    assert_eq!(
        rows[1].pointer("/src").and_then(Value::as_str),
        Some("second"),
        "{v}"
    );
    assert!(
        rows[0].pointer("/dst").and_then(Value::as_str).is_some_and(|t| !t.is_empty()),
        "{v}"
    );
    assert!(
        rows[1].pointer("/dst").and_then(Value::as_str).is_some_and(|t| !t.is_empty()),
        "{v}"
    );
    assert_eq!(v.pointer("/from").and_then(Value::as_str), Some("en"), "{v}");
    assert_eq!(v.pointer("/to").and_then(Value::as_str), Some("zh"), "{v}");
}

fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Days-since-epoch -> YYYYMMDD, matching TC3/SigV4 credential scopes.
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
async fn e2e_tencent_official_text_translate_envelope() {
    // Official TMT TextTranslate (BUG-MCK-05): Response carries RequestId +
    // echoed Source/Target alongside TargetText.
    let host = "127.0.0.1:9090";
    let body_str = r#"{"SourceText":"Hello TC3","Source":"en","Target":"zh","ProjectId":0}"#;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let timestamp = now.to_string();
    let date = format_unix_date(now);

    let payload_hash = sha256_hex(body_str);
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json\nhost:{host}\n\ncontent-type;host\n{payload_hash}"
    );
    let credential_scope = format!("{date}/tmt/tc3_request");
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp}\n{credential_scope}\n{}",
        sha256_hex(&canonical_request)
    );
    let secret_date = hmac_sha256_bytes(format!("TC3{}", config::TC3_SECRET_KEY).as_bytes(), date.as_bytes());
    let secret_service = hmac_sha256_bytes(&secret_date, b"tmt");
    let signing_key = hmac_sha256_bytes(&secret_service, b"tc3_request");
    // Final signature = HMAC(signing_key, string_to_sign).
    let signature = {
        let mut mac = Hmac::<Sha256>::new_from_slice(&signing_key).unwrap();
        mac.update(string_to_sign.as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    };
    let authorization = format!(
        "TC3-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders=content-type;host, Signature={signature}",
        config::TC3_SECRET_ID
    );

    let (st, v) = call(
        "POST",
        "/",
        &[
            ("content-type", "application/json"),
            ("host", host),
            ("authorization", &authorization),
            ("x-tc-timestamp", &timestamp),
            ("x-tc-action", "TextTranslate"),
        ],
        body_str,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        get_at_path(&v, "Response.TargetText").and_then(Value::as_str).is_some_and(|t| !t.is_empty()),
        "{v}"
    );
    assert!(
        v.pointer("/Response/RequestId").and_then(Value::as_str).is_some_and(|r| !r.is_empty()),
        "RequestId missing: {v}"
    );
    assert_eq!(v.pointer("/Response/Source").and_then(Value::as_str), Some("en"), "{v}");
    assert_eq!(v.pointer("/Response/Target").and_then(Value::as_str), Some("zh"), "{v}");
}

#[tokio::test]
async fn e2e_aws_official_translate_text_envelope() {
    // Official TranslateText (BUG-MCK-05): echoed SourceLanguageCode /
    // TargetLanguageCode alongside TranslatedText.
    let host = "127.0.0.1:9090";
    let body_str = r#"{"Text":"Hello AWS","SourceLanguageCode":"en","TargetLanguageCode":"zh"}"#;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let date = format_unix_date(now);

    let payload_hash = sha256_hex(body_str);
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json\nhost:{host}\n\ncontent-type;host\n{payload_hash}"
    );
    let credential_scope = format!("{date}/us-east-1/translate/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}T000000Z\n{credential_scope}\n{}",
        date,
        sha256_hex(&canonical_request)
    );
    let date_key = hmac_sha256_bytes(format!("AWS4{}", config::AWS_SECRET_KEY).as_bytes(), date.as_bytes());
    let region_key = hmac_sha256_bytes(&date_key, b"us-east-1");
    let service_key = hmac_sha256_bytes(&region_key, b"translate");
    let signing_key = hmac_sha256_bytes(&service_key, b"aws4_request");
    let signature = {
        let mut mac = Hmac::<Sha256>::new_from_slice(&signing_key).unwrap();
        mac.update(string_to_sign.as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    };
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders=content-type;host, Signature={signature}",
        config::AWS_ACCESS_KEY
    );

    let (st, v) = call(
        "POST",
        "/",
        &[
            ("content-type", "application/json"),
            ("host", host),
            ("authorization", &authorization),
            ("x-amz-date", &format!("{date}T000000Z")),
            ("x-amz-target", "AWSShineServiceFrontendService.TranslateText"),
        ],
        body_str,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        get_at_path(&v, "TranslatedText").and_then(Value::as_str).is_some_and(|t| !t.is_empty()),
        "{v}"
    );
    assert_eq!(
        v.pointer("/SourceLanguageCode").and_then(Value::as_str),
        Some("en"),
        "{v}"
    );
    assert_eq!(
        v.pointer("/TargetLanguageCode").and_then(Value::as_str),
        Some("zh"),
        "{v}"
    );
}

#[tokio::test]
async fn e2e_hmac_sha256_route() {
    let text = "hello";
    let salt = "123";
    let sign = hmac_sha256_hex(
        config::HMAC_SECRET,
        &format!("{}{}{}", config::HMAC_API_KEY, text, salt),
    );
    let body = json!({
        "api_key": config::HMAC_API_KEY,
        "text": text,
        "salt": salt,
        "sign": sign,
        "source_lang": "en",
        "target_lang": "zh"
    })
    .to_string();
    let (st, v) = call(
        "POST",
        "/api/hmac/translate",
        &[("content-type", "application/json")],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let bad = json!({
        "api_key": config::HMAC_API_KEY,
        "text": text,
        "salt": salt,
        "sign": "00",
        "source_lang": "en",
        "target_lang": "zh"
    })
    .to_string();
    let (st, _) = call(
        "POST",
        "/api/hmac/translate",
        &[("content-type", "application/json")],
        &bad,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn e2e_niutrans_md5_official_path() {
    let q = "hello";
    let from = "en";
    let to = "zh";
    let sign = md5_hex(&format!(
        "{}{}{}{}{}",
        config::NIUTRANS_API_KEY, q, from, to, config::NIUTRANS_API_KEY
    ));
    let body = format!(
        "apikey={}&q={}&from={}&to={}&sign={}",
        config::NIUTRANS_API_KEY, q, from, to, sign
    );
    let (st, v) = call(
        "POST",
        "/NiuTransServer/translation",
        &[("content-type", "application/x-www-form-urlencoded")],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "tgt_text").and_then(Value::as_str).is_some());

    let (st, _) = call(
        "POST",
        "/NiuTransServer/translation",
        &[("content-type", "application/x-www-form-urlencoded")],
        &format!(
            "apikey={}&q={}&from={}&to={}&sign=bad",
            config::NIUTRANS_API_KEY, q, from, to
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn e2e_iflytek_checksum_on_v1_its() {
    let cur_time = "1710000000";
    let x_param = "e30="; // base64 {}
    let checksum = md5_hex(&format!(
        "{}{}{}",
        config::IFLYTEK_API_SECRET, cur_time, x_param
    ));
    let body = json!({
        "text": "hello",
        "to": "zh",
        "data": { "text": "hello" },
        "business": { "to": "zh" }
    })
    .to_string();

    let (st, v) = call(
        "POST",
        "/v1/its",
        &[
            ("content-type", "application/json"),
            ("x-appid", config::IFLYTEK_APP_ID),
            ("x-curtime", cur_time),
            ("x-checksum", &checksum),
            ("x-param", x_param),
        ],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        get_at_path(&v, "data.trans_result.dst")
            .and_then(Value::as_str)
            .is_some(),
        "{v}"
    );

    let (st, _) = call(
        "POST",
        "/v1/its",
        &[
            ("content-type", "application/json"),
            ("x-appid", config::IFLYTEK_APP_ID),
            ("x-curtime", cur_time),
            ("x-checksum", "bad"),
            ("x-param", x_param),
        ],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn e2e_kakao_and_azure_and_deepl_keys() {
    let (st, v) = call(
        "POST",
        "/v2/translation/translate",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &format!("KakaoAK {}", config::KAKAO_KEY)),
        ],
        "query=hello&target_lang=zh",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translated_text.0.0").is_some() || v.get("translated_text").is_some());

    let (st, _) = call(
        "POST",
        "/api/azure/translate",
        &[
            ("content-type", "application/json"),
            ("ocp-apim-subscription-key", config::AZURE_SUB_KEY),
        ],
        &json!([{"Text":"hello"}]).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, v) = call(
        "POST",
        "/v2/translate",
        &[
            (
                "authorization",
                &format!("DeepL-Auth-Key {}", config::DEEPL_AUTH_KEY),
            ),
            ("content-type", "application/json"),
        ],
        &json!({"text":["hello"],"target_lang":"ZH"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&v, "translations.0.text").is_some());
}

#[tokio::test]
async fn e2e_oauth_client_credentials_then_translate() {
    let (st, token_resp) = call(
        "POST",
        "/api/oauth/token",
        &[("content-type", "application/json")],
        &json!({
            "grant_type": "client_credentials",
            "client_id": config::OAUTH_CLIENT_ID,
            "client_secret": config::OAUTH_CLIENT_SECRET
        })
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{token_resp}");
    let access = token_resp["access_token"].as_str().expect("access_token");

    let (st, v) = call(
        "POST",
        "/api/oauth/translate",
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Bearer {access}")),
        ],
        &json!({"text":"hello","target_lang":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn e2e_reverso_unbabel_huawei_shapes() {
    let (st, v) = call(
        "POST",
        "/translate/v1/translation",
        &[
            ("content-type", "application/json"),
            ("api-key", config::BEARER_KEY),
        ],
        &json!({"q":"hello","target":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translation.0").and_then(Value::as_str).is_some());

    let (st, v) = call(
        "POST",
        "/v1/translation",
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
        ],
        &json!({"text":"hello","target_lang":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translated_text").and_then(Value::as_str).is_some());

    let (st, v) = call(
        "POST",
        "/v1/infers/machine-translation/text-translation",
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
        ],
        &json!({"text":"hello","to":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translations.0.text").and_then(Value::as_str).is_some());
}

#[tokio::test]
async fn e2e_google_api_key_and_oauth_credentials() {
    // Official Google Translate v2 channel: via X-Goog-Api-Key header or ?key= query parameter.
    let (st, v) = call(
        "POST",
        "/language/translate/v2",
        &[
            ("content-type", "application/json"),
            ("x-goog-api-key", config::GOOGLE_API_KEY),
        ],
        &json!({
            "q": "hello",
            "source": "en",
            "target": "zh"
        })
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "data.translations.0.translatedText").is_some());

    // Strict channel check: passing key in body only is rejected with HTTP 403 unregistered callers
    let (st_body_only, v_body_only) = call(
        "POST",
        "/language/translate/v2",
        &[("content-type", "application/json")],
        &json!({
            "q": "hello",
            "source": "en",
            "target": "zh",
            "key": config::GOOGLE_API_KEY
        })
        .to_string(),
    )
    .await;
    assert_eq!(st_body_only, StatusCode::FORBIDDEN, "{v_body_only}");
    assert_eq!(
        v_body_only.pointer("/error/status").and_then(Value::as_str),
        Some("PERMISSION_DENIED")
    );

    let (st, v) = call(
        "POST",
        "/v3/projects/demo/locations/global:translateText",
        &[
            ("content-type", "application/json"),
            (
                "authorization",
                &format!("Bearer {}", config::GOOGLE_ACCESS_TOKEN),
            ),
        ],
        &json!({
            "contents": ["hello"],
            "sourceLanguageCode": "en",
            "targetLanguageCode": "zh"
        })
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translations.0.translatedText").is_some());
}
