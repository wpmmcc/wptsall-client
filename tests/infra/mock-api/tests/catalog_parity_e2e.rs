//! Mock-own e2e: exercise catalog-shaped paths against in-process router.
//!
//! Covers former E-gap vendors + DeepL/Matecat split + Microsoft/Papago/Libre.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use mock_translate_api::catalog_parity::{get_at_path, parity_paths};
use mock_translate_api::config;
use mock_translate_api::{build_router, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> axum::Router {
    build_router(AppState::new(), "media/translated")
}

async fn json_post(
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("POST").uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
    let response = app().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        json!({ "_raw": String::from_utf8_lossy(&bytes) })
    });
    (status, value)
}

#[tokio::test]
async fn e2e_health() {
    let req = Request::builder()
        .uri("/api/v1/health")
        .body(Body::empty())
        .unwrap();
    let response = app().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn e2e_openai_compatible_bearer() {
    let (status, value) = json_post(
        "/v1/chat/completions",
        &[
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
            ("content-type", "application/json"),
        ],
        &json!({
            "model": "gpt-4o-mini",
            "messages": [
                {"role":"system","content":"Translate to zh"},
                {"role":"user","content":"hello"}
            ]
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(!content.is_empty(), "openai content empty: {value}");
}

#[tokio::test]
async fn e2e_deepl_vs_matecat_same_path() {
    let deepl_body = json!({
        "source_lang": "EN",
        "target_lang": "ZH",
        "text": ["hello"]
    })
    .to_string();

    let (st, deepl) = json_post(
        "/v2/translate",
        &[
            (
                "authorization",
                &format!("DeepL-Auth-Key {}", config::DEEPL_AUTH_KEY),
            ),
            ("content-type", "application/json"),
        ],
        &deepl_body,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&deepl, "translations.0.text")
        .and_then(Value::as_str)
        .is_some());

    let matecat_body = json!({
        "q": "hello",
        "source": "en",
        "target": "zh",
        "text": "hello"
    })
    .to_string();
    let (st, matecat) = json_post(
        "/v2/translate",
        &[
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
            ("content-type", "application/json"),
        ],
        &matecat_body,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&matecat, "translation")
        .and_then(Value::as_str)
        .is_some());

    let (st, bad) = json_post(
        "/v2/translate",
        &[
            ("authorization", "DeepL-Auth-Key wrong-key"),
            ("content-type", "application/json"),
        ],
        &deepl_body,
    )
    .await;
    assert!(
        st == StatusCode::UNAUTHORIZED || st.as_u16() == 401 || st.as_u16() == 403 || !st.is_success(),
        "expected auth failure, got {st} {bad}"
    );
}

#[tokio::test]
async fn e2e_microsoft_ocp_key() {
    let (status, value) = json_post(
        "/translate?api-version=3.0&from=en&to=zh",
        &[
            ("content-type", "application/json"),
            ("ocp-apim-subscription-key", config::AZURE_SUB_KEY),
        ],
        &json!([{ "text": "hello" }]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(get_at_path(&value, "0.translations.0.text")
        .and_then(Value::as_str)
        .is_some());
}

#[tokio::test]
async fn e2e_microsoft_batch_full_mapping_official_keys() {
    // Official v3 batch (BUG-MCK-01): EVERY input item maps to one response
    // element with lowercase `translations[].text` + per-item `to` echo —
    // no truncation to the first item, no dual-case compatibility keys.
    let (status, value) = json_post(
        "/translate?api-version=3.0&from=en&to=zh",
        &[
            ("content-type", "application/json"),
            ("ocp-apim-subscription-key", config::AZURE_SUB_KEY),
        ],
        &json!([{ "Text": "hello" }, { "Text": "world" }]).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let items = value.as_array().unwrap_or_else(|| panic!("array response: {value}"));
    assert_eq!(items.len(), 2, "every input item must map: {value}");
    for (idx, item) in items.iter().enumerate() {
        let text = get_at_path(item, "translations.0.text").and_then(Value::as_str);
        assert!(
            text.is_some_and(|t| !t.is_empty()),
            "item {idx} missing translations.0.text: {value}"
        );
        assert_eq!(
            item.pointer("/translations/0/to").and_then(Value::as_str),
            Some("zh"),
            "item {idx} per-item `to` echo: {value}"
        );
        assert!(
            item.get("Translations").is_none(),
            "no dual-case keys in official shape: {value}"
        );
    }
}

#[tokio::test]
async fn e2e_deepl_string_form_and_query_auth() {
    // Single-string JSON `text` (BUG-MCK-02): one translations entry with
    // detected_source_language.
    let (st, v) = json_post(
        "/v2/translate",
        &[
            (
                "authorization",
                &format!("DeepL-Auth-Key {}", config::DEEPL_AUTH_KEY),
            ),
            ("content-type", "application/json"),
        ],
        &json!({ "text": "hello", "target_lang": "ZH" }).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        get_at_path(&v, "translations.0.text")
            .and_then(Value::as_str)
            .is_some(),
        "{v}"
    );
    assert!(
        v.pointer("/translations/0/detected_source_language")
            .and_then(Value::as_str)
            .is_some(),
        "{v}"
    );

    // Form-urlencoded multi-`text` + `?auth_key=` query auth (legacy
    // channel): one translations entry per text, in order.
    let (st, v) = json_post(
        &format!("/v2/translate?auth_key={}", config::DEEPL_AUTH_KEY),
        &[("content-type", "application/x-www-form-urlencoded")],
        "text=alpha&text=beta&target_lang=ZH",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        get_at_path(&v, "translations.0.text").and_then(Value::as_str).is_some(),
        "{v}"
    );
    assert!(
        get_at_path(&v, "translations.1.text").and_then(Value::as_str).is_some(),
        "multi-text must map in order: {v}"
    );

    // `auth_key` form-field channel (no header, no query param).
    let (st, v) = json_post(
        "/v2/translate",
        &[("content-type", "application/x-www-form-urlencoded")],
        &format!("text=gamma&target_lang=ZH&auth_key={}", config::DEEPL_AUTH_KEY),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        get_at_path(&v, "translations.0.text").and_then(Value::as_str).is_some(),
        "{v}"
    );

    // Wrong key via the query channel must fail auth.
    let (st, _) = json_post(
        "/v2/translate?auth_key=wrong-key",
        &[("content-type", "application/x-www-form-urlencoded")],
        "text=x&target_lang=ZH",
    )
    .await;
    assert!(!st.is_success(), "wrong auth_key must be rejected");
}

#[tokio::test]
async fn e2e_former_e_gap_paths_reject_bad_and_accept_good() {
    let cases: Vec<(
        &str,
        Vec<(&str, String)>,
        String,
        &str,
    )> = vec![
        (
            "/api/v1.5/tr.json/translate",
            vec![("content-type", "application/x-www-form-urlencoded".into())],
            format!(
                "text=hello&lang=en-zh&key={}",
                config::YANDEX_API_KEY
            ),
            "text.0",
        ),
        (
            "/v1/papago/n2mt",
            vec![
                ("content-type", "application/x-www-form-urlencoded".into()),
                ("x-naver-client-id", config::PAPAGO_CLIENT_ID.into()),
                ("x-naver-client-secret", config::PAPAGO_CLIENT_SECRET.into()),
            ],
            "source=en&target=zh&text=hello".into(),
            "message.result.translatedText",
        ),
        (
            "/api/v1/document/translate",
            vec![
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
                ("content-type", "application/json".into()),
            ],
            json!({"text":"hello","sourceLanguage":"en","targetLanguage":"zh"}).to_string(),
            "translatedText",
        ),
        (
            "/v2/projects/mt/translate",
            vec![
                ("authorization", format!("token {}", config::BEARER_KEY)),
                ("content-type", "application/json".into()),
            ],
            json!({"q":"hello","source":"en","target":"zh"}).to_string(),
            "translation",
        ),
        (
            "/api/v2/translations",
            vec![("authorization", format!("Bearer {}", config::BEARER_KEY))],
            String::new(),
            "0.0",
        ),
        (
            "/index.php",
            vec![
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
                ("content-type", "application/json".into()),
            ],
            json!({"q":"hello","text":"hello","source":"en","target":"zh"}).to_string(),
            "content.out",
        ),
        (
            "/api/transweb/translate",
            vec![
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
                ("content-type", "application/json".into()),
            ],
            json!({"text":"hello","target":"zh"}).to_string(),
            "data.translatedText",
        ),
        (
            "/api/v2/machines/translations",
            vec![
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
                ("content-type", "application/json".into()),
            ],
            json!({"text":"hello","target":"zh"}).to_string(),
            "data.0.text",
        ),
        (
            "/api2/projects/mt/translate",
            vec![
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
                ("content-type", "application/json".into()),
            ],
            json!({"text":"hello","target":"zh"}).to_string(),
            "translation",
        ),
    ];

    assert_eq!(cases.len(), parity_paths().len());

    for (path, headers, body, text_path) in cases {
        let header_refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();
        let (status, value) = json_post(path, &header_refs, &body).await;
        assert_eq!(status, StatusCode::OK, "path {path} failed: {value}");
        let got = get_at_path(&value, text_path).and_then(Value::as_str);
        assert!(
            got.is_some() && !got.unwrap().is_empty(),
            "path {path} missing {text_path}: {value}"
        );

        // Wrong credentials must fail for auth-gated routes.
        if path == "/v1/papago/n2mt" {
            let (st, _) = json_post(
                path,
                &[
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("x-naver-client-id", "bad"),
                    ("x-naver-client-secret", "bad"),
                ],
                "source=en&target=zh&text=hello",
            )
            .await;
            assert!(!st.is_success(), "papago should reject bad creds");
        } else if path.starts_with("/api/v1.5") {
            let (st, _) = json_post(
                path,
                &[("content-type", "application/x-www-form-urlencoded")],
                "text=hello&lang=en-zh&key=wrong",
            )
            .await;
            assert!(!st.is_success(), "yandex should reject bad key");
        } else if path != "/api/v2/translations" || true {
            let bad_auth = if path == "/v2/projects/mt/translate" {
                "token wrong"
            } else {
                "Bearer wrong"
            };
            let mut bad_headers: HashMap<&str, &str> = HashMap::new();
            for (k, v) in &header_refs {
                bad_headers.insert(*k, *v);
            }
            if bad_headers.contains_key("authorization") {
                bad_headers.insert("authorization", bad_auth);
                let pairs: Vec<(&str, &str)> = bad_headers.into_iter().collect();
                let (st, _) = json_post(path, &pairs, &body).await;
                assert!(!st.is_success(), "{path} should reject bad auth");
            }
        }
    }
}

#[tokio::test]
async fn e2e_libre_and_modernmt_and_systran() {
    let (st, v) = json_post(
        "/translate",
        &[("content-type", "application/json")],
        &json!({
            "api_key": config::BEARER_KEY,
            "q": "hello",
            "source": "en",
            "target": "zh"
        })
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&v, "translatedText").and_then(Value::as_str).is_some());

    let (st, _) = json_post(
        "/translate",
        &[("content-type", "application/json")],
        &json!({
            "api_key": "wrong",
            "q": "hello",
            "source": "en",
            "target": "zh"
        })
        .to_string(),
    )
    .await;
    assert!(!st.is_success());

    let (st, v) = json_post(
        "/translate",
        &[
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
            ("content-type", "application/json"),
        ],
        &json!({"q":"hello","source":"en","target":"zh","text":"hello"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&v, "data.translation")
        .and_then(Value::as_str)
        .is_some());

    let (st, v) = json_post(
        "/translation/text/translate",
        &[
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
            ("content-type", "application/json"),
        ],
        &json!({"input":"hello","source":"en","target":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&v, "outputs.0.output")
        .and_then(Value::as_str)
        .is_some());
}

#[tokio::test]
async fn e2e_fixture_inventory_matches_parity_module() {
    let raw = include_str!("../fixtures/catalog-http-mt-parity.json");
    let fixture: Value = serde_json::from_str(raw).unwrap();
    let entries = fixture["entries"].as_array().unwrap();
    assert!(entries.len() >= 40, "fixture too small: {}", entries.len());

    for path in parity_paths() {
        let hit = entries.iter().any(|e| e["path"].as_str() == Some(path));
        assert!(hit, "fixture missing parity path {path}");
    }
}
