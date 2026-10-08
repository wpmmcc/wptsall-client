//! Full auth-profile e2e: auth modes + complete I/O + input length limits.
//!
//! Each multi-auth vendor is exercised as **separate mock profiles** so WP plugin
//! Lab can map `entry_id` + auth fields 1:1.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use mock_translate_api::auth_profiles::{self, AuthScheme};
use mock_translate_api::catalog_parity::get_at_path;
use mock_translate_api::config;
use mock_translate_api::{build_router, AppState};
use serde_json::{json, Value};
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
    let req = builder
        .body(if body.is_empty() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap();
    let response = app().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "_raw": String::from_utf8_lossy(&bytes) }));
    (status, value)
}

#[tokio::test]
async fn inventory_endpoint_lists_split_google_and_azure() {
    let (st, v) = call("GET", "/api/v1/mock-auth-profiles", &[], "").await;
    assert_eq!(st, StatusCode::OK);
    let profiles = v["profiles"].as_array().expect("profiles");
    assert!(profiles.len() >= auth_profiles::all_profiles().len());
    let ids: Vec<&str> = profiles
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert!(ids.contains(&"google-v2-api-key-body"));
    assert!(ids.contains(&"google-v3-oauth"));
    assert!(ids.contains(&"azure-translator-subscription-key"));
    assert!(ids.contains(&"azure-translator-oauth"));
    let signed = v["signed_routes"].as_array().expect("signed_routes");
    assert!(signed.len() >= 10);
    assert!(signed.iter().any(|r| r["entry_id"] == "baidu"));
    assert!(signed.iter().any(|r| r["algorithm"] == "aws_sigv4"));
}

#[tokio::test]
async fn google_v2_three_key_placements_and_length() {
    let short = json!({
        "q": "hello",
        "source": "en",
        "target": "zh",
        "key": config::GOOGLE_API_KEY
    })
    .to_string();

    let (st, v) = call(
        "POST",
        "/mock/profiles/google-v2-api-key-body/translate",
        &[("content-type", "application/json")],
        &short,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(get_at_path(&v, "data.translations.0.translatedText")
        .and_then(Value::as_str)
        .is_some());

    let (st, v) = call(
        "POST",
        &format!(
            "/mock/profiles/google-v2-api-key-query/translate?key={}",
            config::GOOGLE_API_KEY
        ),
        &[("content-type", "application/json")],
        &json!({"q":"hello","source":"en","target":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "data.translations.0.translatedText").is_some());

    let (st, v) = call(
        "POST",
        "/mock/profiles/google-v2-api-key-header/translate",
        &[
            ("content-type", "application/json"),
            ("x-goog-api-key", config::GOOGLE_API_KEY),
        ],
        &json!({"q":"hello","source":"en","target":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    // bad key
    let (st, _) = call(
        "POST",
        "/mock/profiles/google-v2-api-key-body/translate",
        &[("content-type", "application/json")],
        &json!({"q":"hello","target":"zh","key":"wrong"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // official path also accepts query key
    let (st, _) = call(
        "POST",
        &format!("/language/translate/v2?key={}", config::GOOGLE_API_KEY),
        &[("content-type", "application/json")],
        &json!({"q":"hi","target":"zh"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // length limit 30000
    let too_long = "字".repeat(30_001);
    let (st, v) = call(
        "POST",
        "/mock/profiles/google-v2-api-key-body/translate",
        &[("content-type", "application/json")],
        &json!({
            "q": too_long,
            "target": "zh",
            "key": config::GOOGLE_API_KEY
        })
        .to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"], "input_too_long");
    assert_eq!(v["max_input_chars"], 30_000);
}

#[tokio::test]
async fn google_v3_oauth_vs_bearer_api_key_split() {
    let oauth_body = json!({
        "contents": ["hello world"],
        "sourceLanguageCode": "en",
        "targetLanguageCode": "zh"
    })
    .to_string();

    let (st, v) = call(
        "POST",
        "/mock/profiles/google-v3-oauth/translate",
        &[
            ("content-type", "application/json"),
            (
                "authorization",
                &format!("Bearer {}", config::GOOGLE_ACCESS_TOKEN),
            ),
        ],
        &oauth_body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translations.0.translatedText")
        .and_then(Value::as_str)
        .is_some());

    // wrong oauth token rejected
    let (st, _) = call(
        "POST",
        "/mock/profiles/google-v3-oauth/translate",
        &[
            ("content-type", "application/json"),
            ("authorization", "Bearer not-a-google-login-token"),
        ],
        &oauth_body,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // catalog-style Bearer api_key profile (separate mock)
    let (st, v) = call(
        "POST",
        "/mock/profiles/google-v3-bearer-api-key/translate",
        &[
            ("content-type", "application/json"),
            ("authorization", &format!("Bearer {}", config::BEARER_KEY)),
        ],
        &json!({"q":"hello","source":"en","target":"zh","text":"hello"}).to_string(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "translations.0.translatedText").is_some());

    // official v3 path requires auth
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
        &oauth_body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn azure_subscription_key_and_oauth_are_separate_mocks() {
    let body = json!([{ "text": "hello" }]).to_string();

    let (st, v) = call(
        "POST",
        "/mock/profiles/azure-translator-subscription-key/translate?api-version=3.0&to=zh",
        &[
            ("content-type", "application/json"),
            ("ocp-apim-subscription-key", config::AZURE_SUB_KEY),
        ],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "0.translations.0.text").is_some());

    let (st, v) = call(
        "POST",
        "/mock/profiles/azure-translator-oauth/translate?api-version=3.0&to=zh",
        &[
            ("content-type", "application/json"),
            (
                "authorization",
                &format!("Bearer {}", config::AZURE_ACCESS_TOKEN),
            ),
        ],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "0.translations.0.text").is_some());

    // cross-auth must fail: oauth profile + subscription key only
    let (st, _) = call(
        "POST",
        "/mock/profiles/azure-translator-oauth/translate?to=zh",
        &[
            ("content-type", "application/json"),
            ("ocp-apim-subscription-key", config::AZURE_SUB_KEY),
        ],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // official /translate accepts either
    let (st, _) = call(
        "POST",
        "/translate?api-version=3.0&to=zh",
        &[
            ("content-type", "application/json"),
            (
                "authorization",
                &format!("Bearer {}", config::AZURE_ACCESS_TOKEN),
            ),
        ],
        &body,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn azure_openai_api_key_header_vs_bearer_oauth() {
    let chat = json!({
        "messages": [
            {"role":"user","content":"hello"}
        ]
    })
    .to_string();

    let (st, v) = call(
        "POST",
        "/mock/profiles/azure-openai-api-key/chat/completions",
        &[
            ("content-type", "application/json"),
            ("api-key", config::BEARER_KEY),
        ],
        &chat,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(get_at_path(&v, "choices.0.message.content").is_some());

    let (st, v) = call(
        "POST",
        "/mock/profiles/azure-openai-oauth/chat/completions",
        &[
            ("content-type", "application/json"),
            (
                "authorization",
                &format!("Bearer {}", config::AZURE_ACCESS_TOKEN),
            ),
        ],
        &chat,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn every_registered_profile_accepts_valid_and_rejects_invalid_auth() {
    for profile in auth_profiles::all_profiles() {
        let (good_headers, good_query, good_body) = good_request(profile);
        let uri = if good_query.is_empty() {
            profile.profile_path.to_string()
        } else {
            format!("{}?{}", profile.profile_path, good_query)
        };
        let header_refs: Vec<(&str, &str)> = good_headers
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();
        let (st, v) = call("POST", &uri, &header_refs, &good_body).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "profile {} should succeed: {v}",
            profile.id
        );
        // output path present
        let got = get_at_path(&v, profile.translated_text_path).and_then(Value::as_str);
        assert!(
            got.is_some() && !got.unwrap().is_empty(),
            "profile {} missing {}: {v}",
            profile.id,
            profile.translated_text_path
        );

        // invalid auth
        let (bad_headers, bad_query, bad_body) = bad_auth_request(profile);
        let uri = if bad_query.is_empty() {
            profile.profile_path.to_string()
        } else {
            format!("{}?{}", profile.profile_path, bad_query)
        };
        let header_refs: Vec<(&str, &str)> = bad_headers
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();
        let (st, _) = call("POST", &uri, &header_refs, &bad_body).await;
        assert_eq!(
            st,
            StatusCode::UNAUTHORIZED,
            "profile {} should reject bad auth",
            profile.id
        );
    }
}

fn good_request(
    profile: &auth_profiles::MockAuthProfile,
) -> (Vec<(&'static str, String)>, String, String) {
    match profile.scheme {
        AuthScheme::GoogleApiKeyBody => (
            vec![("content-type", "application/json".into())],
            String::new(),
            json!({"q":"hello","target":"zh","key": config::GOOGLE_API_KEY}).to_string(),
        ),
        AuthScheme::GoogleApiKeyQuery => (
            vec![("content-type", "application/json".into())],
            format!("key={}", config::GOOGLE_API_KEY),
            json!({"q":"hello","target":"zh"}).to_string(),
        ),
        AuthScheme::GoogleApiKeyHeader => (
            vec![
                ("content-type", "application/json".into()),
                ("x-goog-api-key", config::GOOGLE_API_KEY.into()),
            ],
            String::new(),
            json!({"q":"hello","target":"zh"}).to_string(),
        ),
        AuthScheme::GoogleOAuthBearer => (
            vec![
                ("content-type", "application/json".into()),
                (
                    "authorization",
                    format!("Bearer {}", config::GOOGLE_ACCESS_TOKEN),
                ),
            ],
            String::new(),
            json!({"contents":["hello"],"targetLanguageCode":"zh"}).to_string(),
        ),
        AuthScheme::BearerApiKey if profile.id.starts_with("google-") => (
            vec![
                ("content-type", "application/json".into()),
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
            ],
            String::new(),
            json!({"q":"hello","target":"zh"}).to_string(),
        ),
        AuthScheme::BearerApiKey if profile.id.contains("openai") => (
            vec![
                ("content-type", "application/json".into()),
                (
                    "authorization",
                    if profile.id.contains("azure-openai-oauth") {
                        format!("Bearer {}", config::AZURE_ACCESS_TOKEN)
                    } else {
                        format!("Bearer {}", config::BEARER_KEY)
                    },
                ),
            ],
            String::new(),
            json!({"messages":[{"role":"user","content":"hello"}]}).to_string(),
        ),
        AuthScheme::BearerApiKey => (
            vec![
                ("content-type", "application/json".into()),
                ("authorization", format!("Bearer {}", config::BEARER_KEY)),
            ],
            String::new(),
            json!({"q":"hello","target":"zh"}).to_string(),
        ),
        AuthScheme::DeepLAuthKey => (
            vec![
                ("content-type", "application/json".into()),
                (
                    "authorization",
                    format!("DeepL-Auth-Key {}", config::DEEPL_AUTH_KEY),
                ),
            ],
            String::new(),
            json!({"text":["hello"],"target_lang":"ZH"}).to_string(),
        ),
        AuthScheme::AzureSubscriptionKey => (
            vec![
                ("content-type", "application/json".into()),
                ("ocp-apim-subscription-key", config::AZURE_SUB_KEY.into()),
            ],
            "api-version=3.0&to=zh".into(),
            json!([{"text":"hello"}]).to_string(),
        ),
        AuthScheme::AzureOAuthBearer => (
            vec![
                ("content-type", "application/json".into()),
                (
                    "authorization",
                    format!("Bearer {}", config::AZURE_ACCESS_TOKEN),
                ),
            ],
            "to=zh".into(),
            json!([{"text":"hello"}]).to_string(),
        ),
        AuthScheme::AzureOpenAiApiKey => (
            vec![
                ("content-type", "application/json".into()),
                ("api-key", config::BEARER_KEY.into()),
            ],
            String::new(),
            json!({"messages":[{"role":"user","content":"hello"}]}).to_string(),
        ),
        AuthScheme::PhraseToken => (
            vec![
                ("content-type", "application/json".into()),
                ("authorization", format!("token {}", config::BEARER_KEY)),
            ],
            String::new(),
            json!({"q":"hello","target":"zh"}).to_string(),
        ),
        AuthScheme::PapagoNaver => (
            vec![
                ("content-type", "application/x-www-form-urlencoded".into()),
                ("x-naver-client-id", config::PAPAGO_CLIENT_ID.into()),
                ("x-naver-client-secret", config::PAPAGO_CLIENT_SECRET.into()),
            ],
            String::new(),
            "source=en&target=zh&text=hello".into(),
        ),
        AuthScheme::YandexApiKey => (
            vec![("content-type", "application/x-www-form-urlencoded".into())],
            String::new(),
            format!(
                "text=hello&lang=en-zh&key={}",
                config::YANDEX_API_KEY
            ),
        ),
    }
}

fn bad_auth_request(
    profile: &auth_profiles::MockAuthProfile,
) -> (Vec<(&'static str, String)>, String, String) {
    let (mut headers, query, body) = good_request(profile);
    match profile.scheme {
        AuthScheme::GoogleApiKeyBody => {
            let body = json!({"q":"hello","target":"zh","key":"wrong"}).to_string();
            (headers, query, body)
        }
        AuthScheme::GoogleApiKeyQuery => (
            headers,
            "key=wrong".into(),
            json!({"q":"hello","target":"zh"}).to_string(),
        ),
        AuthScheme::GoogleApiKeyHeader => {
            headers.retain(|(k, _)| *k != "x-goog-api-key");
            headers.push(("x-goog-api-key", "wrong".into()));
            (headers, query, body)
        }
        AuthScheme::GoogleOAuthBearer
        | AuthScheme::BearerApiKey
        | AuthScheme::AzureOAuthBearer
        | AuthScheme::DeepLAuthKey
        | AuthScheme::PhraseToken => {
            headers.retain(|(k, _)| *k != "authorization");
            headers.push(("authorization", "Bearer totally-wrong".into()));
            (headers, query, body)
        }
        AuthScheme::AzureSubscriptionKey => {
            headers.retain(|(k, _)| *k != "ocp-apim-subscription-key");
            headers.push(("ocp-apim-subscription-key", "wrong".into()));
            (headers, query, body)
        }
        AuthScheme::AzureOpenAiApiKey => {
            headers.retain(|(k, _)| *k != "api-key");
            headers.push(("api-key", "wrong".into()));
            (headers, query, body)
        }
        AuthScheme::PapagoNaver => {
            headers.retain(|(k, _)| *k != "x-naver-client-id" && *k != "x-naver-client-secret");
            headers.push(("x-naver-client-id", "bad".into()));
            headers.push(("x-naver-client-secret", "bad".into()));
            (headers, query, body)
        }
        AuthScheme::YandexApiKey => (
            headers,
            query,
            "text=hello&lang=en-zh&key=wrong".into(),
        ),
    }
}
