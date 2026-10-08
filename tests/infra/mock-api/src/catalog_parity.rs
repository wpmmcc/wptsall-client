//! Catalog path parity for http_mt vendors that were missing or weakly mocked.
//!
//! Response shapes follow `translated_text_path` from the builtin-3 provider catalog
//! so Lab/e2e can parse mock replies exactly like real vendor APIs.

use std::collections::HashMap;

use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::handlers::{auth_error_response, key_enter, key_exit, translate_text};
use crate::verify;
use crate::AppState;

/// Try to handle a catalog-shaped official path. Returns `None` if not a parity route.
pub async fn try_handle(
    state: &AppState,
    path: &str,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Option<Response> {
    match path {
        "/api/v1.5/tr.json/translate" => Some(handle_yandex_v15(state, headers, query, body).await),
        "/v1/papago/n2mt" => Some(handle_papago_n2mt(state, headers, body).await),
        "/api/v1/document/translate" => {
            Some(handle_bearer_path(state, headers, body, "smartcat", "translatedText").await)
        }
        "/v2/projects/mt/translate" => Some(handle_phrase(state, headers, body).await),
        "/api/v2/translations" => Some(handle_linguee(state, headers, query, body).await),
        "/index.php" => {
            Some(handle_bearer_path(state, headers, body, "iciba", "content.out").await)
        }
        "/api/transweb/translate" => {
            Some(handle_bearer_path(state, headers, body, "sogou", "data.translatedText").await)
        }
        "/api/v2/machines/translations" => {
            Some(handle_bearer_path(state, headers, body, "crowdin-mt", "data.0.text").await)
        }
        "/api2/projects/mt/translate" => {
            Some(handle_bearer_path(state, headers, body, "lokalise-mt", "translation").await)
        }
        // Matecat shares DeepL's /v2/translate path; caller must branch on auth scheme.
        _ => None,
    }
}

/// Matecat / Translated: Bearer on `/v2/translate` → `{ "translation": "..." }`.
pub async fn handle_matecat(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    handle_bearer_path(state, headers, body, "matecat", "translation").await
}

async fn handle_yandex_v15(
    state: &AppState,
    _headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Response {
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
    let stat_key = format!("official:yandex-v15:{key}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_yandex_api_key(&key) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let text = form
        .get("text")
        .cloned()
        .or_else(|| query.get("text").cloned())
        .or_else(|| {
            json_body
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let lang = form
        .get("lang")
        .cloned()
        .or_else(|| query.get("lang").cloned())
        .or_else(|| {
            json_body
                .get("lang")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "en-zh".into());
    let target = lang.split('-').nth(1).unwrap_or("en");
    let translated = translate_text(&text, target);
    key_exit(state, &stat_key).await;
    // Catalog path: text.0
    json_ok(json!({ "code": 200, "lang": lang, "text": [translated] }))
}

async fn handle_papago_n2mt(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let client_id = header_value(headers, "x-naver-client-id").unwrap_or_default();
    let client_secret = header_value(headers, "x-naver-client-secret").unwrap_or_default();
    let stat_key = format!("official:papago-n2mt:{client_id}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_papago(&client_id, &client_secret) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let form = parse_form(body);
    let text = form.get("text").map(String::as_str).unwrap_or_default();
    let target = form.get("target").map(String::as_str).unwrap_or("en");
    let translated = translate_text(text, target);
    key_exit(state, &stat_key).await;
    json_ok(json!({
        "message": {
            "result": {
                "translatedText": translated,
                "srcLangType": form.get("source").cloned().unwrap_or_else(|| "auto".into()),
                "tarLangType": target
            }
        }
    }))
}

async fn handle_phrase(state: &AppState, headers: &HeaderMap, body: &str) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:phrase:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_phrase_token(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let (text, target) = extract_text_target(body);
    let response = json_ok(set_at_path(Value::Object(Default::default()), "translation", &translate_text(&text, &target)));
    key_exit(state, &stat_key).await;
    response
}

async fn handle_linguee(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
    body: &str,
) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:linguee:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let text = query
        .get("q")
        .cloned()
        .or_else(|| {
            let v = parse_json(body);
            v.get("q")
                .or_else(|| v.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let target = query
        .get("target")
        .or_else(|| query.get("dest"))
        .map(String::as_str)
        .unwrap_or("en");
    let translated = translate_text(&text, target);
    // Catalog path: 0.0 → [[translated]]
    key_exit(state, &stat_key).await;
    json_ok(json!([[translated]]))
}

async fn handle_bearer_path(
    state: &AppState,
    headers: &HeaderMap,
    body: &str,
    family: &str,
    translated_text_path: &str,
) -> Response {
    let authorization = header_value(headers, "authorization").unwrap_or_default();
    let stat_key = format!("official:{family}:{authorization}");
    let _ = key_enter(state, &stat_key).await;
    if let Err(error) = verify::verify_bearer_api_key(&authorization) {
        key_exit(state, &stat_key).await;
        return auth_error_response(error);
    }
    let (text, target) = extract_text_target(body);
    let translated = translate_text(&text, &target);
    let payload = set_at_path(Value::Object(Default::default()), translated_text_path, &translated);
    key_exit(state, &stat_key).await;
    json_ok(payload)
}

fn extract_text_target(body: &str) -> (String, String) {
    let value = parse_json(body);
    let text = value
        .get("text")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else {
                v.as_array()
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }
        })
        .or_else(|| value.get("q").and_then(Value::as_str).map(str::to_string))
        .or_else(|| value.get("input").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let target = value
        .get("target")
        .or_else(|| value.get("target_lang"))
        .or_else(|| value.get("targetLanguage"))
        .or_else(|| value.get("to"))
        .and_then(Value::as_str)
        .unwrap_or("en")
        .to_string();
    (text, target)
}

/// Build nested JSON so `get_at_path(result, path) == translated`.
pub fn set_at_path(mut root: Value, path: &str, translated: &str) -> Value {
    if path.is_empty() {
        return Value::String(translated.to_string());
    }
    let segments: Vec<&str> = path.split('.').collect();
    ensure_path(&mut root, &segments, translated);
    root
}

fn ensure_path(node: &mut Value, segments: &[&str], leaf: &str) {
    if segments.is_empty() {
        *node = Value::String(leaf.to_string());
        return;
    }
    let head = segments[0];
    let rest = &segments[1..];
    if let Ok(index) = head.parse::<usize>() {
        if !node.is_array() {
            *node = Value::Array(vec![]);
        }
        let arr = node.as_array_mut().expect("array");
        while arr.len() <= index {
            arr.push(if rest.is_empty() {
                Value::Null
            } else if rest[0].parse::<usize>().is_ok() {
                Value::Array(vec![])
            } else {
                Value::Object(Default::default())
            });
        }
        ensure_path(&mut arr[index], rest, leaf);
        return;
    }
    if !node.is_object() {
        *node = Value::Object(Default::default());
    }
    let obj = node.as_object_mut().expect("object");
    let entry = obj.entry(head.to_string()).or_insert_with(|| {
        if rest.is_empty() {
            Value::Null
        } else if rest[0].parse::<usize>().is_ok() {
            Value::Array(vec![])
        } else {
            Value::Object(Default::default())
        }
    });
    ensure_path(entry, rest, leaf);
}

/// Read a dotted path (supports numeric array indexes).
pub fn get_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = value;
    for seg in path.split('.') {
        if let Ok(index) = seg.parse::<usize>() {
            cur = cur.as_array()?.get(index)?;
        } else {
            cur = cur.get(seg)?;
        }
    }
    Some(cur)
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

fn parse_form(body: &str) -> HashMap<String, String> {
    form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn parse_json(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| json!({}))
}

fn json_ok(value: Value) -> Response {
    Json(value).into_response()
}

/// Paths covered by this module (for e2e inventory).
pub fn parity_paths() -> &'static [&'static str] {
    &[
        "/api/v1.5/tr.json/translate",
        "/v1/papago/n2mt",
        "/api/v1/document/translate",
        "/v2/projects/mt/translate",
        "/api/v2/translations",
        "/index.php",
        "/api/transweb/translate",
        "/api/v2/machines/translations",
        "/api2/projects/mt/translate",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_nested_object_path() {
        let v = set_at_path(json!({}), "data.translatedText", "你好");
        assert_eq!(get_at_path(&v, "data.translatedText").and_then(Value::as_str), Some("你好"));
    }

    #[test]
    fn set_and_get_array_index_path() {
        let v = set_at_path(json!({}), "data.0.text", "hola");
        assert_eq!(get_at_path(&v, "data.0.text").and_then(Value::as_str), Some("hola"));
    }

    #[test]
    fn set_linguee_0_0() {
        let v = set_at_path(json!([]), "0.0", "bonjour");
        assert_eq!(get_at_path(&v, "0.0").and_then(Value::as_str), Some("bonjour"));
    }

    #[test]
    fn set_content_out() {
        let v = set_at_path(json!({}), "content.out", "out");
        assert_eq!(get_at_path(&v, "content.out").and_then(Value::as_str), Some("out"));
    }

    #[test]
    fn parity_paths_cover_former_e_gaps() {
        let paths = parity_paths();
        assert!(paths.contains(&"/v1/papago/n2mt"));
        assert!(paths.contains(&"/api/v1.5/tr.json/translate"));
        assert!(paths.contains(&"/api/v1/document/translate"));
        assert_eq!(paths.len(), 9);
    }

    #[test]
    fn mock_credentials_are_stable() {
        assert!(!crate::config::PAPAGO_CLIENT_ID.is_empty());
        assert!(!crate::config::YANDEX_API_KEY.is_empty());
        assert!(!crate::config::BEARER_KEY.is_empty());
    }
}
