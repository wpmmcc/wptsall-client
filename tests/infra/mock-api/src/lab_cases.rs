//! Lab provider cases — one UI/API test case per catalog entry (+ auth profiles).
//!
//! Source of truth for e2e UI fills: credentials, mock URL, response path, notes.
//! Clients must **not** hardcode vendor keys; fetch this inventory from mock-api.

use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::auth_profiles;
use crate::config;

const FIXTURE: &str = include_str!("../fixtures/catalog-builtin3-112.json");

fn mock_base() -> String {
    std::env::var("MOCK_PUBLIC_BASE")
        .unwrap_or_else(|_| "http://127.0.0.1:9090".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn mint_oauth_access_token() -> String {
    crate::verify::verify_oauth_client_credentials(
        config::OAUTH_CLIENT_ID,
        config::OAUTH_CLIENT_SECRET,
    )
    .unwrap_or_else(|_| "invalid-oauth-token".to_string())
}

fn mint_lab_jwt_token() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let claims = json!({
        "iss": "test",
        "sub": "lab-provider",
        "aud": "test",
        "iat": now,
        "exp": now + 3600,
    });
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(config::JWT_RSA_PRIVATE_KEY_PEM.as_bytes())
        .expect("JWT_RSA_PRIVATE_KEY_PEM must parse");
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    jsonwebtoken::encode(&header, &claims, &key).unwrap_or_else(|_| "invalid-jwt".to_string())
}

fn auth_values_for(entry_id: &str, fields: &[String]) -> Value {
    let mut map = serde_json::Map::new();
    let preset: &[(&str, &[(&str, &str)])] = &[
        ("baidu", &[("app_id", config::BAIDU_APPID), ("api_key", config::BAIDU_SECRET)]),
        (
            "youdao",
            &[
                ("app_key", config::YOUDAO_APP_KEY),
                ("app_secret", config::YOUDAO_APP_SECRET),
            ],
        ),
        (
            "hmac-generic",
            &[
                ("api_key", config::HMAC_API_KEY),
                ("secret_key", config::HMAC_SECRET),
            ],
        ),
        (
            "tencent",
            &[
                ("secret_id", config::TC3_SECRET_ID),
                ("secret_key", config::TC3_SECRET_KEY),
            ],
        ),
        (
            "amazon-translate",
            &[
                ("access_key", config::AWS_ACCESS_KEY),
                ("secret_key", config::AWS_SECRET_KEY),
            ],
        ),
        (
            "volcengine",
            &[
                ("access_key", config::VOLC_ACCESS_KEY),
                ("secret_key", config::VOLC_SECRET_KEY),
            ],
        ),
        (
            "alibaba",
            &[
                ("access_key", config::ALI_ACCESS_KEY),
                ("secret_key", config::ALI_SECRET_KEY),
            ],
        ),
        (
            "iflytek",
            &[
                ("app_id", config::IFLYTEK_APP_ID),
                ("api_secret", config::IFLYTEK_API_SECRET),
            ],
        ),
        ("niutrans", &[("api_key", config::NIUTRANS_API_KEY)]),
        ("kakao", &[("api_key", config::KAKAO_KEY)]),
        (
            "azure-cognitive",
            &[("subscription_key", config::AZURE_SUB_KEY)],
        ),
        (
            "microsoft-translator",
            &[("api_key", config::AZURE_SUB_KEY)],
        ),
        ("microsoft-custom", &[("api_key", config::AZURE_SUB_KEY)]),
        ("deepl", &[("api_key", config::DEEPL_AUTH_KEY)]),
        ("deepl-pro", &[("api_key", config::DEEPL_AUTH_KEY)]),
        ("google-translate", &[("api_key", config::GOOGLE_API_KEY)]),
        (
            "google-advanced",
            &[("api_key", config::GOOGLE_ACCESS_TOKEN)],
        ),
        (
            "papago",
            &[
                ("client_id", config::PAPAGO_CLIENT_ID),
                ("client_secret", config::PAPAGO_CLIENT_SECRET),
            ],
        ),
        ("yandex", &[("api_key", config::YANDEX_API_KEY)]),
        (
            "ibm-watson",
            &[
                ("api_key", config::BEARER_KEY),
                ("instance_id", "mock-instance"),
            ],
        ),
        ("azure-openai", &[("api_key", config::BEARER_KEY)]),
        ("phrase", &[("api_key", config::BEARER_KEY)]),
    ];
    // OAuth translate uses Bearer {{auth.access_token}} — mint a real HS256 token.
    if entry_id == "oauth-mt" {
        map.insert(
            "client_id".to_string(),
            Value::String(config::OAUTH_CLIENT_ID.to_string()),
        );
        map.insert(
            "client_secret".to_string(),
            Value::String(config::OAUTH_CLIENT_SECRET.to_string()),
        );
        map.insert(
            "access_token".to_string(),
            Value::String(mint_oauth_access_token()),
        );
        return Value::Object(map);
    }
    // JWT translate uses Bearer {{auth.jwt_token}} — mint a real RS256 token.
    if entry_id == "jwt-mt" {
        map.insert("jwt_token".to_string(), Value::String(mint_lab_jwt_token()));
        return Value::Object(map);
    }
    if let Some((_, pairs)) = preset.iter().find(|(id, _)| *id == entry_id) {
        for (k, v) in *pairs {
            map.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        return Value::Object(map);
    }
    for f in fields {
        if f == "jwt_token" {
            map.insert(f.clone(), Value::String(mint_lab_jwt_token()));
            continue;
        }
        let v = match f.as_str() {
            "api_key" | "subscription_key" => config::BEARER_KEY,
            "client_id" => config::OAUTH_CLIENT_ID,
            "client_secret" => config::OAUTH_CLIENT_SECRET,
            "access_token" => config::GOOGLE_ACCESS_TOKEN,
            "instance_id" => "mock-instance",
            "app_id" => config::BAIDU_APPID,
            "app_key" => config::YOUDAO_APP_KEY,
            "app_secret" => config::YOUDAO_APP_SECRET,
            "secret_id" => config::TC3_SECRET_ID,
            "secret_key" => config::TC3_SECRET_KEY,
            "access_key" => config::AWS_ACCESS_KEY,
            "api_secret" => config::IFLYTEK_API_SECRET,
            _ => config::BEARER_KEY,
        };
        map.insert(f.clone(), Value::String(v.to_string()));
    }
    // openai_compatible + most http_mt with only api_key
    if map.is_empty() {
        map.insert(
            "api_key".to_string(),
            Value::String(config::BEARER_KEY.to_string()),
        );
    }
    Value::Object(map)
}

fn special_mock_path(entry_id: &str) -> Option<&'static str> {
    match entry_id {
        "azure-openai" => Some("/mock/profiles/azure-openai-api-key/chat/completions"),
        "mymemory" => Some("/get?q=Hello%20world&langpair=en%7Czh"),
        "apertium" => Some("/apy/translate?q=Hello%20world&langpair=en%7Czh"),
        "yandex" => Some("/api/v1.5/tr.json/translate"),
        _ => None,
    }
}

fn build_case(entry: &Value, base: &str) -> Value {
    let entry_id = entry.get("entry_id").and_then(Value::as_str).unwrap_or("");
    let vendor_id = entry.get("vendor_id").and_then(Value::as_str).unwrap_or("");
    let name = entry.get("name").and_then(Value::as_str).unwrap_or(entry_id);
    let family = entry.get("family").and_then(Value::as_str).unwrap_or("");
    let fields: Vec<String> = entry
        .get("auth_fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_else(|| vec!["api_key".to_string()]);
    let mock_path = special_mock_path(entry_id)
        .map(str::to_string)
        .or_else(|| {
            entry
                .get("mock_path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "/v1/chat/completions".to_string());
    let mock_query = entry
        .get("mock_query")
        .and_then(Value::as_str)
        .unwrap_or("");
    let request_url = if mock_path.contains('?') || mock_query.is_empty() {
        format!("{base}{mock_path}")
    } else {
        format!("{base}{mock_path}?{mock_query}")
    };
    let response_path = entry
        .get("translated_text_path")
        .and_then(Value::as_str)
        .unwrap_or("translated_text");
    let sign_algo = entry.get("sign_algo").cloned().unwrap_or(Value::Null);
    let auth_values = auth_values_for(entry_id, &fields);
    let notes = format!(
        "family={family}; sign={}; fill auth_fields in order; override request.url to mock; response path={}",
        sign_algo.as_str().unwrap_or("-"),
        response_path
    );
    let (source_lang, target_lang) = if entry_id.contains("deepl") {
        ("EN", "ZH")
    } else {
        ("en", "zh")
    };

    json!({
        "case_id": format!("catalog:{entry_id}"),
        "entry_id": entry_id,
        "vendor_id": vendor_id,
        "name": name,
        "family": family,
        "confirm_phase": entry.get("confirm_phase"),
        "evidence_tier": entry.get("evidence_tier"),
        "method": entry.get("method").and_then(Value::as_str).unwrap_or("POST"),
        "official_url": entry.get("url"),
        "auth_fields": fields,
        "auth_values": auth_values,
        "request_url": request_url,
        "response_translated_text_path": response_path,
        "sign_algo": sign_algo,
        "source_lang": source_lang,
        "target_lang": target_lang,
        "ui": {
            "search": entry_id,
            "display_name": name,
            "fill_auth_field_order": fields,
            "request_url_testid": "wizard-request-url",
            "primary_auth_testid": "wizard-api-key",
            "source_lang": source_lang,
            "target_lang": target_lang
        },
        "notes": notes,
        "source": "fixtures/catalog-builtin3-112.json + mock config credentials"
    })
}

/// Full Lab inventory for UI / API e2e drivers.
pub fn lab_provider_cases_json() -> Value {
    let base = mock_base();
    let doc: Value = serde_json::from_str(FIXTURE).unwrap_or(json!({}));
    let entries = doc
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut cases: Vec<Value> = entries.iter().map(|e| build_case(e, &base)).collect();

    // Expand multi-auth mock profiles as additional cases (same entry_id, different URL/creds).
    for profile in auth_profiles::all_profiles() {
        for entry_id in profile.catalog_entry_ids {
            let Some(base_entry) = entries.iter().find(|e| {
                e.get("entry_id").and_then(Value::as_str) == Some(*entry_id)
            }) else {
                continue;
            };
            let mut case = build_case(base_entry, &base);
            case["case_id"] = json!(format!("auth-profile:{}:{}", profile.id, entry_id));
            case["auth_profile_id"] = json!(profile.id);
            case["auth_mode"] = json!(profile.auth_mode);
            case["request_url"] = json!(format!("{base}{}", profile.profile_path));
            case["response_translated_text_path"] = json!(profile.translated_text_path);
            case["credential_hint"] = json!(profile.credential_hint);
            case["notes"] = json!(format!(
                "auth-profile {}; scheme={:?}; {}",
                profile.id, profile.scheme, profile.credential_hint
            ));
            // Overlay profile credentials onto auth_values when field names match.
            if let Some(obj) = case.get_mut("auth_values").and_then(Value::as_object_mut) {
                let creds = auth_profiles::profiles_inventory_json();
                if let Some(list) = creds.get("profiles").and_then(Value::as_array) {
                    if let Some(p) = list.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(profile.id)) {
                        if let Some(c) = p.get("credentials").and_then(Value::as_object) {
                            for (k, v) in c {
                                if obj.contains_key(k) {
                                    obj.insert(k.clone(), v.clone());
                                } else if k == "access_token" && obj.contains_key("api_key") {
                                    obj.insert("api_key".into(), v.clone());
                                } else if k == "subscription_key" && obj.contains_key("api_key") {
                                    obj.insert("api_key".into(), v.clone());
                                } else if k == "api_key" {
                                    obj.insert("api_key".into(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
            cases.push(case);
        }
    }

    json!({
        "count": cases.len(),
        "catalog_entries": entries.len(),
        "mock_base": base,
        "policy": "UI/API e2e must load cases from this endpoint — do not hardcode vendor keys/URLs in scripts",
        "client_url_policy": "Client allows any http(s) provider URL (except cloud metadata hosts)",
        "cases": cases
    })
}

pub async fn list_lab_provider_cases() -> Response {
    Json(lab_provider_cases_json()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_cases_cover_all_112_catalog_entries() {
        let inv = lab_provider_cases_json();
        assert_eq!(inv["catalog_entries"], 112);
        let cases = inv["cases"].as_array().unwrap();
        let catalog: Vec<_> = cases
            .iter()
            .filter(|c| {
                c.get("case_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .starts_with("catalog:")
            })
            .collect();
        assert_eq!(catalog.len(), 112);
        assert!(cases.len() >= 112);
        let deepl = catalog
            .iter()
            .find(|c| c["entry_id"] == "deepl")
            .unwrap();
        assert!(deepl["auth_values"]["api_key"]
            .as_str()
            .unwrap()
            .contains("deepl"));
        assert!(deepl["request_url"]
            .as_str()
            .unwrap()
            .contains("/v2/translate"));
    }
}
