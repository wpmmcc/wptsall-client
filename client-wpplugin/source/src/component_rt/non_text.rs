//! Non-text execution traits and placeholder implementations.
//!
//! This module defines the `NonTextExecutor` trait for media-type-specific
//! execution (image, audio, video, document). Each media type has dedicated
//! input/output types that encode the specific fields relevant to that type.
//!
//! Current state: placeholder implementations that return explicit
//! `NOT_IMPLEMENTED` results. These are NOT mock successes -- they clearly
//! signal that real execution has not occurred, and downstream code treats
//! them as errors (not as silent successes).
//!
//! Future: swap placeholders for real service integrations (e.g., image
//! localization APIs, subtitle translation services) by implementing the
//! trait for concrete executor structs.
#![allow(dead_code)]

use async_trait::async_trait;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Input types -- one per media category
// ---------------------------------------------------------------------------

/// Input for image translation/localization tasks.
#[derive(Debug, Clone)]
pub(crate) struct ImageInput {
    /// The source image URL or file reference.
    pub(crate) source_ref: String,
    /// Optional alt text to translate.
    pub(crate) alt_text: String,
    /// Optional caption to translate.
    pub(crate) caption: String,
    /// Optional title to translate.
    pub(crate) title: String,
    /// Source language code (e.g. "zh_CN").
    pub(crate) source_lang: String,
    /// Target language code (e.g. "en_US").
    pub(crate) target_lang: String,
    /// Raw source payload from the task, for custom field extraction.
    pub(crate) source_payload: Option<Value>,
}

/// Input for video translation tasks.
#[derive(Debug, Clone)]
pub(crate) struct VideoInput {
    /// The source video URL or file reference.
    pub(crate) source_ref: String,
    /// Optional transcript or subtitle text to translate.
    pub(crate) transcript: String,
    /// Optional caption/description to translate.
    pub(crate) caption: String,
    /// Source language code.
    pub(crate) source_lang: String,
    /// Target language code.
    pub(crate) target_lang: String,
    /// Raw source payload.
    pub(crate) source_payload: Option<Value>,
}

/// Input for audio translation tasks.
#[derive(Debug, Clone)]
pub(crate) struct AudioInput {
    /// The source audio URL or file reference.
    pub(crate) source_ref: String,
    /// Optional transcript text to translate.
    pub(crate) transcript: String,
    /// Optional caption/description to translate.
    pub(crate) caption: String,
    /// Source language code.
    pub(crate) source_lang: String,
    /// Target language code.
    pub(crate) target_lang: String,
    /// Raw source payload.
    pub(crate) source_payload: Option<Value>,
}

/// Input for document translation tasks (PDF, DOCX, etc.).
#[derive(Debug, Clone)]
pub(crate) struct DocumentInput {
    /// The source document URL or file reference.
    pub(crate) source_ref: String,
    /// Optional extracted text content to translate.
    pub(crate) text_content: String,
    /// Optional title to translate.
    pub(crate) title: String,
    /// Source language code.
    pub(crate) source_lang: String,
    /// Target language code.
    pub(crate) target_lang: String,
    /// Raw source payload.
    pub(crate) source_payload: Option<Value>,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Result of a non-text execution attempt.
#[derive(Debug, Clone)]
pub(crate) struct NonTextExecutionResult {
    /// Whether the execution succeeded.
    pub(crate) success: bool,
    /// Reference to the translated asset (URL, file path, attachment ID).
    /// Empty when not implemented or on failure.
    pub(crate) translated_ref: String,
    /// Translated text fields (key -> translated value).
    /// For images: alt_text, caption, title.
    /// For video/audio: transcript, caption.
    /// For documents: text_content, title.
    pub(crate) translated_fields: Vec<(String, String)>,
    /// Error code. Empty on success.
    /// Placeholder implementations use "NOT_IMPLEMENTED".
    pub(crate) error_code: String,
    /// Human-readable error message. Empty on success.
    pub(crate) error_message: String,
    /// Identifier of the executor that produced this result.
    pub(crate) executor_id: String,
}

impl NonTextExecutionResult {
    /// Create a NOT_IMPLEMENTED result for a given media type and executor.
    pub(crate) fn not_implemented(media_type: &str, executor_id: &str) -> Self {
        Self {
            success: false,
            translated_ref: String::new(),
            translated_fields: Vec::new(),
            error_code: "NOT_IMPLEMENTED".to_string(),
            error_message: format!(
                "{} execution is not yet implemented (executor={})",
                media_type, executor_id
            ),
            executor_id: executor_id.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Trait definition
// ---------------------------------------------------------------------------

/// Trait for executing non-text translation/localization tasks.
///
/// Each method corresponds to a media type. Implementations can support one
/// or more media types. The default implementations return NOT_IMPLEMENTED,
/// so a concrete executor only needs to override the types it supports.
///
/// Returning NOT_IMPLEMENTED is explicitly NOT a success. Callers must check
/// `result.success` and treat NOT_IMPLEMENTED as a routing/capability gap,
/// not as a completed task.
#[async_trait]
pub(crate) trait NonTextExecutor: Send + Sync {
    /// Executor identifier, used in logs and result attribution.
    fn executor_id(&self) -> &str;

    /// Execute an image translation/localization task.
    async fn execute_image(&self, input: &ImageInput) -> NonTextExecutionResult {
        let _ = input;
        NonTextExecutionResult::not_implemented("image", self.executor_id())
    }

    /// Execute a video translation task.
    async fn execute_video(&self, input: &VideoInput) -> NonTextExecutionResult {
        let _ = input;
        NonTextExecutionResult::not_implemented("video", self.executor_id())
    }

    /// Execute an audio translation task.
    async fn execute_audio(&self, input: &AudioInput) -> NonTextExecutionResult {
        let _ = input;
        NonTextExecutionResult::not_implemented("audio", self.executor_id())
    }

    /// Execute a document translation task.
    async fn execute_document(&self, input: &DocumentInput) -> NonTextExecutionResult {
        let _ = input;
        NonTextExecutionResult::not_implemented("document", self.executor_id())
    }
}

// ---------------------------------------------------------------------------
// Placeholder executor -- returns NOT_IMPLEMENTED for all media types
// ---------------------------------------------------------------------------

/// Placeholder executor that returns NOT_IMPLEMENTED for every media type.
///
/// This is the default executor used when no real service integration is
/// configured. It ensures non-text tasks fail explicitly rather than
/// silently returning mock data.
pub(crate) struct PlaceholderNonTextExecutor;

impl PlaceholderNonTextExecutor {

}

#[async_trait]
impl NonTextExecutor for PlaceholderNonTextExecutor {
    fn executor_id(&self) -> &str {
        "placeholder"
    }
    // All methods use the default NOT_IMPLEMENTED implementations.
}

// ---------------------------------------------------------------------------
// Dispatch helper
// ---------------------------------------------------------------------------

/// Route a non-text task to the appropriate executor method based on the
/// normalized content type.
///
/// Returns `None` if the content type is not a recognized non-text type
/// (e.g. "text" or "mixed" are not dispatched here).
pub(crate) async fn dispatch_non_text(
    executor: &dyn NonTextExecutor,
    media_type: &str,
    source_ref: &str,
    source_text: &str,
    source_lang: &str,
    target_lang: &str,
    source_payload: Option<&Value>,
) -> Option<NonTextExecutionResult> {
    let payload_clone = source_payload.cloned();

    match media_type {
        "image" => {
            let input = ImageInput {
                source_ref: source_ref.to_string(),
                alt_text: extract_payload_field(source_payload, &["alt", "alt_text"])
                    .unwrap_or_default(),
                caption: extract_payload_field(source_payload, &["caption"]).unwrap_or_default(),
                title: extract_payload_field(source_payload, &["title"]).unwrap_or_default(),
                source_lang: source_lang.to_string(),
                target_lang: target_lang.to_string(),
                source_payload: payload_clone,
            };
            Some(executor.execute_image(&input).await)
        }
        "video" => {
            let input = VideoInput {
                source_ref: source_ref.to_string(),
                transcript: extract_payload_field(
                    source_payload,
                    &["transcript", "subtitle", "text"],
                )
                .unwrap_or_else(|| source_text.to_string()),
                caption: extract_payload_field(source_payload, &["caption", "description"])
                    .unwrap_or_default(),
                source_lang: source_lang.to_string(),
                target_lang: target_lang.to_string(),
                source_payload: payload_clone,
            };
            Some(executor.execute_video(&input).await)
        }
        "audio" => {
            let input = AudioInput {
                source_ref: source_ref.to_string(),
                transcript: extract_payload_field(source_payload, &["transcript", "text"])
                    .unwrap_or_else(|| source_text.to_string()),
                caption: extract_payload_field(source_payload, &["caption", "description"])
                    .unwrap_or_default(),
                source_lang: source_lang.to_string(),
                target_lang: target_lang.to_string(),
                source_payload: payload_clone,
            };
            Some(executor.execute_audio(&input).await)
        }
        "document" => {
            let input = DocumentInput {
                source_ref: source_ref.to_string(),
                text_content: extract_payload_field(
                    source_payload,
                    &["text", "content", "text_content"],
                )
                .unwrap_or_else(|| source_text.to_string()),
                title: extract_payload_field(source_payload, &["title"]).unwrap_or_default(),
                source_lang: source_lang.to_string(),
                target_lang: target_lang.to_string(),
                source_payload: payload_clone,
            };
            Some(executor.execute_document(&input).await)
        }
        _ => None,
    }
}

/// Extract a string field from a JSON payload, trying multiple candidate keys.
fn extract_payload_field(payload: Option<&Value>, keys: &[&str]) -> Option<String> {
    let obj = payload?.as_object()?;
    for key in keys {
        if let Some(value) = obj.get(*key).and_then(|v| v.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}



pub(crate) mod recovery;
pub(crate) mod recovery_store;

// ---------------------------------------------------------------------------
// Chunked media upload — WP Protocol v2 endpoints
// ---------------------------------------------------------------------------

use anyhow::Context;

use crate::auth::verify_wp_response_signature_for_plaintext;

/// Configuration for the chunked upload client.
pub(crate) struct ChunkedUploadConfig {
    /// WP API base URL — already includes the route-secret segment, e.g.
    /// `https://blog.example.com/wp-json/wptsall/v2/{secret}/client`.
    pub(crate) wp_base: String,
    /// WP client auth token (`wptc1.…`).
    pub(crate) token: String,
    /// Route secret (kept for callers that pass it separately; not used when
    /// `wp_base` already embeds the secret).
    pub(crate) route_secret: Option<String>,
    /// Worker ID sent in `X-WPTSALL-Worker-ID`.
    pub(crate) worker_id: String,
    /// opus5 A-03 (AF-03): stable WP device identity sent in
    /// `X-WPTSALL-Device-Id`; threaded from the boot-level WorkerConfig,
    /// never resolved from env at request time.
    pub(crate) device_id: String,
    /// Chunk size in bytes (default: 5 MiB).  Zero means use default.
    pub(crate) chunk_size: usize,
}

impl ChunkedUploadConfig {
    /// Effective chunk size: caller-supplied value or 5 MiB.
    fn effective_chunk_size(&self) -> usize {
        if self.chunk_size == 0 {
            5 * 1024 * 1024
        } else {
            self.chunk_size
        }
    }
}

/// Response from `POST /media-upload/init`.
#[derive(serde::Deserialize)]
struct InitResponse {
    upload_id: String,
    /// Server-suggested chunk size (informational; we use our own).
    #[allow(dead_code)]
    chunk_size: Option<usize>,
}

/// Response from `GET /media-upload/status`.
/// WP returns `received_chunks` as an array of chunk indices (e.g. `[0,1,2]`).
#[derive(serde::Deserialize)]
struct StatusResponse {
    #[serde(default)]
    received_chunks: Vec<usize>,
    #[serde(default)]
    total_chunks: usize,
    #[serde(default)]
    missing_chunks: Vec<usize>,
}

fn validate_chunk_status(status: &StatusResponse, expected: usize) -> anyhow::Result<bool> {
    anyhow::ensure!(
        expected > 0 && status.total_chunks == expected,
        "chunk status count does not match this upload"
    );
    let mut seen = std::collections::HashSet::new();
    for index in status.received_chunks.iter().chain(&status.missing_chunks) {
        anyhow::ensure!(
            *index < expected && seen.insert(*index),
            "chunk status has duplicate, overlapping or out-of-range indices"
        );
    }
    anyhow::ensure!(
        seen.len() == expected,
        "chunk status does not account for every chunk"
    );
    Ok(status.missing_chunks.is_empty())
}

/// Final result of a media upload (either single or chunked).
pub(crate) struct MediaUploadResult {
    pub(crate) success: bool,
    pub(crate) attachment_id: i64,
    pub(crate) url: String,
    pub(crate) error: String,
}

impl MediaUploadResult {
    fn ok(attachment_id: i64, url: String) -> Self {
        Self {
            success: true,
            attachment_id,
            url,
            error: String::new(),
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            attachment_id: 0,
            url: String::new(),
            error: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// URL helper
// ---------------------------------------------------------------------------

/// Build a full WP endpoint URL.
///
/// Because `wp_base` already contains the route-secret segment (matching the
/// pattern used throughout `discoverer.rs`), this simply appends the endpoint:
/// `{wp_base}/{endpoint}` — e.g. `.../client/media-upload/init`.
fn make_wp_url(wp_base: &str, endpoint: &str) -> String {
    format!("{}/{}", wp_base.trim_end_matches('/'), endpoint)
}

// ---------------------------------------------------------------------------
// Standard auth headers
// ---------------------------------------------------------------------------

fn auth_headers(token: &str, worker_id: &str, device_id: &str) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(token) {
        map.insert("X-WPTSALL-Client-Token", v);
    }
    // Protocol v2 requires Device-Id on every authenticated client call.
    // opus5 A-03 (AF-03): threaded from the boot-level identity via
    // ChunkedUploadConfig — no env read at request time.
    if !device_id.is_empty() {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(device_id) {
            map.insert("X-WPTSALL-Device-Id", v);
        }
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(worker_id) {
        map.insert("X-WPTSALL-Worker-Id", v);
    }
    map.insert(
        "X-WPTSALL-Protocol-Version",
        reqwest::header::HeaderValue::from_static("2"),
    );
    map.insert(
        "X-Client-Version",
        reqwest::header::HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    if let Ok(value) = crate::auth::build_request_id("media-upload").parse() {
        map.insert(crate::config::REQUEST_ID_HEADER, value);
    }
    if let Some(trace) = crate::auth::current_run_trace_id() {
        if let Ok(value) = trace.parse() {
            map.insert(crate::config::TRACE_ID_HEADER, value);
        }
    }
    map
}

fn add_signature_headers(
    headers: &mut reqwest::header::HeaderMap,
    method: &str,
    url: &str,
    token: &str,
    body: &[u8],
    signed_headers: &[(&str, &str)],
) {
    let (timestamp, sig_nonce, signature) =
        crate::auth::sign_request_with_headers(method, url, token, body, signed_headers);
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&timestamp) {
        headers.insert("X-WPTSALL-Timestamp", v);
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&sig_nonce) {
        headers.insert("X-WPTSALL-Signature-Nonce", v);
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&signature) {
        headers.insert("X-WPTSALL-Signature", v);
    }
}

async fn send_media_request(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    method: reqwest::Method,
    url: &str,
    mut headers: reqwest::header::HeaderMap,
    plaintext: Vec<u8>,
    content_type: &str,
) -> anyhow::Result<Value> {
    let is_get = method == reqwest::Method::GET;
    let body = if crate::auth::requires_transport_encryption(url) {
        let (payload, nonce) = crate::crypto::transport_encrypt(&plaintext, &config.token)?;
        headers.insert("X-WPTSALL-Transport", "encrypted".parse()?);
        headers.insert("X-WPTSALL-Nonce", nonce.parse()?);
        serde_json::to_vec(&serde_json::json!({
            "encrypted_payload":payload,"nonce":nonce,"algorithm":"aes-256-gcm-v1"}))?
    } else {
        plaintext
    };
    let request = client
        .request(method, url)
        .headers(headers)
        .header("Content-Type", content_type);
    let request = if is_get { request } else { request.body(body) };
    let response = request.send().await.map_err(|error| error.without_url())?;
    let status = response.status();
    let signature = response
        .headers()
        .get("X-WPTSALL-Response-Signature")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let transport = response
        .headers()
        .get("X-WPTSALL-Transport")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let body = response.text().await?;
    anyhow::ensure!(status.is_success(), "media transport HTTP {status}");
    crate::auth::process_wp_response(
        &body,
        &transport,
        signature.as_deref(),
        &config.token,
        url,
        true,
    )
    .map_err(|_| {
        anyhow::anyhow!("media response could not be decoded or signature verified; retained")
    })
}

// ---------------------------------------------------------------------------
// Public entry-point
// ---------------------------------------------------------------------------

/// Upload a local file to WP, automatically selecting single vs. chunked
/// protocol based on file size.
///
/// Decision rule:
/// - Files < 50 MiB  → single upload (`POST /media-upload`)
/// - Files ≥ 50 MiB  → chunked upload (`init` / `chunk` / `status` / `complete`)
///
/// Note: older builds forced chunked for audio/video/application/*; that broke
/// Lab/WP installs whose PHP `post_max_size` is below the 5 MiB chunk default.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_file_to_wp(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    local_path: &str,
    filename: &str,
    content_type: &str,
    source_id: i64,
    task_id: i64,
    relation_id: i64,
) -> MediaUploadResult {
    match recovery::upload(
        client,
        config,
        local_path,
        filename,
        content_type,
        source_id,
        task_id,
        relation_id,
        None,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => MediaUploadResult::err(format!("durable media upload: {error:#}")),
    }
}

// ---------------------------------------------------------------------------
// Single upload
// ---------------------------------------------------------------------------

/// Read the entire file and POST it to `POST /media-upload`.
async fn single_upload(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    local_path: &str,
    filename: &str,
    content_type: &str,
    source_id: i64,
    task_id: i64,
    relation_id: i64,
    recovery: Option<&recovery::Checkpoint>,
) -> anyhow::Result<MediaUploadResult> {
    let file_bytes = crate::retained_assets::read_async(local_path, 512 * 1024 * 1024).await?;

    let url = make_wp_url(&config.wp_base, "media-upload");
    let mut headers = auth_headers(&config.token, &config.worker_id, &config.device_id);

    // Additional per-request metadata headers (mirrors submitter.rs conventions).
    if let Ok(v) = reqwest::header::HeaderValue::from_str(filename) {
        headers.insert("X-WPTSALL-Filename", v);
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&task_id.to_string()) {
        headers.insert("X-WPTSALL-Task-ID", v);
    }
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&source_id.to_string()) {
        headers.insert("X-WPTSALL-Source-ID", v);
    }
    if relation_id > 0 {
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&relation_id.to_string()) {
            headers.insert("X-WPTSALL-Relation-ID", v);
        }
    }
    let task_id_str = task_id.to_string();
    let source_id_str = source_id.to_string();
    let relation_id_str = relation_id.to_string();
    let mut signed_headers = vec![
        ("X-WPTSALL-Filename", filename),
        ("X-WPTSALL-Task-ID", task_id_str.as_str()),
        ("X-WPTSALL-Source-ID", source_id_str.as_str()),
    ];
    if relation_id > 0 {
        signed_headers.push(("X-WPTSALL-Relation-ID", relation_id_str.as_str()));
    }
    if let Some(row) = recovery {
        headers.insert("X-WPTSALL-Operation-ID", row.operation_id.parse()?);
        headers.insert("X-WPTSALL-Content-SHA256", row.sha256.parse()?);
        signed_headers.push(("X-WPTSALL-Operation-ID", &row.operation_id));
        signed_headers.push(("X-WPTSALL-Content-SHA256", &row.sha256));
    }
    add_signature_headers(
        &mut headers,
        "POST",
        &url,
        &config.token,
        &file_bytes,
        &signed_headers,
    );

    let response = send_media_request(
        client,
        config,
        reqwest::Method::POST,
        &url,
        headers,
        file_bytes,
        content_type,
    )
    .await?;
    if let Some(row) = recovery {
        verify_media_operation_response(&response, row)?;
    }
    parse_media_upload_result(response)
}

fn verify_media_operation_response(
    value: &Value,
    row: &recovery::Checkpoint,
) -> anyhow::Result<()> {
    let data = value.get("data").unwrap_or(value);
    anyhow::ensure!(
        data["operation_id"].as_str() == Some(&row.operation_id)
            && data["content_sha256"].as_str() == Some(&row.sha256)
            && data["source_id"].as_i64() == Some(row.source_id)
            && data["task_id"].as_i64() == Some(row.task_id)
            && data["relation_id"].as_i64() == Some(row.relation_id),
        "media result does not match the signed operation scope; retained"
    );
    Ok(())
}

async fn single_upload_recoverable(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    row: &recovery::Checkpoint,
) -> anyhow::Result<MediaUploadResult> {
    single_upload(
        client,
        config,
        &row.asset,
        &row.filename,
        &row.content_type,
        row.source_id,
        row.task_id,
        row.relation_id,
        Some(row),
    )
    .await
}

/// Parse the JSON response from `POST /media-upload`.
fn parse_single_upload_response(
    status: reqwest::StatusCode,
    body_text: &str,
    response_sig: Option<&str>,
    token: &str,
    url: &str,
) -> anyhow::Result<MediaUploadResult> {
    if !status.is_success() {
        anyhow::bail!("HTTP {} — {}", status, body_text);
    }

    verify_wp_response_signature_for_plaintext(
        token,
        body_text.as_bytes(),
        response_sig,
        url,
        true,
    )
    .with_context(|| "single upload response signature verification failed")?;
    parse_media_upload_result(serde_json::from_str(body_text)?)
}

fn parse_media_upload_result(value: Value) -> anyhow::Result<MediaUploadResult> {
    #[derive(serde::Deserialize)]
    struct Resp {
        success: bool,
        #[serde(default)]
        error: String,
        #[serde(default)]
        message: String,
        data: Option<RespData>,
        // Some WP endpoints flatten `attachment_id` / `url` at the top level.
        attachment_id: Option<serde_json::Value>,
        url: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct RespData {
        attachment_id: Option<serde_json::Value>,
        url: Option<String>,
    }

    let resp: Resp = serde_json::from_value(value)
        .map_err(|e| anyhow::anyhow!("parse single upload response: {}", e))?;

    if !resp.success {
        anyhow::bail!("upload rejected: {} — {}", resp.error, resp.message);
    }

    // Resolve attachment_id from either `data.attachment_id` or top-level.
    let raw_id = resp
        .data
        .as_ref()
        .and_then(|d| d.attachment_id.as_ref())
        .or(resp.attachment_id.as_ref());

    let attachment_id = raw_id
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or_else(|| anyhow::anyhow!("missing attachment_id in single upload response"))?;
    anyhow::ensure!(
        attachment_id > 0,
        "media upload attachment ID must be positive"
    );

    let url = resp
        .data
        .as_ref()
        .and_then(|d| d.url.clone())
        .or(resp.url)
        .unwrap_or_default();

    Ok(MediaUploadResult::ok(attachment_id, url))
}

// ---------------------------------------------------------------------------
// Chunked upload (4-step protocol)
// ---------------------------------------------------------------------------

const CHUNK_RETRY_MAX: usize = 3;
const CHUNK_RETRY_DELAY_MS: u64 = 1_000;

// ---------------------------------------------------------------------------
// Step 1 — Init
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn init_upload(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    filename: &str,
    total_size: u64,
    chunk_count: usize,
    content_type: &str,
    source_id: i64,
    task_id: i64,
    relation_id: i64,
    recovery: Option<&recovery::Checkpoint>,
) -> anyhow::Result<String> {
    let url = make_wp_url(&config.wp_base, "media-upload/init");
    let mut headers = auth_headers(&config.token, &config.worker_id, &config.device_id);

    let mut body = serde_json::json!({
        "filename":     filename,
        "total_size":   total_size,
        "chunk_count":  chunk_count,
        "content_type": content_type,
        "source_id":    source_id,
        "task_id":      task_id,
        "relation_id":  relation_id,
    });
    if let Some(row) = recovery {
        body["operation_id"] = serde_json::json!(row.operation_id);
        body["content_sha256"] = serde_json::json!(row.sha256);
        body["chunk_size"] = serde_json::json!(row.chunk_size);
    }
    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| anyhow::anyhow!("serialize init body: {}", e))?;
    add_signature_headers(&mut headers, "POST", &url, &config.token, &body_bytes, &[]);
    let response = send_media_request(
        client,
        config,
        reqwest::Method::POST,
        &url,
        headers,
        body_bytes,
        "application/json",
    )
    .await?;

    #[derive(serde::Deserialize)]
    struct Wrapper {
        success: bool,
        #[serde(default)]
        error: String,
        data: Option<InitResponse>,
        // Flat layout fallback.
        upload_id: Option<String>,
    }

    let wrapper: Wrapper = serde_json::from_value(response)
        .map_err(|e| anyhow::anyhow!("parse init response: {}", e))?;

    if !wrapper.success {
        anyhow::bail!("init upload rejected: {}", wrapper.error);
    }

    let upload_id = wrapper
        .data
        .map(|d| d.upload_id)
        .or(wrapper.upload_id)
        .ok_or_else(|| anyhow::anyhow!("init response missing upload_id"))?;

    anyhow::ensure!(
        !upload_id.trim().is_empty(),
        "init response has empty upload_id"
    );
    reqwest::header::HeaderValue::from_str(&upload_id)
        .context("init response upload_id cannot be used as a header")?;
    Ok(upload_id)
}

async fn init_upload_recoverable(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    row: &recovery::Checkpoint,
) -> anyhow::Result<String> {
    init_upload(
        client,
        config,
        &row.filename,
        row.size,
        usize::try_from(row.size)?.div_ceil(row.chunk_size),
        &row.content_type,
        row.source_id,
        row.task_id,
        row.relation_id,
        Some(row),
    )
    .await
}

// ---------------------------------------------------------------------------
// Step 2 — Upload only verified missing chunks
// ---------------------------------------------------------------------------

/// Read one chunk from the file and POST it to `POST /media-upload/chunk`.
/// Retries up to `CHUNK_RETRY_MAX` times on failure.
async fn upload_single_chunk(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    local_path: &str,
    upload_id: &str,
    chunk_index: usize,
    chunk_size: usize,
) -> anyhow::Result<()> {
    // Convert to Bytes immediately so that .clone() inside the retry loop is an
    // O(1) reference-count increment instead of an O(n) Vec heap copy.
    // bytes::Bytes::clone() is cheap regardless of chunk size.
    let chunk_bytes: bytes::Bytes =
        bytes::Bytes::from(read_chunk(local_path, chunk_index, chunk_size).await?);
    let url = make_wp_url(&config.wp_base, "media-upload/chunk");
    let mut last_err = String::new();
    for attempt in 0..CHUNK_RETRY_MAX {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(CHUNK_RETRY_DELAY_MS)).await;
        }
        let mut headers = auth_headers(&config.token, &config.worker_id, &config.device_id);
        headers.insert("X-WPTSALL-Upload-ID", upload_id.parse()?);
        let index = chunk_index.to_string();
        headers.insert("X-WPTSALL-Chunk-Index", index.parse()?);
        add_signature_headers(
            &mut headers,
            "POST",
            &url,
            &config.token,
            &chunk_bytes,
            &[
                ("X-WPTSALL-Upload-ID", upload_id),
                ("X-WPTSALL-Chunk-Index", &index),
            ],
        );
        let response = match send_media_request(
            client,
            config,
            reqwest::Method::POST,
            &url,
            headers,
            chunk_bytes.to_vec(),
            "application/octet-stream",
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                last_err = format!("send error: {error}");
                continue;
            }
        };
        anyhow::ensure!(response["success"] == true, "chunk upload rejected");
        return Ok(());
    }
    anyhow::bail!("chunk {chunk_index} upload failed after {CHUNK_RETRY_MAX} attempts: {last_err}")
}

/// Read exactly one chunk from a local file at the given chunk index.
async fn read_chunk(
    local_path: &str,
    chunk_index: usize,
    chunk_size: usize,
) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(chunk_size > 0, "chunk size must be positive");
    let offset = chunk_index
        .checked_mul(chunk_size)
        .and_then(|offset| u64::try_from(offset).ok())
        .ok_or_else(|| anyhow::anyhow!("chunk offset overflow"))?;
    crate::retained_assets::range_async(local_path, offset, chunk_size, 512 * 1024 * 1024).await
}

// ---------------------------------------------------------------------------
// Step 4 — Complete
// ---------------------------------------------------------------------------

async fn complete_upload(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    row: &recovery::Checkpoint,
) -> anyhow::Result<MediaUploadResult> {
    let upload_id = row
        .upload_id
        .as_deref()
        .context("media session identity missing")?;
    let url = make_wp_url(&config.wp_base, "media-upload/complete");
    let mut headers = auth_headers(&config.token, &config.worker_id, &config.device_id);

    let body = serde_json::json!({ "upload_id": upload_id });
    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| anyhow::anyhow!("serialize complete body: {}", e))?;
    add_signature_headers(&mut headers, "POST", &url, &config.token, &body_bytes, &[]);
    let response = send_media_request(
        client,
        config,
        reqwest::Method::POST,
        &url,
        headers,
        body_bytes,
        "application/json",
    )
    .await?;
    verify_media_operation_response(&response, row)?;
    parse_media_upload_result(response)
}
