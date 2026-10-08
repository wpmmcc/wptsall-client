//! Auth-mode mock profiles — one profile ≈ one login/授权方式 for WP Lab mapping.
//!
//! Real vendors often expose multiple auth schemes (API key vs OAuth access token).
//! We **split them into distinct mock profiles** so client catalog `entry_id` + auth
//! fields map 1:1 to a mock target (path + credentials + I/O contract + length limit).

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config;

/// How credentials are carried on the wire (mirrors real vendor APIs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScheme {
    /// Google Translate v2 API key in JSON body `key`.
    GoogleApiKeyBody,
    /// Google Translate v2 API key in query `?key=`.
    GoogleApiKeyQuery,
    /// Google Cloud header `x-goog-api-key`.
    GoogleApiKeyHeader,
    /// Google Cloud Translation Advanced / OAuth: `Authorization: Bearer <access_token>`.
    GoogleOAuthBearer,
    /// OpenAI-compatible / generic: `Authorization: Bearer <api_key>`.
    BearerApiKey,
    /// DeepL: `Authorization: DeepL-Auth-Key <key>`.
    DeepLAuthKey,
    /// Microsoft / Azure Translator: `Ocp-Apim-Subscription-Key`.
    AzureSubscriptionKey,
    /// Azure Entra / OAuth on Translator: `Authorization: Bearer <access_token>`.
    AzureOAuthBearer,
    /// Azure OpenAI: header `api-key`.
    AzureOpenAiApiKey,
    /// Phrase: `Authorization: token <key>`.
    PhraseToken,
    /// Naver Papago client id/secret headers.
    PapagoNaver,
    /// Yandex form/query `key`.
    YandexApiKey,
}

/// One mockable auth configuration (maps to catalog entries for WP plugin tests).
#[derive(Debug, Clone, Copy)]
pub struct MockAuthProfile {
    /// Stable id used in `/mock/profiles/{id}/…` and Lab overrides.
    pub id: &'static str,
    /// Builtin catalog `entry_id`s that should use this profile.
    pub catalog_entry_ids: &'static [&'static str],
    /// Official-style mode label: `key` | `oauth`.
    pub auth_mode: &'static str,
    /// Canonical official path (rewrite host → mock).
    pub official_path: &'static str,
    /// Dedicated mock profile path (always on mock host; avoids dual-auth collisions).
    pub profile_path: &'static str,
    pub max_input_chars: usize,
    pub input_fields: &'static [&'static str],
    pub translated_text_path: &'static str,
    pub scheme: AuthScheme,
    /// Credential constant name / hint for Lab UI docs.
    pub credential_hint: &'static str,
}

/// Full registry — multi-auth vendors appear as **multiple rows**.
pub fn all_profiles() -> &'static [MockAuthProfile] {
    &PROFILES
}

pub fn profile_by_id(id: &str) -> Option<&'static MockAuthProfile> {
    all_profiles().iter().find(|p| p.id == id)
}

pub fn profiles_for_catalog_entry(entry_id: &str) -> Vec<&'static MockAuthProfile> {
    all_profiles()
        .iter()
        .filter(|p| p.catalog_entry_ids.contains(&entry_id))
        .collect()
}

const PROFILES: &[MockAuthProfile] = &[
    // --- Google: API key (v2) vs OAuth access token (v3 Advanced) ---
    MockAuthProfile {
        id: "google-v2-api-key-body",
        catalog_entry_ids: &["google-translate"],
        auth_mode: "key",
        official_path: "/language/translate/v2",
        profile_path: "/mock/profiles/google-v2-api-key-body/translate",
        max_input_chars: 30_000,
        input_fields: &["q", "source", "target", "key"],
        translated_text_path: "data.translations.0.translatedText",
        scheme: AuthScheme::GoogleApiKeyBody,
        credential_hint: "GOOGLE_API_KEY (body key)",
    },
    MockAuthProfile {
        id: "google-v2-api-key-query",
        catalog_entry_ids: &["google-translate"],
        auth_mode: "key",
        official_path: "/language/translate/v2",
        profile_path: "/mock/profiles/google-v2-api-key-query/translate",
        max_input_chars: 30_000,
        input_fields: &["q", "source", "target"],
        translated_text_path: "data.translations.0.translatedText",
        scheme: AuthScheme::GoogleApiKeyQuery,
        credential_hint: "GOOGLE_API_KEY (?key=)",
    },
    MockAuthProfile {
        id: "google-v2-api-key-header",
        catalog_entry_ids: &["google-translate"],
        auth_mode: "key",
        official_path: "/language/translate/v2",
        profile_path: "/mock/profiles/google-v2-api-key-header/translate",
        max_input_chars: 30_000,
        input_fields: &["q", "source", "target"],
        translated_text_path: "data.translations.0.translatedText",
        scheme: AuthScheme::GoogleApiKeyHeader,
        credential_hint: "GOOGLE_API_KEY (x-goog-api-key)",
    },
    MockAuthProfile {
        id: "google-v3-oauth",
        catalog_entry_ids: &["google-advanced"],
        auth_mode: "oauth",
        official_path: "/v3/projects/YOUR_PROJECT/locations/global:translateText",
        profile_path: "/mock/profiles/google-v3-oauth/translate",
        max_input_chars: 30_000,
        input_fields: &["contents", "sourceLanguageCode", "targetLanguageCode"],
        translated_text_path: "translations.0.translatedText",
        scheme: AuthScheme::GoogleOAuthBearer,
        credential_hint: "GOOGLE_ACCESS_TOKEN (Bearer oauth)",
    },
    // Client catalog currently sends Bearer {{auth.api_key}} for google-advanced —
    // keep a separate profile so Lab can test that wire shape without conflating OAuth.
    MockAuthProfile {
        id: "google-v3-bearer-api-key",
        catalog_entry_ids: &["google-advanced"],
        auth_mode: "key",
        official_path: "/v3/projects/YOUR_PROJECT/locations/global:translateText",
        profile_path: "/mock/profiles/google-v3-bearer-api-key/translate",
        max_input_chars: 30_000,
        input_fields: &["q", "source", "target", "text", "contents"],
        translated_text_path: "translations.0.translatedText",
        scheme: AuthScheme::BearerApiKey,
        credential_hint: "BEARER_KEY (catalog google-advanced)",
    },
    // --- Azure Translator: subscription key vs Entra OAuth ---
    MockAuthProfile {
        id: "azure-translator-subscription-key",
        catalog_entry_ids: &["azure-cognitive", "microsoft-translator", "microsoft-custom"],
        auth_mode: "key",
        official_path: "/translate",
        profile_path: "/mock/profiles/azure-translator-subscription-key/translate",
        max_input_chars: 50_000,
        input_fields: &["text", "Text"],
        translated_text_path: "0.translations.0.text",
        scheme: AuthScheme::AzureSubscriptionKey,
        credential_hint: "AZURE_SUB_KEY (Ocp-Apim-Subscription-Key)",
    },
    MockAuthProfile {
        id: "azure-translator-oauth",
        catalog_entry_ids: &["azure-cognitive"],
        auth_mode: "oauth",
        official_path: "/translate",
        profile_path: "/mock/profiles/azure-translator-oauth/translate",
        max_input_chars: 50_000,
        input_fields: &["text", "Text"],
        translated_text_path: "0.translations.0.text",
        scheme: AuthScheme::AzureOAuthBearer,
        credential_hint: "AZURE_ACCESS_TOKEN (Bearer oauth)",
    },
    // --- Azure OpenAI: api-key header vs Bearer ---
    MockAuthProfile {
        id: "azure-openai-api-key",
        catalog_entry_ids: &["azure-openai"],
        auth_mode: "key",
        official_path: "/openai/deployments/YOUR_DEPLOYMENT/chat/completions",
        profile_path: "/mock/profiles/azure-openai-api-key/chat/completions",
        max_input_chars: 100_000,
        input_fields: &["messages"],
        translated_text_path: "choices.0.message.content",
        scheme: AuthScheme::AzureOpenAiApiKey,
        credential_hint: "BEARER_KEY (api-key header)",
    },
    MockAuthProfile {
        id: "azure-openai-oauth",
        catalog_entry_ids: &["azure-openai"],
        auth_mode: "oauth",
        official_path: "/openai/deployments/YOUR_DEPLOYMENT/chat/completions",
        profile_path: "/mock/profiles/azure-openai-oauth/chat/completions",
        max_input_chars: 100_000,
        input_fields: &["messages"],
        translated_text_path: "choices.0.message.content",
        scheme: AuthScheme::BearerApiKey,
        credential_hint: "AZURE_ACCESS_TOKEN or BEARER_KEY (Bearer)",
    },
    // --- DeepL / OpenAI / others (single scheme each, still profiled for Lab) ---
    MockAuthProfile {
        id: "deepl-auth-key",
        catalog_entry_ids: &["deepl", "deepl-pro"],
        auth_mode: "key",
        official_path: "/v2/translate",
        profile_path: "/mock/profiles/deepl-auth-key/translate",
        max_input_chars: 0,
        input_fields: &["text", "source_lang", "target_lang"],
        translated_text_path: "translations.0.text",
        scheme: AuthScheme::DeepLAuthKey,
        credential_hint: "DEEPL_AUTH_KEY",
    },
    MockAuthProfile {
        id: "openai-bearer",
        catalog_entry_ids: &["openai-compatible"],
        auth_mode: "key",
        official_path: "/v1/chat/completions",
        profile_path: "/mock/profiles/openai-bearer/chat/completions",
        max_input_chars: 100_000,
        input_fields: &["messages", "model"],
        translated_text_path: "choices.0.message.content",
        scheme: AuthScheme::BearerApiKey,
        credential_hint: "BEARER_KEY",
    },
    MockAuthProfile {
        id: "papago-naver",
        catalog_entry_ids: &["papago"],
        auth_mode: "key",
        official_path: "/v1/papago/n2mt",
        profile_path: "/mock/profiles/papago-naver/translate",
        max_input_chars: 5_000,
        input_fields: &["source", "target", "text"],
        translated_text_path: "message.result.translatedText",
        scheme: AuthScheme::PapagoNaver,
        credential_hint: "PAPAGO_CLIENT_ID/SECRET",
    },
    MockAuthProfile {
        id: "yandex-api-key",
        catalog_entry_ids: &["yandex"],
        auth_mode: "key",
        official_path: "/api/v1.5/tr.json/translate",
        profile_path: "/mock/profiles/yandex-api-key/translate",
        max_input_chars: 10_000,
        input_fields: &["text", "lang", "key"],
        translated_text_path: "text.0",
        scheme: AuthScheme::YandexApiKey,
        credential_hint: "YANDEX_API_KEY",
    },
    MockAuthProfile {
        id: "phrase-token",
        catalog_entry_ids: &["phrase"],
        auth_mode: "key",
        official_path: "/v2/projects/mt/translate",
        profile_path: "/mock/profiles/phrase-token/translate",
        max_input_chars: 10_000,
        input_fields: &["q", "source", "target"],
        translated_text_path: "translation",
        scheme: AuthScheme::PhraseToken,
        credential_hint: "BEARER_KEY (token …)",
    },
];

/// Reject oversized input the way vendor APIs do (400 + structured error).
pub fn reject_if_too_long(text: &str, max_input_chars: usize) -> Option<Response> {
    if max_input_chars == 0 {
        return None;
    }
    let actual = text.chars().count();
    if actual <= max_input_chars {
        return None;
    }
    Some(
        (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "input_too_long",
                "message": format!(
                    "Input length {actual} exceeds max_input_chars {max_input_chars}"
                ),
                "max_input_chars": max_input_chars,
                "actual_chars": actual
            })),
        )
            .into_response(),
    )
}

/// JSON inventory for WP client / Lab discovery.
pub fn profiles_inventory_json() -> Value {
    let items: Vec<Value> = all_profiles()
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "catalog_entry_ids": p.catalog_entry_ids,
                "auth_mode": p.auth_mode,
                "official_path": p.official_path,
                "profile_path": p.profile_path,
                "max_input_chars": p.max_input_chars,
                "input_fields": p.input_fields,
                "translated_text_path": p.translated_text_path,
                "scheme": format!("{:?}", p.scheme),
                "credential_hint": p.credential_hint,
                "credentials": credential_values(p.scheme),
            })
        })
        .collect();
    json!({
        "count": items.len(),
        "profiles": items,
        "signed_routes": signed_routes_inventory(),
        "note": "Multi-auth vendors are split into multiple profiles (e.g. google-v2-api-key-* vs google-v3-oauth). Signed vendors use /api/<vendor>/translate or official paths with verify_*."
    })
}

fn signed_routes_inventory() -> Value {
    json!([
        {"entry_id":"baidu","algorithm":"md5","paths":["/api/trans/vip/translate","/api/baidu/translate"],"auth_fields":["app_id","api_key"],"credentials":{"appid":config::BAIDU_APPID,"secret":config::BAIDU_SECRET},"translated_text_path":"trans_result.0.dst"},
        {"entry_id":"youdao","algorithm":"sha256","paths":["/api","/api/youdao/translate"],"auth_fields":["app_key","app_secret"],"credentials":{"app_key":config::YOUDAO_APP_KEY,"app_secret":config::YOUDAO_APP_SECRET},"translated_text_path":"translation.0"},
        {"entry_id":"hmac-generic","algorithm":"hmac_sha256","paths":["/api/hmac/translate"],"auth_fields":["api_key","secret_key"],"credentials":{"api_key":config::HMAC_API_KEY,"secret":config::HMAC_SECRET},"translated_text_path":"translated_text"},
        {"entry_id":"tencent","algorithm":"tc3_hmac_sha256","paths":["/","/api/tencent/translate"],"auth_fields":["secret_id","secret_key"],"credentials":{"secret_id":config::TC3_SECRET_ID,"secret_key":config::TC3_SECRET_KEY},"translated_text_path":"Response.TargetText"},
        {"entry_id":"amazon-translate","algorithm":"aws_sigv4","paths":["/","/api/aws/translate"],"auth_fields":["access_key","secret_key"],"credentials":{"access_key":config::AWS_ACCESS_KEY,"secret_key":config::AWS_SECRET_KEY},"translated_text_path":"TranslatedText"},
        {"entry_id":"volcengine","algorithm":"volcengine_hmac_sha256","paths":["/","/api/volcengine/translate"],"auth_fields":["access_key","secret_key"],"credentials":{"access_key":config::VOLC_ACCESS_KEY,"secret_key":config::VOLC_SECRET_KEY},"translated_text_path":"Translation"},
        {"entry_id":"alibaba","algorithm":"alibaba_v1","paths":["/api/translate/web","/api/alibaba/translate"],"auth_fields":["access_key","secret_key"],"credentials":{"access_key":config::ALI_ACCESS_KEY,"secret_key":config::ALI_SECRET_KEY},"translated_text_path":"Data.TranslatedText"},
        {"entry_id":"iflytek","algorithm":"md5","paths":["/v1/its","/api/iflytek/translate"],"auth_fields":["app_id","api_secret"],"credentials":{"app_id":config::IFLYTEK_APP_ID,"api_secret":config::IFLYTEK_API_SECRET},"translated_text_path":"data.trans_result.dst"},
        {"entry_id":"niutrans","algorithm":"md5","paths":["/NiuTransServer/translation","/api/niutrans/translate"],"auth_fields":["api_key"],"credentials":{"api_key":config::NIUTRANS_API_KEY},"translated_text_path":"tgt_text"},
        {"entry_id":"kakao","algorithm":"kakao_api_key","paths":["/v2/translation/translate","/api/kakao/translate"],"auth_fields":["api_key"],"credentials":{"api_key":config::KAKAO_KEY},"translated_text_path":"translated_text.0.0"},
        {"entry_id":"azure-cognitive","algorithm":"azure_subscription_key","paths":["/translate","/api/azure/translate"],"auth_fields":["subscription_key"],"credentials":{"subscription_key":config::AZURE_SUB_KEY},"translated_text_path":"0.translations.0.text"},
        {"entry_id":"oauth-mt","algorithm":"oauth_client_credentials","paths":["/api/oauth/token","/api/oauth/translate"],"auth_fields":["client_id","client_secret"],"credentials":{"client_id":config::OAUTH_CLIENT_ID,"client_secret":config::OAUTH_CLIENT_SECRET},"translated_text_path":"translated_text"},
        {"entry_id":"jwt-mt","algorithm":"jwt_rs256","paths":["/api/jwt/translate"],"auth_fields":["jwt_token"],"credentials":{"hint":"sign with JWT_RSA_PRIVATE_KEY_PEM"},"translated_text_path":"translated_text"}
    ])
}

fn credential_values(scheme: AuthScheme) -> Value {
    match scheme {
        AuthScheme::GoogleApiKeyBody
        | AuthScheme::GoogleApiKeyQuery
        | AuthScheme::GoogleApiKeyHeader => json!({ "api_key": config::GOOGLE_API_KEY }),
        AuthScheme::GoogleOAuthBearer => json!({ "access_token": config::GOOGLE_ACCESS_TOKEN }),
        AuthScheme::BearerApiKey => json!({ "api_key": config::BEARER_KEY }),
        AuthScheme::DeepLAuthKey => json!({ "api_key": config::DEEPL_AUTH_KEY }),
        AuthScheme::AzureSubscriptionKey => json!({ "subscription_key": config::AZURE_SUB_KEY }),
        AuthScheme::AzureOAuthBearer => json!({ "access_token": config::AZURE_ACCESS_TOKEN }),
        AuthScheme::AzureOpenAiApiKey => json!({ "api_key": config::BEARER_KEY }),
        AuthScheme::PhraseToken => json!({ "api_key": config::BEARER_KEY }),
        AuthScheme::PapagoNaver => json!({
            "client_id": config::PAPAGO_CLIENT_ID,
            "client_secret": config::PAPAGO_CLIENT_SECRET
        }),
        AuthScheme::YandexApiKey => json!({ "api_key": config::YANDEX_API_KEY }),
    }
}

pub fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_is_split_into_key_and_oauth_profiles() {
        let google: Vec<_> = all_profiles()
            .iter()
            .filter(|p| p.id.starts_with("google-"))
            .collect();
        assert!(google.len() >= 4);
        assert!(google.iter().any(|p| p.auth_mode == "key"));
        assert!(google.iter().any(|p| p.auth_mode == "oauth"));
        assert!(google.iter().any(|p| p.scheme == AuthScheme::GoogleOAuthBearer));
    }

    #[test]
    fn azure_is_split_into_subscription_and_oauth() {
        let azure: Vec<_> = profiles_for_catalog_entry("azure-cognitive");
        assert!(azure.iter().any(|p| p.id.contains("subscription-key")));
        assert!(azure.iter().any(|p| p.id.contains("oauth")));
    }

    #[test]
    fn catalog_google_advanced_maps_to_multiple_profiles() {
        let list = profiles_for_catalog_entry("google-advanced");
        assert!(list.len() >= 2);
    }

    #[test]
    fn reject_if_too_long_triggers_at_limit() {
        assert!(reject_if_too_long("abc", 3).is_none());
        assert!(reject_if_too_long("abcd", 3).is_some());
        assert!(reject_if_too_long("anything", 0).is_none());
    }

    #[test]
    fn inventory_lists_all_profiles() {
        let inv = profiles_inventory_json();
        assert_eq!(inv["count"], all_profiles().len());
    }
}
