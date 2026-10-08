use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Text ---

#[derive(Deserialize)]
pub struct TextRequest {
    pub text: String,
    pub source_lang: String,
    pub target_lang: String,
    #[serde(default)]
    pub content_format: String,
    #[serde(default)]
    pub input_type: String,
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Serialize)]
pub struct TextResponse {
    pub translated_text: String,
    pub source_lang: String,
    pub target_lang: String,
    pub used_key: String,
    pub active_concurrency: usize,
}

// --- Media (image / audio / video / document) ---

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct MediaRequest {
    pub source_ref: String,
    pub source_lang: String,
    pub target_lang: String,
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Serialize)]
pub struct MediaResponse {
    pub translated_ref: String,
    #[serde(rename = "type")]
    pub ref_type: String,
    pub used_key: String,
    pub active_concurrency: usize,
    /// Text product beside the file URL: OCR, subtitle, transcript, or
    /// extracted document text. File replacement stays in `translated_ref`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub translated_text: String,
}

// --- Batch ---

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum BatchItem {
    Text {
        text: String,
        source_lang: String,
        target_lang: String,
        #[serde(default)]
        content_format: String,
    },
    Image {
        source_ref: String,
        source_lang: String,
        target_lang: String,
    },
    Audio {
        source_ref: String,
        source_lang: String,
        target_lang: String,
    },
    Video {
        source_ref: String,
        source_lang: String,
        target_lang: String,
    },
    Document {
        source_ref: String,
        source_lang: String,
        target_lang: String,
    },
}

#[derive(Deserialize)]
pub struct BatchRequest {
    pub items: Vec<BatchItem>,
    #[serde(default)]
    pub delay_ms: u64,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum BatchResultItem {
    Text(TextResponse),
    Media(MediaResponse),
}

#[derive(Serialize)]
pub struct BatchResponse {
    pub results: Vec<BatchResultItem>,
}

// --- Health ---

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

// --- Auth error response ---

#[derive(Serialize)]
pub struct AuthErrorResponse {
    pub error: String,
    pub message: String,
    pub algorithm: String,
}

// --- Baidu ---

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct BaiduRequest {
    pub q: String,
    pub from: String,
    pub to: String,
    pub appid: String,
    pub salt: String,
    pub sign: String,
}

// --- Youdao ---

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct YoudaoRequest {
    pub q: String,
    pub from: String,
    pub to: String,
    #[serde(alias = "appKey")]
    pub app_key: String,
    pub salt: String,
    pub sign: String,
    #[serde(alias = "signType", default)]
    pub sign_type: String,
    #[serde(default)]
    pub curtime: String,
}

// --- HMAC ---

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct HmacRequest {
    pub text: String,
    pub source_lang: String,
    pub target_lang: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(alias = "timestamp", default)]
    pub salt: String,
    pub sign: String,
    #[serde(default)]
    pub delay_ms: u64,
}

// --- Generic signed request (TC3, AWS, Volc) ---
// These use headers for auth, so body is a plain text translate request.

#[derive(Deserialize, Clone)]
pub struct GenericTranslateRequest {
    #[serde(alias = "query", default)]
    pub text: String,
    #[serde(alias = "Text", alias = "SourceText", default)]
    pub text_alt: String,
    #[serde(
        alias = "SourceLanguageCode",
        alias = "source_lang",
        alias = "Source",
        alias = "SourceLanguage",
        alias = "from",
        alias = "src_lang",
        default
    )]
    pub source_lang: String,
    #[serde(
        alias = "TargetLanguageCode",
        alias = "target_lang",
        alias = "Target",
        alias = "TargetLanguage",
        alias = "to",
        default
    )]
    pub target_lang: String,
    #[serde(default)]
    pub delay_ms: u64,
}

impl GenericTranslateRequest {
    pub fn get_text(&self) -> &str {
        if !self.text.is_empty() {
            &self.text
        } else {
            &self.text_alt
        }
    }
}

// --- Alibaba ---

// Alibaba sends params as form/query params, not JSON body.
// We handle this in the handler by parsing the raw body.

// --- OAuth ---

#[derive(Deserialize)]
pub struct OAuthTokenRequest {
    pub grant_type: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

// --- Stats ---

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeyStats {
    pub total_requests: usize,
    pub active: usize,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub keys: HashMap<String, KeyStats>,
    pub total_requests: usize,
}
