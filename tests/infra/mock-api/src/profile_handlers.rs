//! Dispatch `/mock/profiles/{id}/…` — one path per auth-mode profile.
//!
//! Official vendor paths remain available; profile paths give WP Lab a
//! collision-free target when a vendor has multiple login methods.

use std::collections::HashMap;

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::auth_profiles::{
    self, header_value, reject_if_too_long, AuthScheme, MockAuthProfile,
};
use crate::config;
use crate::handlers::{auth_error_response, key_enter, key_exit, translate_text};
use crate::verify;
use crate::AppState;

pub async fn list_profiles() -> Response {
    Json(auth_profiles::profiles_inventory_json()).into_response()
}

/// Handle `POST /mock/profiles/{id}/…`.
pub async fn try_handle_profile(
    state: &AppState,
    path: &str,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Option<Response> {
    let Some(rest) = path.strip_prefix("/mock/profiles/") else {
        return None;
    };
    let (id, _suffix) = rest.split_once('/').unwrap_or((rest, ""));
    let Some(profile) = auth_profiles::profile_by_id(id) else {
        return Some(
            (
                axum::http::StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "unknown_mock_profile",
                    "id": id,
                    "hint": "GET /api/v1/mock-auth-profiles"
                })),
            )
                .into_response(),
        );
    };
    Some(dispatch_profile(state, profile, headers, query, body).await)
}

async fn dispatch_profile(
    state: &AppState,
    profile: &MockAuthProfile,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Response {
    let stat_key = format!("profile:{}:{}", profile.id, profile.auth_mode);
    let _ = key_enter(state, &stat_key).await;

    if let Err(error) = verify_profile_auth(profile, headers, query, body) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }

    let (text, target) = extract_io(profile, query, body);
    if let Some(resp) = reject_if_too_long(&text, profile.max_input_chars) {
        key_exit(state, &stat_key).await;
        return resp;
    }

    let translated = translate_text(&text, &target);
    let payload = build_response(profile, &translated);
    key_exit(state, &stat_key).await;
    Json(payload).into_response()
}

fn verify_profile_auth(
    profile: &MockAuthProfile,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Result<(), verify::AuthError> {
    match profile.scheme {
        AuthScheme::GoogleApiKeyBody => {
            let value = parse_json(body);
            let key = value.get("key").and_then(Value::as_str).unwrap_or_default();
            // Catalog template carries the key in the x-goog-api-key header
            // (official-spec fix 65f2a34); accept that channel too so the
            // body-style profile stays meaningful for header templates.
            let header_key = header_value(headers, "x-goog-api-key").unwrap_or_default();
            let key = if key.is_empty() { header_key.as_str() } else { key };
            verify::verify_google_api_key(key)
        }
        AuthScheme::GoogleApiKeyQuery => {
            // Prefer query `?key=`; also accept catalog body `key` and the
            // x-goog-api-key header so any official key channel passes when
            // the Lab overrides request.url to this profile path.
            let from_body = parse_json(body);
            let key = query
                .get("key")
                .map(String::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| from_body.get("key").and_then(Value::as_str))
                .unwrap_or("");
            let header_key = header_value(headers, "x-goog-api-key").unwrap_or_default();
            let key = if key.is_empty() { header_key.as_str() } else { key };
            verify::verify_google_api_key(key)
        }
        AuthScheme::GoogleApiKeyHeader => {
            let header = header_value(headers, "x-goog-api-key").unwrap_or_default();
            let key = if !header.is_empty() {
                header
            } else {
                parse_json(body)
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            verify::verify_google_api_key(&key)
        }
        AuthScheme::GoogleOAuthBearer => {
            let authorization = header_value(headers, "authorization").unwrap_or_default();
            verify::verify_google_access_token(&authorization)
        }
        AuthScheme::BearerApiKey => {
            let authorization = header_value(headers, "authorization").unwrap_or_default();
            // google-v3-bearer-api-key accepts BEARER_KEY; azure-openai-oauth also accepts AZURE token
            if profile.id == "azure-openai-oauth" {
                if verify::verify_azure_access_token(&authorization).is_ok() {
                    return Ok(());
                }
                // Catalog Azure OpenAI template sends `api-key` header, not Bearer.
                let api_key = header_value(headers, "api-key").unwrap_or_default();
                if api_key == config::BEARER_KEY || api_key == config::AZURE_ACCESS_TOKEN {
                    return Ok(());
                }
            }
            if !authorization.is_empty() {
                return verify::verify_bearer_api_key(&authorization);
            }
            // Fallback: some openai_compatible templates only set api-key.
            let api_key = header_value(headers, "api-key").unwrap_or_default();
            if api_key == config::BEARER_KEY {
                return Ok(());
            }
            verify::verify_bearer_api_key(&authorization)
        }
        AuthScheme::DeepLAuthKey => {
            let authorization = header_value(headers, "authorization").unwrap_or_default();
            verify::verify_deepl_auth_key(&authorization)
        }
        AuthScheme::AzureSubscriptionKey => {
            let key = header_value(headers, "ocp-apim-subscription-key").unwrap_or_default();
            verify::verify_azure(&key)
        }
        AuthScheme::AzureOAuthBearer => {
            let authorization = header_value(headers, "authorization").unwrap_or_default();
            verify::verify_azure_access_token(&authorization)
        }
        AuthScheme::AzureOpenAiApiKey => {
            let key = header_value(headers, "api-key").unwrap_or_default();
            if key == config::BEARER_KEY {
                Ok(())
            } else {
                Err(verify::AuthError::new(
                    "azure_openai",
                    format!("Invalid api-key header: expected {}", config::BEARER_KEY),
                ))
            }
        }
        AuthScheme::PhraseToken => {
            let authorization = header_value(headers, "authorization").unwrap_or_default();
            verify::verify_phrase_token(&authorization)
        }
        AuthScheme::PapagoNaver => {
            let id = header_value(headers, "x-naver-client-id").unwrap_or_default();
            let secret = header_value(headers, "x-naver-client-secret").unwrap_or_default();
            verify::verify_papago(&id, &secret)
        }
        AuthScheme::YandexApiKey => {
            let form = parse_form(body);
            let json_body = parse_json(body);
            let key = form
                .get("key")
                .cloned()
                .or_else(|| query.get("key").cloned())
                .or_else(|| {
                    json_body
                        .get("key")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            verify::verify_yandex_api_key(&key)
        }
    }
}

fn extract_io(
    profile: &MockAuthProfile,
    query: &HashMap<String, String>,
    body: &str,
) -> (String, String) {
    match profile.scheme {
        AuthScheme::PapagoNaver | AuthScheme::YandexApiKey => {
            let form = parse_form(body);
            let text = form
                .get("text")
                .cloned()
                .or_else(|| query.get("text").cloned())
                .or_else(|| query.get("q").cloned())
                .unwrap_or_default();
            let target = form
                .get("target")
                .cloned()
                .or_else(|| {
                    form.get("lang")
                        .and_then(|l| l.split('-').nth(1).map(str::to_string))
                })
                .unwrap_or_else(|| "zh".into());
            (text, target)
        }
        AuthScheme::AzureSubscriptionKey | AuthScheme::AzureOAuthBearer => {
            let value = parse_json(body);
            let text = value
                .as_array()
                .and_then(|a| a.first())
                .and_then(|item| {
                    item.get("text")
                        .or_else(|| item.get("Text"))
                        .and_then(Value::as_str)
                })
                .unwrap_or_default()
                .to_string();
            let target = query.get("to").cloned().unwrap_or_else(|| "zh".into());
            (text, target)
        }
        AuthScheme::DeepLAuthKey => {
            let value = parse_json(body);
            let text = value
                .get("text")
                .and_then(|v| {
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .or_else(|| v.as_str())
                })
                .unwrap_or_default()
                .to_string();
            let target = value
                .get("target_lang")
                .and_then(Value::as_str)
                .unwrap_or("ZH")
                .to_string();
            (text, target)
        }
        AuthScheme::GoogleOAuthBearer => {
            let value = parse_json(body);
            let text = value
                .get("contents")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .or_else(|| value.get("q").and_then(Value::as_str))
                .or_else(|| value.get("text").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            let target = value
                .get("targetLanguageCode")
                .or_else(|| value.get("target"))
                .and_then(Value::as_str)
                .unwrap_or("zh")
                .to_string();
            (text, target)
        }
        AuthScheme::BearerApiKey
            if profile.id.starts_with("openai") || profile.id.starts_with("azure-openai") =>
        {
            let value = parse_json(body);
            let text = value
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|msgs| {
                    msgs.iter().rev().find_map(|m| {
                        if m.get("role").and_then(Value::as_str) == Some("user") {
                            m.get("content").and_then(Value::as_str)
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or("hello")
                .to_string();
            (text, "zh".into())
        }
        AuthScheme::AzureOpenAiApiKey => {
            let value = parse_json(body);
            let text = value
                .get("messages")
                .and_then(Value::as_array)
                .and_then(|msgs| {
                    msgs.iter().rev().find_map(|m| m.get("content").and_then(Value::as_str))
                })
                .unwrap_or("hello")
                .to_string();
            (text, "zh".into())
        }
        _ => {
            let value = parse_json(body);
            let text = value
                .get("q")
                .and_then(|v| {
                    v.as_array()
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .or_else(|| v.as_str())
                })
                .or_else(|| value.get("text").and_then(Value::as_str))
                .or_else(|| value.get("contents").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            let target = value
                .get("target")
                .or_else(|| value.get("target_lang"))
                .or_else(|| value.get("targetLanguageCode"))
                .and_then(Value::as_str)
                .unwrap_or("zh")
                .to_string();
            (text, target)
        }
    }
}

fn build_response(profile: &MockAuthProfile, translated: &str) -> Value {
    match profile.scheme {
        AuthScheme::GoogleApiKeyBody
        | AuthScheme::GoogleApiKeyQuery
        | AuthScheme::GoogleApiKeyHeader => {
            json!({ "data": { "translations": [{ "translatedText": translated }] } })
        }
        AuthScheme::GoogleOAuthBearer | AuthScheme::BearerApiKey
            if profile.id.starts_with("google-") =>
        {
            json!({ "translations": [{ "translatedText": translated }] })
        }
        AuthScheme::AzureSubscriptionKey | AuthScheme::AzureOAuthBearer => {
            json!([{
                "translations": [{ "text": translated, "Text": translated }],
                "Translations": [{ "Text": translated, "text": translated }]
            }])
        }
        AuthScheme::DeepLAuthKey => {
            json!({ "translations": [{ "text": translated }] })
        }
        AuthScheme::BearerApiKey
            if profile.id.starts_with("openai") || profile.id.contains("openai") =>
        {
            json!({
                "id": "chatcmpl-mock",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": translated },
                    "finish_reason": "stop"
                }]
            })
        }
        AuthScheme::AzureOpenAiApiKey => {
            json!({
                "id": "chatcmpl-azure-mock",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": translated },
                    "finish_reason": "stop"
                }]
            })
        }
        AuthScheme::PapagoNaver => {
            json!({ "message": { "result": { "translatedText": translated } } })
        }
        AuthScheme::YandexApiKey => {
            json!({ "code": 200, "text": [translated] })
        }
        AuthScheme::PhraseToken => {
            json!({ "translation": translated })
        }
        _ => json!({ "translated_text": translated }),
    }
}

fn parse_json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| json!({}))
}

fn parse_form(body: &str) -> HashMap<String, String> {
    form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
