// NOTE: This standalone mock is the unified local mock translation service
// used by official mock component templates (http://127.0.0.1:9090).
// It is also used for isolated testing (e.g. per-vendor auth verification).

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::Config;
use crate::types::*;
use crate::verify;
use crate::AppState;

// ---------------------------------------------------------------------------
// Core translation logic
// ---------------------------------------------------------------------------

fn wrap_with_lang_markers(text: &str, target_lang: &str) -> String {
    format!("\u{3010}{target_lang}\u{3011}{text}\u{3010}/{target_lang}\u{3011}")
}

fn looks_like_html(text: &str) -> bool {
    let bytes = text.as_bytes();
    if !bytes.contains(&b'<') || !bytes.contains(&b'>') {
        return false;
    }
    bytes
        .windows(2)
        .any(|w| w[0] == b'<' && (w[1].is_ascii_alphabetic() || w[1] == b'/' || w[1] == b'!'))
}

fn wrap_text_segment(segment: &str, target_lang: &str) -> String {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return segment.to_string();
    }
    let prefix_len = segment.find(trimmed).unwrap_or(0);
    let suffix_start = prefix_len + trimmed.len();
    format!(
        "{}{}{}",
        &segment[..prefix_len],
        wrap_with_lang_markers(trimmed, target_lang),
        &segment[suffix_start..]
    )
}

fn translate_html_text_nodes(html: &str, target_lang: &str) -> String {
    let mut out = String::with_capacity(html.len() + 32);
    let mut text_buf = String::new();
    let mut in_tag = false;
    let mut quote: Option<char> = None;

    for ch in html.chars() {
        if in_tag {
            out.push(ch);
            match quote {
                Some(q) if ch == q => quote = None,
                None if ch == '"' || ch == '\'' => quote = Some(ch),
                None if ch == '>' => in_tag = false,
                _ => {}
            }
            continue;
        }

        if ch == '<' {
            if !text_buf.is_empty() {
                out.push_str(&wrap_text_segment(&text_buf, target_lang));
                text_buf.clear();
            }
            in_tag = true;
            out.push(ch);
            continue;
        }

        text_buf.push(ch);
    }

    if !text_buf.is_empty() {
        out.push_str(&wrap_text_segment(&text_buf, target_lang));
    }

    out
}

fn is_html_content_hint(content_format: &str, input_type: &str) -> bool {
    let format = content_format.trim().to_ascii_lowercase();
    let input = input_type.trim().to_ascii_lowercase();
    matches!(
        format.as_str(),
        "rich_html" | "html" | "text/html" | "application/xhtml+xml"
    ) || matches!(input.as_str(), "rich_html" | "html")
}

/// Wrap plain text with language markers; for HTML input only tag text nodes.
pub(crate) fn translate_text(text: &str, target_lang: &str) -> String {
    if looks_like_html(text) {
        translate_html_text_nodes(text, target_lang)
    } else {
        wrap_with_lang_markers(text, target_lang)
    }
}

fn normalize_lang_suffix(lang: &str) -> String {
    let trimmed = lang.trim();
    if trimmed.is_empty() {
        return "auto".to_string();
    }
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn append_lang_suffix_to_filename(filename: &str, target_lang: &str) -> String {
    match filename.rfind('.') {
        Some(dot) if dot > 0 => {
            let (base, ext) = filename.split_at(dot);
            format!("{base}-{target_lang}{ext}")
        }
        _ => format!("{filename}-{target_lang}"),
    }
}

/// Return the URL to a pre-prepared translated media file served by this server.
pub(crate) fn translated_media_url(media_type: &str) -> String {
    let config = Config::from_env();
    let filename = match media_type {
        "image" => "image-translated.png",
        "video" => "video-translated.mp4",
        "audio" => "audio-translated.mp3",
        "document" => "document-translated.pdf",
        _ => "document-translated.pdf",
    };
    format!("{}/media/{}", config.base_url, filename)
}

fn translate_media_ref(source_ref: &str, target_lang: &str) -> String {
    let lang = normalize_lang_suffix(target_lang);

    let (without_fragment, fragment) = match source_ref.find('#') {
        Some(i) => (&source_ref[..i], &source_ref[i..]),
        None => (source_ref, ""),
    };
    let (without_query, query) = match without_fragment.find('?') {
        Some(i) => (&without_fragment[..i], &without_fragment[i..]),
        None => (without_fragment, ""),
    };

    let slash = without_query.rfind('/').map(|idx| idx + 1).unwrap_or(0);
    let (prefix, filename) = without_query.split_at(slash);
    let translated_name = append_lang_suffix_to_filename(filename, &lang);

    format!("{prefix}{translated_name}{query}{fragment}")
}

pub(crate) fn media_translated_text(media_type: &str, source_ref: &str, target_lang: &str) -> String {
    let kind = match media_type {
        "image" => "ocr",
        "video" => "subtitle",
        "audio" => "transcript",
        _ => "document",
    };
    let source = if source_ref.trim().is_empty() {
        media_type
    } else {
        source_ref.trim()
    };
    translate_text(&format!("{kind}:{source}"), target_lang)
}

fn media_response(
    source_ref: &str,
    target_lang: &str,
    media_type: &str,
    used_key: String,
    active_concurrency: usize,
) -> MediaResponse {
    MediaResponse {
        translated_ref: translate_media_or_default(source_ref, target_lang, media_type),
        translated_text: media_translated_text(media_type, source_ref, target_lang),
        ref_type: "url".into(),
        used_key,
        active_concurrency,
    }
}

pub(crate) fn translate_media_or_default(
    source_ref: &str,
    target_lang: &str,
    media_type: &str,
) -> String {
    let source = source_ref.trim();
    if source.is_empty() {
        translated_media_url(media_type)
    } else {
        translate_media_ref(source, target_lang)
    }
}

// ---------------------------------------------------------------------------
// Helper: auth error -> JSON response
// ---------------------------------------------------------------------------

pub(crate) fn auth_error_response(err: verify::AuthError) -> Response {
    let body = AuthErrorResponse {
        error: "auth_failed".to_string(),
        message: err.message.clone(),
        algorithm: err.algorithm.to_string(),
    };
    let mut resp = (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    let safe_msg = err.message.replace(['\r', '\n'], " ");
    if let Ok(val) = HeaderValue::from_str(&safe_msg) {
        resp.headers_mut().insert("x-mock-auth-error", val);
    }
    if let Ok(val) = HeaderValue::from_str(err.algorithm) {
        resp.headers_mut().insert("x-mock-auth-algo", val);
    }
    resp
}

fn text_response_with_stats(
    translated_text: String,
    target_lang: &str,
    source_lang: &str,
    used_key: String,
    active_concurrency: usize,
) -> Response {
    Json(TextResponse {
        translated_text,
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        used_key,
        active_concurrency,
    })
    .into_response()
}

fn text_response_with_stats_auto(
    text: &str,
    target_lang: &str,
    source_lang: &str,
    used_key: String,
    active_concurrency: usize,
) -> Response {
    text_response_with_stats(
        translate_text(text, target_lang),
        target_lang,
        source_lang,
        used_key,
        active_concurrency,
    )
}

// ---------------------------------------------------------------------------
// Helper: extract Bearer token from Authorization header
// ---------------------------------------------------------------------------

pub fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Authorization")
        .or_else(|| headers.get("authorization"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Helper: increment per-key concurrency, returns active count
// ---------------------------------------------------------------------------

pub(crate) async fn key_enter(state: &AppState, key_id: &str) -> usize {
    let mut stats = state.key_stats.lock().await;
    let entry = stats.entry(key_id.to_string()).or_default();
    entry.active += 1;
    entry.total_requests += 1;
    let active = entry.active;
    state.total_requests.fetch_add(1, Ordering::Relaxed);
    active
}

pub(crate) async fn key_exit(state: &AppState, key_id: &str) {
    let mut stats = state.key_stats.lock().await;
    if let Some(entry) = stats.get_mut(key_id) {
        entry.active = entry.active.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------
// Existing routes (Bearer token auth via middleware)
// ---------------------------------------------------------------------------

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        service: "mock-translate-api".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    })
}

pub async fn list_languages() -> Json<Value> {
    Json(crate::mock_failing::languages_payload())
}

pub async fn list_models() -> Json<Value> {
    Json(crate::mock_failing::models_payload())
}

pub async fn translate_text_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TextRequest>,
) -> impl IntoResponse {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let translated_text = if is_html_content_hint(&req.content_format, &req.input_type) {
        translate_html_text_nodes(&req.text, &req.target_lang)
    } else {
        translate_text(&req.text, &req.target_lang)
    };
    let resp = text_response_with_stats(
        translated_text,
        &req.target_lang,
        &req.source_lang,
        key_id.clone(),
        active,
    );
    key_exit(&state, &key_id).await;
    resp
}

/// OpenAI-compatible endpoint used by local fallback components in E2E tests.
///
/// Supported input shape (subset):
/// - model: string
/// - messages: [{ role, content }]
///
/// Returns:
/// - choices[0].message.content with translated text markers.
pub async fn openai_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let _active = key_enter(&state, &key_id).await;

    if key_id.contains("bad")
        || key_id.contains("invalid")
        || (key_id != "anonymous"
            && key_id != crate::config::BEARER_KEY
            && !key_id.starts_with("eyJ"))
    {
        key_exit(&state, &key_id).await;
        let mut resp = (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "Incorrect API key provided.",
                    "type": "invalid_request_error",
                    "param": null,
                    "code": "invalid_api_key"
                }
            })),
        )
            .into_response();
        resp.headers_mut().insert("x-mock-auth-error", HeaderValue::from_static("openai_invalid_api_key"));
        resp.headers_mut().insert("x-mock-vendor", HeaderValue::from_static("openai-chat"));
        return resp;
    }

    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("mock-openai");

    let messages = req
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let user_text = messages
        .iter()
        .rev()
        .find(|msg| msg.get("role").and_then(|v| v.as_str()) == Some("user"))
        .and_then(|msg| msg.get("content").and_then(|v| v.as_str()))
        .unwrap_or("");

    let target_lang = messages
        .iter()
        .find(|msg| msg.get("role").and_then(|v| v.as_str()) == Some("system"))
        .and_then(|msg| msg.get("content").and_then(|v| v.as_str()))
        .and_then(|content| {
            let lower = content.to_ascii_lowercase();
            let pos = lower.find(" to ")?;
            let tail = &content[pos + 4..];
            let candidate = tail.split_whitespace().next().unwrap_or("");
            let normalized = candidate
                .trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
            if normalized.is_empty() {
                None
            } else {
                Some(normalized.to_string())
            }
        })
        .unwrap_or_else(|| "target".to_string());

    let translated = translate_text(user_text, &target_lang);
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let stream = req
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if stream {
        let body = crate::mock_failing::openai_sse_body(model, &translated);
        let resp = Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(axum::body::Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        key_exit(&state, &key_id).await;
        return resp;
    }

    let payload = json!({
        "id": format!("chatcmpl-mock-{}", created),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": translated
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        }
    });

    let resp = Json(payload).into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn translate_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MediaRequest>,
) -> impl IntoResponse {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let resp = Json(media_response(
        &req.source_ref,
        &req.target_lang,
        "image",
        key_id.clone(),
        active,
    ))
    .into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn translate_audio(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MediaRequest>,
) -> impl IntoResponse {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let resp = Json(media_response(
        &req.source_ref,
        &req.target_lang,
        "audio",
        key_id.clone(),
        active,
    ))
    .into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn translate_video(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MediaRequest>,
) -> impl IntoResponse {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let resp = Json(media_response(
        &req.source_ref,
        &req.target_lang,
        "video",
        key_id.clone(),
        active,
    ))
    .into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn translate_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MediaRequest>,
) -> impl IntoResponse {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let resp = Json(media_response(
        &req.source_ref,
        &req.target_lang,
        "document",
        key_id.clone(),
        active,
    ))
    .into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn translate_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BatchRequest>,
) -> impl IntoResponse {
    let key_id = extract_bearer_token(&headers).unwrap_or_else(|| "anonymous".to_string());
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let results = req
        .items
        .into_iter()
        .map(|item| match item {
            BatchItem::Text {
                text,
                source_lang,
                target_lang,
                content_format: _,
            } => BatchResultItem::Text(TextResponse {
                translated_text: translate_text(&text, &target_lang),
                source_lang,
                target_lang,
                used_key: key_id.clone(),
                active_concurrency: active,
            }),
            BatchItem::Image {
                source_ref,
                target_lang,
                ..
            } => BatchResultItem::Media(media_response(
                &source_ref,
                &target_lang,
                "image",
                key_id.clone(),
                active,
            )),
            BatchItem::Audio {
                source_ref,
                target_lang,
                ..
            } => BatchResultItem::Media(media_response(
                &source_ref,
                &target_lang,
                "audio",
                key_id.clone(),
                active,
            )),
            BatchItem::Video {
                source_ref,
                target_lang,
                ..
            } => BatchResultItem::Media(media_response(
                &source_ref,
                &target_lang,
                "video",
                key_id.clone(),
                active,
            )),
            BatchItem::Document {
                source_ref,
                target_lang,
                ..
            } => BatchResultItem::Media(media_response(
                &source_ref,
                &target_lang,
                "document",
                key_id.clone(),
                active,
            )),
        })
        .collect();

    let resp = Json(BatchResponse { results }).into_response();
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Stats endpoints
// ---------------------------------------------------------------------------

pub async fn handle_stats(State(state): State<AppState>) -> impl IntoResponse {
    let stats = state.key_stats.lock().await;
    let response = StatsResponse {
        keys: stats.clone(),
        total_requests: state.total_requests.load(Ordering::Relaxed),
    };
    Json(response).into_response()
}

pub async fn handle_stats_reset(State(state): State<AppState>) -> impl IntoResponse {
    let mut stats = state.key_stats.lock().await;
    stats.clear();
    state.total_requests.store(0, Ordering::Relaxed);
    Json(serde_json::json!({"ok": true})).into_response()
}

// ---------------------------------------------------------------------------
// Virtual test file endpoint
// ---------------------------------------------------------------------------

pub async fn handle_test_file(Path(size_kb): Path<u64>, method: Method) -> impl IntoResponse {
    // Cap at 100 MB to avoid runaway allocations.
    let size_kb = size_kb.min(100 * 1024);
    let content_length = size_kb * 1024;

    if method == Method::HEAD {
        Response::builder()
            .status(200)
            .header("Content-Length", content_length.to_string())
            .header("Content-Type", "application/octet-stream")
            .body(axum::body::Body::empty())
            .unwrap()
    } else {
        // For GET: limit actual allocation to 10 MB; larger requests get a
        // 413 to avoid OOM.  HEAD always works for any advertised size.
        const GET_LIMIT_KB: u64 = 10 * 1024;
        if size_kb > GET_LIMIT_KB {
            return Response::builder()
                .status(413)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"error":"too_large","message":"Use HEAD for files >10 MB"}"#,
                ))
                .unwrap();
        }
        let body = vec![0u8; content_length as usize];
        Response::builder()
            .status(200)
            .header("Content-Length", content_length.to_string())
            .header("Content-Type", "application/octet-stream")
            .body(axum::body::Body::from(body))
            .unwrap()
    }
}

// ---------------------------------------------------------------------------
// Baidu: MD5 sign verification
// ---------------------------------------------------------------------------

pub async fn baidu_translate(State(state): State<AppState>, body: String) -> Response {
    // Accept both JSON and form-urlencoded body.
    let (appid, q, salt, sign, from, to) =
        if let Ok(req) = serde_json::from_str::<BaiduRequest>(&body) {
            (req.appid, req.q, req.salt, req.sign, req.from, req.to)
        } else {
            let pairs: std::collections::HashMap<String, String> =
                form_urlencoded::parse(body.as_bytes())
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
            (
                pairs.get("appid").cloned().unwrap_or_default(),
                pairs.get("q").cloned().unwrap_or_default(),
                pairs.get("salt").cloned().unwrap_or_default(),
                pairs.get("sign").cloned().unwrap_or_default(),
                pairs.get("from").cloned().unwrap_or_default(),
                pairs.get("to").cloned().unwrap_or_default(),
            )
        };

    let key_id = format!("baidu:{}", appid);
    let active = key_enter(&state, &key_id).await;

    let params = verify::BaiduParams {
        appid,
        q: q.clone(),
        salt,
        sign,
    };

    if let Err(e) = verify::verify_baidu(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(&q, &to, &from, key_id.clone(), active);
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Youdao: SHA256 sign verification
// ---------------------------------------------------------------------------

pub async fn youdao_translate(State(state): State<AppState>, body: String) -> Response {
    // Accept both JSON and form-urlencoded body.
    let (app_key, q, salt, curtime, sign, sign_type, from, to) =
        if let Ok(req) = serde_json::from_str::<YoudaoRequest>(&body) {
            (
                req.app_key,
                req.q,
                req.salt,
                req.curtime,
                req.sign,
                req.sign_type,
                req.from,
                req.to,
            )
        } else {
            let pairs: std::collections::HashMap<String, String> =
                form_urlencoded::parse(body.as_bytes())
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
            (
                pairs
                    .get("appKey")
                    .or(pairs.get("app_key"))
                    .cloned()
                    .unwrap_or_default(),
                pairs.get("q").cloned().unwrap_or_default(),
                pairs.get("salt").cloned().unwrap_or_default(),
                pairs.get("curtime").cloned().unwrap_or_default(),
                pairs.get("sign").cloned().unwrap_or_default(),
                pairs
                    .get("signType")
                    .or(pairs.get("sign_type"))
                    .cloned()
                    .unwrap_or_default(),
                pairs.get("from").cloned().unwrap_or_default(),
                pairs.get("to").cloned().unwrap_or_default(),
            )
        };

    let key_id = format!("youdao:{}", app_key);
    let active = key_enter(&state, &key_id).await;

    let params = verify::YoudaoParams {
        app_key,
        q: q.clone(),
        salt,
        curtime,
        sign,
        sign_type,
    };

    if let Err(e) = verify::verify_youdao(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(&q, &to, &from, key_id.clone(), active);
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// HMAC-SHA256 sign verification
// ---------------------------------------------------------------------------

pub async fn hmac_translate(
    State(state): State<AppState>,
    Json(req): Json<HmacRequest>,
) -> Response {
    // api_key may come in body or be inferred from config (when client omits it from body).
    let api_key = if req.api_key.is_empty() {
        crate::config::HMAC_API_KEY.to_string()
    } else {
        req.api_key.clone()
    };

    let key_id = format!("hmac:{}", api_key);
    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    let params = verify::HmacParams {
        api_key,
        text: req.text.clone(),
        salt: req.salt,
        sign: req.sign,
    };

    if let Err(e) = verify::verify_hmac(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(
        &req.text,
        &req.target_lang,
        &req.source_lang,
        key_id.clone(),
        active,
    );
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// TC3-HMAC-SHA256 (Tencent Cloud)
// ---------------------------------------------------------------------------

pub async fn tencent_translate(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let timestamp = headers
        .get("x-tc-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Extract secret_id from Credential field for key tracking.
    let key_id = authorization
        .find("Credential=")
        .map(|i| {
            let rest = &authorization[i + "Credential=".len()..];
            let end = rest.find('/').unwrap_or(rest.len());
            format!("tencent:{}", &rest[..end])
        })
        .unwrap_or_else(|| "tencent:unknown".to_string());

    let active = key_enter(&state, &key_id).await;

    let params = verify::Tc3Params {
        authorization,
        timestamp,
        body: body.clone(),
        host,
        path: uri.path().to_string(),
    };

    if let Err(e) = verify::verify_tc3(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let req: GenericTranslateRequest =
        serde_json::from_str(&body).unwrap_or(GenericTranslateRequest {
            text: String::new(),
            text_alt: String::new(),
            source_lang: "auto".into(),
            target_lang: "en".into(),
            delay_ms: 0,
        });

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    // Catalog path: Response.TargetText (Tencent Cloud TMT).
    let translated = translate_text(req.get_text(), &req.target_lang);
    let resp = (
        StatusCode::OK,
        Json(json!({
            "Response": { "TargetText": translated },
            "used_key": key_id.clone(),
            "active_concurrency": active,
            "source_lang": req.source_lang,
            "target_lang": req.target_lang,
        })),
    )
        .into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn aws_translate(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let amz_date = headers
        .get("x-amz-date")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let key_id = authorization
        .find("Credential=")
        .map(|i| {
            let rest = &authorization[i + "Credential=".len()..];
            let end = rest.find('/').unwrap_or(rest.len());
            format!("aws:{}", &rest[..end])
        })
        .unwrap_or_else(|| "aws:unknown".to_string());

    let active = key_enter(&state, &key_id).await;

    let params = verify::AwsSigV4Params {
        authorization,
        amz_date,
        body: body.clone(),
        host,
        path: uri.path().to_string(),
    };

    if let Err(e) = verify::verify_aws_sigv4(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let req: GenericTranslateRequest =
        serde_json::from_str(&body).unwrap_or(GenericTranslateRequest {
            text: String::new(),
            text_alt: String::new(),
            source_lang: "auto".into(),
            target_lang: "en".into(),
            delay_ms: 0,
        });

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    // Catalog path: TranslatedText (Amazon Translate).
    let translated = translate_text(req.get_text(), &req.target_lang);
    let resp = (
        StatusCode::OK,
        Json(json!({
            "TranslatedText": translated,
            "used_key": key_id.clone(),
            "active_concurrency": active,
            "source_lang": req.source_lang,
            "target_lang": req.target_lang,
        })),
    )
        .into_response();
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Volcengine HMAC-SHA256 (SigV4 variant)
// ---------------------------------------------------------------------------

pub async fn volcengine_translate(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let amz_date = headers
        .get("x-amz-date")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let key_id = authorization
        .find("Credential=")
        .map(|i| {
            let rest = &authorization[i + "Credential=".len()..];
            let end = rest.find('/').unwrap_or(rest.len());
            format!("volcengine:{}", &rest[..end])
        })
        .unwrap_or_else(|| "volcengine:unknown".to_string());

    let active = key_enter(&state, &key_id).await;

    let params = verify::AwsSigV4Params {
        authorization,
        amz_date,
        body: body.clone(),
        host,
        path: uri.path().to_string(),
    };

    if let Err(e) = verify::verify_volcengine(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let req: GenericTranslateRequest =
        serde_json::from_str(&body).unwrap_or(GenericTranslateRequest {
            text: String::new(),
            text_alt: String::new(),
            source_lang: "auto".into(),
            target_lang: "en".into(),
            delay_ms: 0,
        });

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    // Catalog path: Translation (Volcengine).
    let text = {
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(json!({}));
        parsed
            .get("SourceText")
            .or_else(|| parsed.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| req.get_text())
            .to_string()
    };
    let target = {
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(json!({}));
        parsed
            .get("TargetLanguage")
            .or_else(|| parsed.get("target_lang"))
            .and_then(|v| v.as_str())
            .unwrap_or(&req.target_lang)
            .to_string()
    };
    let translated = translate_text(&text, &target);
    let resp = (
        StatusCode::OK,
        Json(json!({
            "Translation": translated,
            "used_key": key_id.clone(),
            "active_concurrency": active,
            "source_lang": req.source_lang,
            "target_lang": target,
        })),
    )
        .into_response();
    key_exit(&state, &key_id).await;
    resp
}

pub async fn alibaba_translate(State(state): State<AppState>, body: String) -> Response {
    let form_params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let signature = form_params
        .iter()
        .find(|(k, _)| k == "Signature" || k == "signature")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();

    let source_text = form_params
        .iter()
        .find(|(k, _)| k == "SourceText" || k == "q")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let target_lang = form_params
        .iter()
        .find(|(k, _)| k == "TargetLanguage" || k == "to")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "en".to_string());
    let source_lang = form_params
        .iter()
        .find(|(k, _)| k == "SourceLanguage" || k == "from")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "auto".to_string());
    let access_key_id = form_params
        .iter()
        .find(|(k, _)| k == "AccessKeyId")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();

    let key_id = format!("alibaba:{}", access_key_id);
    let active = key_enter(&state, &key_id).await;

    let params = verify::AlibabaParams {
        params: form_params,
        signature,
    };

    if let Err(e) = verify::verify_alibaba(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    // Catalog path is Data.TranslatedText (official Alibaba TranslateGeneral shape).
    let translated = translate_text(&source_text, &target_lang);
    let resp = (
        StatusCode::OK,
        Json(json!({
            "Data": {
                "TranslatedText": translated,
                "Translated": translated
            },
            "used_key": key_id.clone(),
            "active_concurrency": active,
            "source_lang": source_lang,
            "target_lang": target_lang,
        })),
    )
        .into_response();
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// iFlytek V1: X-CheckSum header (MD5 of APISecret + CurTime + X-Param)
// ---------------------------------------------------------------------------

pub async fn iflytek_translate(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let app_id = headers
        .get("X-Appid")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let cur_time = headers
        .get("X-CurTime")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let checksum = headers
        .get("X-CheckSum")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let x_param = headers
        .get("X-Param")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Extract text from body (X-Param is JSON: {"from":"en","to":"zh","text":"hello"})
    // For mock simplicity, parse body JSON to get "text"
    let (source_text, target_lang, source_lang) = match serde_json::from_str::<Value>(&body) {
        Ok(v) => {
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let to = v.get("to").and_then(|x| x.as_str()).unwrap_or("en").to_string();
            let from = v.get("from").and_then(|x| x.as_str()).unwrap_or("auto").to_string();
            (text, to, from)
        }
        Err(_) => ("".to_string(), "en".to_string(), "auto".to_string()),
    };

    let key_id = format!("iflytek:{}", app_id);
    let active = key_enter(&state, &key_id).await;

    let params = verify::IflytekParams {
        app_id,
        cur_time,
        checksum,
        param: x_param,
        body,
    };

    if let Err(e) = verify::verify_iflytek(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(
        &source_text,
        &target_lang,
        &source_lang,
        key_id.clone(),
        active,
    );
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Niutrans V2: sign in body (lowercase MD5 of api_key + q + from + to + api_key)
// ---------------------------------------------------------------------------

pub async fn niutrans_translate(
    State(state): State<AppState>,
    body: String,
) -> Response {
    let form_params: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let api_key = form_params
        .iter()
        .find(|(k, _)| k == "apikey")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let q = form_params
        .iter()
        .find(|(k, _)| k == "q" || k == "text")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let from = form_params
        .iter()
        .find(|(k, _)| k == "from")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "auto".to_string());
    let to = form_params
        .iter()
        .find(|(k, _)| k == "to")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| "en".to_string());
    let sign = form_params
        .iter()
        .find(|(k, _)| k == "sign")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();

    let key_id = format!("niutrans:{}", api_key);
    let active = key_enter(&state, &key_id).await;

    let params = verify::NiutransParams {
        api_key,
        q: q.clone(),
        from: from.clone(),
        to: to.clone(),
        sign,
    };

    if let Err(e) = verify::verify_niutrans(&params) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(&q, &to, &from, key_id.clone(), active);
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Azure: Subscription Key header
// ---------------------------------------------------------------------------

pub async fn azure_translate(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let sub_key = headers
        .get("ocp-apim-subscription-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let key_id = format!("azure:{}", sub_key);
    let active = key_enter(&state, &key_id).await;

    if let Err(e) = verify::verify_azure(sub_key) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    // Accept both array format [{"Text":"..."}] and object format {"text":"..."}
    let text = if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
        arr.first()
            .and_then(|v| v.get("Text").or(v.get("text")))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    } else if let Ok(req) = serde_json::from_str::<GenericTranslateRequest>(&body) {
        req.get_text().to_string()
    } else {
        String::new()
    };

    let resp = text_response_with_stats_auto(&text, "en", "auto", key_id.clone(), active);
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Kakao: KakaoAK header
// ---------------------------------------------------------------------------

pub async fn kakao_translate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GenericTranslateRequest>,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let key_id = authorization
        .strip_prefix("KakaoAK ")
        .map(|k| format!("kakao:{}", k))
        .unwrap_or_else(|| "kakao:unknown".to_string());

    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    if let Err(e) = verify::verify_kakao(authorization) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(
        req.get_text(),
        &req.target_lang,
        &req.source_lang,
        key_id.clone(),
        active,
    );
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// OAuth: token endpoint (client_credentials -> JWT)
// ---------------------------------------------------------------------------

pub async fn oauth_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let req = if content_type.contains("application/x-www-form-urlencoded") {
        let mut grant_type = String::new();
        let mut client_id = String::new();
        let mut client_secret = String::new();
        let mut refresh_token = String::new();
        for (key, value) in form_urlencoded::parse(&body) {
            match key.as_ref() {
                "grant_type" => grant_type = value.into_owned(),
                "client_id" => client_id = value.into_owned(),
                "client_secret" => client_secret = value.into_owned(),
                "refresh_token" => refresh_token = value.into_owned(),
                _ => {}
            }
        }
        OAuthTokenRequest {
            grant_type,
            client_id,
            client_secret,
            refresh_token,
        }
    } else {
        match serde_json::from_slice::<OAuthTokenRequest>(&body) {
            Ok(req) => req,
            Err(error) => {
                return (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    Json(json!({
                        "error": "unsupported_content_type",
                        "message": format!("Unsupported OAuth token request body: {}", error),
                    })),
                )
                    .into_response();
            }
        }
    };

    if req.grant_type == "refresh_token" {
        if req.refresh_token.is_empty() || req.refresh_token.contains("bad") || req.refresh_token.contains("invalid") {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "invalid_grant",
                    "error_description": "Invalid or expired refresh token"
                })),
            )
                .into_response();
        }
        let cid = if req.client_id.is_empty() {
            crate::config::OAUTH_CLIENT_ID.to_string()
        } else {
            req.client_id.clone()
        };
        let token = verify::verify_oauth_client_credentials(&cid, crate::config::OAUTH_CLIENT_SECRET).unwrap_or_else(|_| format!("mock-refreshed-{}", cid));
        let ttl = crate::config::oauth_token_ttl_seconds();
        return Json(OAuthTokenResponse {
            access_token: token,
            token_type: "Bearer".into(),
            expires_in: ttl,
            refresh_token: Some(format!("mock-refresh-{}-rotated", cid)),
        })
        .into_response();
    }

    if req.grant_type != "client_credentials" && req.grant_type != "authorization_code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "unsupported_grant_type",
                "error_description": format!("Unsupported grant_type: {}", req.grant_type)
            })),
        )
            .into_response();
    }

    let client_id = req.client_id.clone();
    if client_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "invalid_client",
                "error_description": "client_id must not be empty"
            })),
        )
            .into_response();
    }

    let key_id = format!("oauth:{}", client_id);
    let active = key_enter(&state, &key_id).await;

    let token_result = if oauth_permissive_tokens_enabled()
        && (client_id != crate::config::OAUTH_CLIENT_ID
            || req.client_secret != crate::config::OAUTH_CLIENT_SECRET)
    {
        // Legacy permissive path for explicit local debugging only.
        Ok(format!("mock-token-{}", client_id))
    } else {
        verify::verify_oauth_client_credentials(&req.client_id, &req.client_secret)
    };

    let ttl = crate::config::oauth_token_ttl_seconds();
    let resp = match token_result {
        Ok(token) => Json(OAuthTokenResponse {
            access_token: token,
            token_type: "Bearer".into(),
            expires_in: ttl,
            refresh_token: Some(format!("mock-refresh-{}", client_id)),
        })
        .into_response(),
        Err(e) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "invalid_client",
                "error_description": e.message
            })),
        )
            .into_response(),
    };

    key_exit(&state, &key_id).await;
    let _ = active; // suppress unused warning
    resp
}

pub async fn oauth_authorize(
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let client_id = query.get("client_id").cloned().unwrap_or_default();
    let redirect_uri = query.get("redirect_uri").cloned().unwrap_or_default();
    let state = query.get("state").cloned().unwrap_or_default();
    let code = format!("mock-auth-code-{}", if client_id.is_empty() { "default" } else { &client_id });
    if !redirect_uri.is_empty() {
        let sep = if redirect_uri.contains('?') { "&" } else { "?" };
        let redirect = format!("{}{}code={}&state={}", redirect_uri, sep, code, state);
        (
            StatusCode::FOUND,
            [(axum::http::header::LOCATION, redirect)],
        )
            .into_response()
    } else {
        Json(json!({
            "code": code,
            "state": state
        }))
        .into_response()
    }
}

pub async fn mock_custom_target(
    State(state): State<AppState>,
    Path(target_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let stat_key = format!("custom:{}", &target_name);
    let _ = key_enter(&state, &stat_key).await;

    // Optional status code override via header
    if let Some(status_str) = headers.get("x-mock-status").or_else(|| headers.get("x-mock-fault-status")).and_then(|v| v.to_str().ok()) {
        if let Ok(code) = status_str.parse::<u16>() {
            key_exit(&state, &stat_key).await;
            if let Some(body_override) = headers.get("x-mock-fault-body").and_then(|v| v.to_str().ok()) {
                if let Ok(val) = serde_json::from_str::<Value>(body_override) {
                    return (
                        StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
                        Json(val),
                    ).into_response();
                }
            }
            return (
                StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
                Json(json!({
                    "error": "custom_target_error",
                    "status": code,
                    "target": target_name
                })),
            )
                .into_response();
        }
    }

    // Optional path verification (P5: 错路径失败仿真)
    if target_name == "not-found" || target_name == "404" {
        key_exit(&state, &stat_key).await;
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "target_not_found",
                "message": format!("Custom target '{}' was not found", target_name),
                "target": target_name
            })),
        )
            .into_response();
    }
    if let Some(expected) = headers.get("x-mock-expected-path").and_then(|v| v.to_str().ok()) {
        if !target_name.ends_with(expected.trim_start_matches('/')) {
            key_exit(&state, &stat_key).await;
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": "path_mismatch",
                    "message": format!("Expected path '{}', got '{}'", expected, target_name),
                    "target": target_name
                })),
            )
                .into_response();
        }
    }

    // Optional auth error simulation
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let api_key_hdr = headers.get("api-key").or_else(|| headers.get("x-api-key")).and_then(|v| v.to_str().ok()).unwrap_or("");
    if auth.contains("bad") || auth.contains("invalid") || api_key_hdr.contains("bad") || api_key_hdr.contains("invalid") {
        key_exit(&state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "invalid_api_key",
                "message": "Custom target authorization failed: invalid key provided",
                "target": target_name
            })),
        )
            .into_response();
    }

    // Optional signature verification (P5: 验签算法支持与错签名失败仿真)
    let sig_hdr = headers.get("x-signature").or_else(|| headers.get("sign")).and_then(|v| v.to_str().ok()).unwrap_or("");
    let sign_algo = headers.get("x-mock-sign-algo").and_then(|v| v.to_str().ok()).unwrap_or("");
    if sig_hdr.contains("bad") || sig_hdr.contains("invalid") || sig_hdr == "deadbeef" {
        key_exit(&state, &stat_key).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "signature_verification_failed",
                "message": "Custom target signature verification failed: invalid signature",
                "target": target_name
            })),
        )
            .into_response();
    }
    if sign_algo == "hmac_sha256" {
        let api_key = if !api_key_hdr.is_empty() {
            api_key_hdr.to_string()
        } else if auth.starts_with("Bearer ") {
            auth.trim_start_matches("Bearer ").to_string()
        } else {
            crate::config::HMAC_API_KEY.to_string()
        };
        if sig_hdr.is_empty() {
            key_exit(&state, &stat_key).await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "signature_missing",
                    "message": "x-mock-sign-algo 'hmac_sha256' requires x-signature header",
                    "target": target_name
                })),
            )
                .into_response();
        }
        let salt = headers.get("x-salt").and_then(|v| v.to_str().ok()).unwrap_or("salt123");
        let body_q = String::from_utf8_lossy(&body);
        if let Err(e) = verify::verify_hmac(&verify::HmacParams {
            api_key,
            text: body_q.to_string(),
            salt: salt.to_string(),
            sign: sig_hdr.to_string(),
        }) {
            key_exit(&state, &stat_key).await;
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "signature_verification_failed",
                    "message": e.message,
                    "target": target_name
                })),
            )
                .into_response();
        }
    }

    let body_str = String::from_utf8_lossy(&body);
    let mut text = String::new();
    let mut target = "en".to_string();
    let mut source = "auto".to_string();

    if let Ok(val) = serde_json::from_str::<Value>(&body_str) {
        if let Some(s) = val.get("text").or_else(|| val.get("q")).or_else(|| val.get("prompt")).or_else(|| val.get("input")).and_then(Value::as_str) {
            text = s.to_string();
        } else if let Some(arr) = val.get("text").or_else(|| val.get("q")).and_then(Value::as_array) {
            if let Some(first) = arr.first().and_then(Value::as_str) {
                text = first.to_string();
            }
        }
        if let Some(t) = val.get("target").or_else(|| val.get("target_lang")).or_else(|| val.get("to")).and_then(Value::as_str) {
            target = t.to_string();
        }
        if let Some(s) = val.get("source").or_else(|| val.get("source_lang")).or_else(|| val.get("from")).and_then(Value::as_str) {
            source = s.to_string();
        }
    } else {
        for (k, v) in form_urlencoded::parse(&body) {
            match k.as_ref() {
                "text" | "q" | "prompt" | "input" => text = v.into_owned(),
                "target" | "target_lang" | "to" => target = v.into_owned(),
                "source" | "source_lang" | "from" => source = v.into_owned(),
                _ => {}
            }
        }
    }

    if text.is_empty() {
        text = "Hello world".to_string();
    }
    let translated = translate_text(&text, &target);

    key_exit(&state, &stat_key).await;
    Json(json!({
        "status": "success",
        "code": 0,
        "custom_target": target_name,
        "data": {
            "translation": translated,
            "translated_text": translated,
            "source": source,
            "target": target
        },
        "result": {
            "text": translated,
            "translated_text": translated
        },
        "translation": translated
    }))
    .into_response()
}

fn oauth_permissive_tokens_enabled() -> bool {
    std::env::var("MOCK_OAUTH_PERMISSIVE_TOKENS")
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// OAuth: Bearer token verification (JWT issued by /oauth/token)
// ---------------------------------------------------------------------------

pub async fn oauth_translate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GenericTranslateRequest>,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let key_id = authorization
        .strip_prefix("Bearer ")
        .map(|t| format!("oauth_bearer:{}", t))
        .unwrap_or_else(|| "oauth_bearer:unknown".to_string());

    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    if let Err(e) = verify::verify_oauth_bearer(authorization) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(
        req.get_text(),
        &req.target_lang,
        &req.source_lang,
        key_id.clone(),
        active,
    );
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// JWT Bearer: RSA-SHA256 signed JWT verification
// ---------------------------------------------------------------------------

pub async fn jwt_translate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GenericTranslateRequest>,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let key_id = authorization
        .strip_prefix("Bearer ")
        .map(|t| {
            // Use just the header.payload portion as the key identifier (trim signature).
            let parts: Vec<&str> = t.splitn(3, '.').collect();
            if parts.len() >= 2 {
                format!("jwt:{}.{}", parts[0], parts[1])
            } else {
                format!("jwt:{}", t)
            }
        })
        .unwrap_or_else(|| "jwt:unknown".to_string());

    let active = key_enter(&state, &key_id).await;

    if req.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.delay_ms)).await;
    }

    if let Err(e) = verify::verify_jwt_bearer(authorization) {
        key_exit(&state, &key_id).await;
        return auth_error_response(e);
    }

    let resp = text_response_with_stats_auto(
        req.get_text(),
        &req.target_lang,
        &req.source_lang,
        key_id.clone(),
        active,
    );
    key_exit(&state, &key_id).await;
    resp
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_text_wraps_with_markers() {
        let result = translate_text("Hello World", "en_US");
        assert_eq!(
            result,
            "\u{3010}en_US\u{3011}Hello World\u{3010}/en_US\u{3011}"
        );
    }

    #[test]
    fn test_translate_text_empty_string() {
        let result = translate_text("", "ja_JP");
        assert_eq!(result, "\u{3010}ja_JP\u{3011}\u{3010}/ja_JP\u{3011}");
    }

    #[test]
    fn test_translate_text_html_nodes_only() {
        let html = "<p>Hello <strong>world</strong></p>";
        let result = translate_text(html, "zh-CN");
        assert_eq!(
            result,
            "<p>\u{3010}zh-CN\u{3011}Hello\u{3010}/zh-CN\u{3011} <strong>\u{3010}zh-CN\u{3011}world\u{3010}/zh-CN\u{3011}</strong></p>"
        );
    }

    #[test]
    fn test_translate_media_ref_with_extension() {
        let result = translate_media_ref("https://site.com/wp-content/uploads/photo.jpg", "en_US");
        assert_eq!(
            result,
            "https://site.com/wp-content/uploads/photo-en_US.jpg"
        );
    }

    #[test]
    fn test_translate_media_ref_without_extension() {
        let result = translate_media_ref("readme", "fr_FR");
        assert_eq!(result, "readme-fr_FR");
    }

    #[test]
    fn test_translate_media_ref_multiple_dots() {
        let result = translate_media_ref("archive.tar.gz", "de_DE");
        assert_eq!(result, "archive.tar-de_DE.gz");
    }

    #[test]
    fn test_translate_media_ref_keeps_query_and_fragment() {
        let result = translate_media_ref("https://a.com/p/v.mp4?x=1#cut", "zh-CN");
        assert_eq!(result, "https://a.com/p/v-zh-CN.mp4?x=1#cut");
    }

    #[test]
    fn test_translated_media_url_image() {
        let url = translated_media_url("image");
        assert!(url.ends_with("/media/image-translated.png"));
    }

    #[test]
    fn test_translated_media_url_audio() {
        let url = translated_media_url("audio");
        assert!(url.ends_with("/media/audio-translated.mp3"));
    }

    #[test]
    fn test_translated_media_url_video() {
        let url = translated_media_url("video");
        assert!(url.ends_with("/media/video-translated.mp4"));
    }

    #[test]
    fn test_translated_media_url_document() {
        let url = translated_media_url("document");
        assert!(url.ends_with("/media/document-translated.pdf"));
    }

    #[test]
    fn test_extract_bearer_token_present() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer my-api-key-123".parse().unwrap(),
        );
        assert_eq!(
            extract_bearer_token(&headers),
            Some("my-api-key-123".to_string())
        );
    }

    #[test]
    fn test_extract_bearer_token_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn test_extract_bearer_token_non_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        assert_eq!(extract_bearer_token(&headers), None);
    }
}
