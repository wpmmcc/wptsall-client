use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use reqwest::Client;
use serde_json::Value;
use url::Url;

fn validate_translation_callback_ack(value: Value) -> anyhow::Result<Value> {
    let success = value
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !success {
        return Err(anyhow!("translation callback ack missing success=true"));
    }

    let queued = value
        .get("queued")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sync_task_id = value
        .get("sync_task_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let result_id = value.get("result_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if result_id <= 0 {
        return Err(anyhow!("translation callback ack missing result_id"));
    }
    if queued && sync_task_id <= 0 {
        return Err(anyhow!(
            "translation callback ack queued=true but sync_task_id missing"
        ));
    }
    if value.get("protocol").is_none() {
        return Err(anyhow!("translation callback ack missing protocol"));
    }
    Ok(value)
}

fn validate_i18n_callback_ack(value: Value) -> anyhow::Result<Value> {
    let success = value
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !success {
        return Err(anyhow!("i18n callback ack missing success=true"));
    }

    let queued = value
        .get("queued")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sync_task_id = value
        .get("sync_task_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let result_id = value.get("result_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if result_id <= 0 {
        return Err(anyhow!("i18n callback ack missing result_id"));
    }
    if queued && sync_task_id <= 0 {
        return Err(anyhow!(
            "i18n callback ack queued=true but sync_task_id missing"
        ));
    }
    Ok(value)
}
use serde_json::json;

use crate::auth::{
    build_request_id, verify_wp_response_signature_for_plaintext,
    wp_post_with_transport_and_headers,
};
use crate::component_rt::non_text::{
    upload_file_to_wp as upload_file_chunked_to_wp, ChunkedUploadConfig,
};
use crate::config::REQUEST_ID_HEADER;
use crate::logging::log_event;
use crate::types::*;

fn parse_base64_data_url(data_url: &str) -> Option<(String, Vec<u8>)> {
    let s = data_url.trim();
    let rest = s.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let (meta, payload) = rest.split_at(comma);
    let payload = payload.trim_start_matches(',').trim();
    if payload.is_empty() {
        return None;
    }

    let mut content_type = "application/octet-stream".to_string();
    let mut is_b64 = false;
    for part in meta.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        if part.eq_ignore_ascii_case("base64") {
            is_b64 = true;
        } else if !part.contains('=') {
            content_type = part.to_string();
        }
    }
    if !is_b64 {
        return None;
    }

    let ct = content_type
        .split(';')
        .next()
        .unwrap_or(&content_type)
        .trim();
    let bytes = BASE64_STANDARD.decode(payload).ok()?;
    Some((ct.to_string(), bytes))
}

fn looks_like_base64_blob(raw: &str) -> bool {
    let s = raw.trim();
    if s.len() < 128 {
        return false;
    }
    if s.chars().any(char::is_whitespace) {
        return false;
    }
    // Strict standard base64 charset only.
    s.is_ascii()
        && s.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '='))
}

/// Attachment lifecycle source copies may only be read from the configured
/// WordPress origin.  This keeps a malicious attachment URL/meta value from
/// turning the Client into a generic SSRF downloader while retaining normal
/// multisite/Lab URLs (including a configured private development origin).
fn is_configured_wp_origin(wp_base: &str, candidate: &str) -> bool {
    let Ok(base) = Url::parse(wp_base.trim()) else {
        return false;
    };
    let Ok(url) = Url::parse(candidate.trim()) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && base.scheme().eq_ignore_ascii_case(url.scheme())
        && base.host_str().map(str::to_ascii_lowercase)
            == url.host_str().map(str::to_ascii_lowercase)
        && base.port_or_known_default() == url.port_or_known_default()
        && url.username().is_empty()
        && url.password().is_none()
}

fn sniff_content_type_and_ext(bytes: &[u8]) -> (String, &'static str) {
    if bytes.len() >= 5 && &bytes[0..5] == b"%PDF-" {
        return ("application/pdf".to_string(), "pdf");
    }
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        return ("image/png".to_string(), "png");
    }
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return ("image/jpeg".to_string(), "jpg");
    }
    if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        return ("image/gif".to_string(), "gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return ("image/webp".to_string(), "webp");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        return ("audio/wav".to_string(), "wav");
    }
    if bytes.len() >= 3 && &bytes[0..3] == b"ID3" {
        return ("audio/mpeg".to_string(), "mp3");
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0 {
        // MPEG audio frame sync
        return ("audio/mpeg".to_string(), "mp3");
    }
    if bytes.len() >= 12 {
        // ISO BMFF / MP4 family: "....ftyp"
        if &bytes[4..8] == b"ftyp" {
            return ("video/mp4".to_string(), "mp4");
        }
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"PK\x03\x04" {
        // Zip container (docx/pptx/xlsx)
        return ("application/zip".to_string(), "zip");
    }
    ("application/octet-stream".to_string(), "bin")
}

/// Canonical (and sole) result submission path (data-driven, callback-based).
///
/// Submits translated content directly via the `/wptsall/v2/translation-callback`
/// endpoint using `relation_id` + `object_ref`. This is the only submission path
/// used by `executor::submit_result_with_compensation`, decoupling the write-back
/// from the WP task lifecycle and enabling direct content routing.
///
/// The legacy `/tasks/{id}/result` endpoint has been removed.
pub(crate) async fn send_translation_callback(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    idempotency_key: &str,
    payload: &crate::types::TranslationCallbackPayload,
    _route_secret: Option<&str>,
) -> anyhow::Result<Value> {
    let callback_url = format!("{}/translation-callback", wp_base.trim_end_matches('/'));
    let body = serde_json::to_value(payload)
        .with_context(|| "serialize translation callback payload failed")?;
    let headers: Vec<(&str, &str)> = vec![("Idempotency-Key", idempotency_key)];
    let ack = wp_post_with_transport_and_headers(
        client,
        &callback_url,
        token,
        worker_id,
        &body,
        &headers,
    )
    .await
    .with_context(|| {
        format!(
            "translation callback request failed (relation_id={})",
            payload.relation_id
        )
    })?;

    validate_translation_callback_ack(ack)
}

/// Submit translated language-pack entries via Path B callback (v1.3.0+).
///
/// Sends a batch of `{ entry_id, msgstr }` entries for theme_i18n / plugin_i18n / config_i18n
/// business lines.  WP writes each `msgstr` back to `template_entries`.
pub(crate) async fn send_i18n_translation_callback(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    idempotency_key: &str,
    payload: &crate::types::I18nCallbackPayload,
    _route_secret: Option<&str>,
) -> anyhow::Result<Value> {
    let callback_url = format!("{}/translation-callback", wp_base.trim_end_matches('/'));
    let body =
        serde_json::to_value(payload).with_context(|| "serialize i18n callback payload failed")?;
    let headers: Vec<(&str, &str)> = vec![("Idempotency-Key", idempotency_key)];
    let ack = wp_post_with_transport_and_headers(
        client,
        &callback_url,
        token,
        worker_id,
        &body,
        &headers,
    )
    .await
    .with_context(|| {
        format!(
            "i18n translation callback failed (relation_id={}, entries={})",
            payload.relation_id,
            payload.entries.len()
        )
    })?;

    validate_i18n_callback_ack(ack)
}

/// Upload a media file binary to WP's media-upload endpoint.
///
/// Returns the attachment_id on success.
///
/// NOTE: Transport encryption (AES-GCM) is intentionally skipped for binary
/// media uploads. The transport encryption layer wraps JSON payloads, which is
/// incompatible with raw binary data. In production, HTTPS already encrypts the
/// binary payload end-to-end. For HTTP (development only), the binary travels
/// in cleartext — the same as any other media download over HTTP.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_media_to_wp(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    task_id: i64,
    relation_id: u64,
    source_id: u64,
    filename: &str,
    content_type: &str,
    file_data: Vec<u8>,
    log_file: &str,
    _route_secret: Option<&str>,
) -> anyhow::Result<u64> {
    let upload_url = format!("{}/media-upload", wp_base.trim_end_matches('/'));

    let request_id = build_request_id("media-upload");
    let is_plain_http = upload_url.trim().to_lowercase().starts_with("http://");
    let task_id_str = task_id.to_string();
    let relation_id_str = relation_id.to_string();
    let source_id_str = source_id.to_string();
    let signed_headers: Vec<(&str, &str)> = vec![
        ("X-WPTSALL-Task-ID", task_id_str.as_str()),
        ("X-WPTSALL-Relation-ID", relation_id_str.as_str()),
        ("X-WPTSALL-Source-ID", source_id_str.as_str()),
        ("X-WPTSALL-Filename", filename),
    ];
    let (timestamp, sig_nonce, signature) = crate::auth::sign_request_with_headers(
        "POST",
        &upload_url,
        token,
        &file_data,
        &signed_headers,
    );
    if is_plain_http {
        let _ = log_event(
            log_file,
            "warning",
            "media.upload_plain_http",
            json!({
                "url": upload_url,
                "reason": "Binary upload over plain HTTP without transport encryption"
            }),
        );
    }

    let request = client
        .post(&upload_url)
        .header("X-WPTSALL-Protocol-Version", "2")
        .header("X-WPTSALL-Client-Token", token)
        // Bind this authenticated upload to the same stable device identity
        // as JSON discovery/callback requests.  Without this, a device token
        // would work for callbacks but fail for media uploads.
        .header(
            "X-WPTSALL-Device-Id",
            crate::config::env_or("WPTSALL_WP_DEVICE_ID", worker_id),
        )
        .header("X-WPTSALL-Worker-Id", worker_id)
        .header("X-Client-Version", env!("CARGO_PKG_VERSION"))
        .header("X-WPTSALL-Timestamp", timestamp)
        .header("X-WPTSALL-Signature-Nonce", sig_nonce)
        .header("X-WPTSALL-Signature", signature)
        .header("X-WPTSALL-Task-ID", task_id_str)
        .header("X-WPTSALL-Relation-ID", relation_id_str)
        .header("X-WPTSALL-Source-ID", source_id_str)
        .header("X-WPTSALL-Filename", filename)
        .header("Content-Type", content_type)
        .header(REQUEST_ID_HEADER, request_id);
    let request = request.body(file_data);

    let response = request.send().await.with_context(|| {
        format!(
            "media upload request failed (task_id={}, source_id={})",
            task_id, source_id
        )
    })?;
    let status = response.status();
    let response_sig = response
        .headers()
        .get("X-WPTSALL-Response-Signature")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body_text = response.text().await.with_context(|| {
        format!(
            "media upload response read failed (task_id={}, source_id={})",
            task_id, source_id
        )
    })?;

    if !status.is_success() {
        anyhow::bail!("media upload failed: HTTP {} — {}", status, body_text);
    }

    verify_wp_response_signature_for_plaintext(
        token,
        body_text.as_bytes(),
        response_sig.as_deref(),
        &upload_url,
        true,
    )
    .with_context(|| {
        format!(
            "media upload response signature verification failed (task_id={}, source_id={})",
            task_id, source_id
        )
    })?;

    #[derive(serde::Deserialize)]
    struct MediaUploadResponse {
        success: bool,
        attachment_id: Option<u64>,
        #[serde(default)]
        error: String,
        #[serde(default)]
        message: String,
    }

    let resp: MediaUploadResponse = serde_json::from_str(&body_text)
        .with_context(|| "failed to parse media upload response")?;

    if !resp.success {
        anyhow::bail!("media upload rejected: {} — {}", resp.error, resp.message);
    }

    let attachment_id = resp
        .attachment_id
        .ok_or_else(|| anyhow::anyhow!("media upload response missing attachment_id"))?;

    let _ = log_event(
        log_file,
        "info",
        "media.upload_ok",
        json!({
            "api_base_url": wp_base,
            "task_id": task_id,
            "source_id": source_id,
            "filename": filename,
            "attachment_id": attachment_id
        }),
    );

    Ok(attachment_id)
}

#[allow(clippy::too_many_arguments)]
async fn upload_media_via_best_path(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    task_id: i64,
    relation_id: u64,
    source_id: u64,
    filename: &str,
    content_type: &str,
    file_data: Vec<u8>,
    log_file: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<u64> {
    const SINGLE_UPLOAD_LIMIT: usize = 50 * 1024 * 1024;
    let use_chunked = file_data.len() >= SINGLE_UPLOAD_LIMIT
        || content_type.starts_with("video/")
        || content_type.starts_with("audio/")
        || content_type == "application/pdf"
        || content_type.starts_with("application/");

    if !use_chunked {
        return upload_media_to_wp(
            client,
            wp_base,
            token,
            worker_id,
            task_id,
            relation_id,
            source_id,
            filename,
            content_type,
            file_data,
            log_file,
            route_secret,
        )
        .await;
    }

    let data_dir = std::env::var("WPTSALL_DATA_DIR")
        .unwrap_or_else(|_| crate::config::DEFAULT_DATA_DIR.to_string());
    let temp_dir = format!("{}/tmp-media-upload", data_dir);
    std::fs::create_dir_all(&temp_dir)
        .with_context(|| format!("create temp media dir failed: {}", temp_dir))?;
    let temp_path = format!("{}/{}-{}", temp_dir, source_id, filename);
    std::fs::write(&temp_path, &file_data)
        .with_context(|| format!("write temp media file failed: {}", temp_path))?;

    let config = ChunkedUploadConfig {
        wp_base: wp_base.to_string(),
        token: token.to_string(),
        route_secret: route_secret.map(|s| s.to_string()),
        worker_id: worker_id.to_string(),
        chunk_size: 0,
    };
    let result = upload_file_chunked_to_wp(
        client,
        &config,
        &temp_path,
        filename,
        content_type,
        i64::try_from(source_id).unwrap_or(0),
        task_id,
        i64::try_from(relation_id).unwrap_or(0),
    )
    .await;
    let _ = std::fs::remove_file(&temp_path);

    if result.success && result.attachment_id > 0 {
        Ok(u64::try_from(result.attachment_id).unwrap_or(0))
    } else {
        Err(anyhow!("chunked media upload failed: {}", result.error))
    }
}

/// Download translated media from component result URLs and upload binary data
/// to WP's media-upload endpoint. Mutates `payload.media_mappings` in place:
/// successful uploads replace `translated_ref` with `attachment_id`.
///
/// On download or upload failure for individual media items, the mapping is left
/// unchanged (retaining `translated_ref` for fallback / manual queue).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_pending_media(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    task_id: i64,
    relation_id: u64,
    payload: &mut TranslationCallbackPayload,
    log_file: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<()> {
    for mapping in &mut payload.media_mappings {
        if mapping.translated_ref.is_empty() || mapping.attachment_id.is_some() {
            continue;
        }

        let raw_ref = mapping.translated_ref.trim();

        // Inline base64 output (data url): decode and upload directly.
        if let Some((ct, bytes)) = parse_base64_data_url(raw_ref) {
            let (sniff_ct, ext) = sniff_content_type_and_ext(&bytes);
            let content_type = if ct.is_empty() || ct == "application/octet-stream" {
                sniff_ct
            } else {
                ct
            };
            let filename = format!("translated_{}.{}", mapping.source_id, ext);

            match upload_media_via_best_path(
                client,
                wp_base,
                token,
                worker_id,
                task_id,
                relation_id,
                mapping.source_id,
                &filename,
                &content_type,
                bytes,
                log_file,
                route_secret,
            )
            .await
            {
                Ok(att_id) => {
                    mapping.attachment_id = Some(att_id);
                    mapping.translated_ref = String::new();
                }
                Err(err) => {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "media.upload_failed",
                        json!({
                            "source_id": mapping.source_id,
                            "ref_kind": "data_url",
                            "error": format!("{:#}", err)
                        }),
                    );
                }
            }
            continue;
        }

        // Inline base64 output (raw): best-effort decode + sniff + upload.
        if looks_like_base64_blob(raw_ref) {
            if let Ok(bytes) = BASE64_STANDARD.decode(raw_ref) {
                let (content_type, ext) = sniff_content_type_and_ext(&bytes);
                let filename = format!("translated_{}.{}", mapping.source_id, ext);

                match upload_media_via_best_path(
                    client,
                    wp_base,
                    token,
                    worker_id,
                    task_id,
                    relation_id,
                    mapping.source_id,
                    &filename,
                    &content_type,
                    bytes,
                    log_file,
                    route_secret,
                )
                .await
                {
                    Ok(att_id) => {
                        mapping.attachment_id = Some(att_id);
                        mapping.translated_ref = String::new();
                    }
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "media.upload_failed",
                            json!({
                                "source_id": mapping.source_id,
                                "ref_kind": "base64",
                                "error": format!("{:#}", err)
                            }),
                        );
                    }
                }
                continue;
            }
        }

        // Local file output: `file://...` or absolute path.
        let local_path = if let Some(rest) = raw_ref.strip_prefix("file://") {
            Some(rest)
        } else if std::path::Path::new(raw_ref).is_absolute() {
            Some(raw_ref)
        } else {
            None
        };
        if let Some(path_str) = local_path {
            let path = std::path::PathBuf::from(path_str);
            if path.exists() && path.is_file() {
                let file_data = match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "media.local_read_failed",
                            json!({
                                "path": path.display().to_string(),
                                "source_id": mapping.source_id,
                                "error": format!("{:#}", err)
                            }),
                        );
                        continue;
                    }
                };

                let (content_type, ext) = sniff_content_type_and_ext(&file_data);
                let mut filename = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if filename.trim().is_empty() {
                    filename = format!("translated_{}.{}", mapping.source_id, ext);
                } else if !filename.contains('.') {
                    filename = format!("{}.{}", filename, ext);
                }

                match upload_media_via_best_path(
                    client,
                    wp_base,
                    token,
                    worker_id,
                    task_id,
                    relation_id,
                    mapping.source_id,
                    &filename,
                    &content_type,
                    file_data,
                    log_file,
                    route_secret,
                )
                .await
                {
                    Ok(att_id) => {
                        let should_cleanup = raw_ref.starts_with("file://");
                        mapping.attachment_id = Some(att_id);
                        mapping.translated_ref = String::new();
                        // Best-effort cleanup for temp outputs.
                        if should_cleanup {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "media.upload_failed",
                            json!({
                                "source_id": mapping.source_id,
                                "ref_kind": "local_file",
                                "path": path.display().to_string(),
                                "error": format!("{:#}", err)
                            }),
                        );
                    }
                }
                continue;
            }
        }

        let url = mapping.translated_ref.clone();

        if mapping.source_copy && !is_configured_wp_origin(wp_base, &url) {
            let _ = log_event(
                log_file,
                "warning",
                "media.source_copy_origin_rejected",
                json!({ "source_id": mapping.source_id, "url": url }),
            );
            continue;
        }

        // Download from translated_ref URL
        let download_resp = match client.get(&url).send().await {
            Ok(resp) => resp,
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "media.download_failed",
                    json!({
                        "url": url,
                        "source_id": mapping.source_id,
                        "error": format!("{:#}", err)
                    }),
                );
                continue; // Skip this media — it will go to manual queue via translated_ref
            }
        };

        if !download_resp.status().is_success() {
            let _ = log_event(
                log_file,
                "warning",
                "media.download_failed",
                json!({
                    "url": url,
                    "source_id": mapping.source_id,
                    "status": download_resp.status().as_u16()
                }),
            );
            continue;
        }

        const MAX_MEDIA_SIZE: u64 = 500 * 1024 * 1024; // 500 MB
        if let Some(content_length) = download_resp.content_length() {
            if content_length > MAX_MEDIA_SIZE {
                let _ = log_event(
                    log_file,
                    "warning",
                    "media.download_too_large",
                    json!({
                        "url": url,
                        "source_id": mapping.source_id,
                        "size": content_length,
                        "max": MAX_MEDIA_SIZE
                    }),
                );
                continue;
            }
        }

        let content_type = download_resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let file_data = match download_resp.bytes().await {
            Ok(bytes) => bytes.to_vec(),
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "media.download_read_failed",
                    json!({
                        "url": url,
                        "source_id": mapping.source_id,
                        "error": format!("{:#}", err)
                    }),
                );
                continue;
            }
        };

        if file_data.len() as u64 > MAX_MEDIA_SIZE {
            let _ = log_event(
                log_file,
                "warning",
                "media.download_exceeded_limit",
                json!({
                    "url": url,
                    "source_id": mapping.source_id,
                    "size": file_data.len(),
                    "max": MAX_MEDIA_SIZE
                }),
            );
            continue;
        }

        let filename = url
            .rsplit('/')
            .next()
            .unwrap_or("translated_media")
            .to_string();

        // Upload to WP
        match upload_media_via_best_path(
            client,
            wp_base,
            token,
            worker_id,
            task_id,
            relation_id,
            mapping.source_id,
            &filename,
            &content_type,
            file_data,
            log_file,
            route_secret,
        )
        .await
        {
            Ok(att_id) => {
                mapping.attachment_id = Some(att_id);
                mapping.translated_ref = String::new();
            }
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "media.upload_failed",
                    json!({
                        "source_id": mapping.source_id,
                        "url": url,
                        "error": format!("{:#}", err)
                    }),
                );
                // Keep translated_ref for fallback / manual queue
            }
        }
    }
    Ok(())
}

pub(crate) fn retry_with_backoff<T, F, Fut>(
    label: &str,
    task_id: i64,
    log_file: &str,
    worker_config: &WorkerConfig,
    mut op: F,
) -> impl std::future::Future<Output = anyhow::Result<T>>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let max_attempts = worker_config.retry_max.saturating_add(1);
    let retry_base_ms = worker_config.retry_base_ms;
    let retry_max_ms = worker_config.retry_max_ms;
    let label = label.to_string();
    let log_file = log_file.to_string();
    async move {
        let mut attempt = 1u32;
        loop {
            match op(attempt).await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let retryable = is_retryable_error(&err);
                    if !retryable || attempt >= max_attempts {
                        return Err(err);
                    }

                    let base_backoff_ms = compute_backoff_ms(attempt, retry_base_ms, retry_max_ms);
                    let retry_after_ms = parse_retry_after_ms_from_error(&err);
                    let backoff_ms = retry_after_ms
                        .map(|retry_after| retry_after.max(base_backoff_ms))
                        .unwrap_or(base_backoff_ms);
                    let _ = log_event(
                        &log_file,
                        "warning",
                        "task.retry_scheduled",
                        json!({
                            "task_id": task_id,
                            "label": label,
                            "attempt": attempt,
                            "max_attempts": max_attempts,
                            "base_backoff_ms": base_backoff_ms,
                            "retry_after_ms": retry_after_ms,
                            "backoff_ms": backoff_ms,
                            "error": format!("{:#}", err)
                        }),
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }
}

pub(crate) fn compute_backoff_ms(attempt: u32, base_ms: u64, max_ms: u64) -> u64 {
    let cap_exp = attempt.saturating_sub(1).min(16);
    let factor = 1u64 << cap_exp;
    base_ms.saturating_mul(factor).min(max_ms)
}

fn parse_retry_after_ms_from_error(err: &anyhow::Error) -> Option<u64> {
    let msg = err.to_string().to_lowercase();
    for key in ["retry_after_ms=", "retry-after-ms="] {
        if let Some(pos) = msg.find(key) {
            let rest = &msg[pos + key.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u64>() {
                if v > 0 {
                    return Some(v);
                }
            }
        }
    }
    for key in ["retry_after=", "retry-after="] {
        if let Some(pos) = msg.find(key) {
            let rest = &msg[pos + key.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u64>() {
                if v > 0 {
                    return Some(v.saturating_mul(1000));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_source_copy_requires_configured_wordpress_origin() {
        assert!(is_configured_wp_origin(
            "http://127.0.0.1:9083/wp-json/wptsall/v2/test/client",
            "http://127.0.0.1:9083/wp-content/uploads/2026/08/source.jpg"
        ));
        assert!(!is_configured_wp_origin(
            "https://example.com/wp-json/wptsall/v2/test/client",
            "https://attacker.example/wp-content/uploads/source.jpg"
        ));
        assert!(!is_configured_wp_origin(
            "https://example.com/wp-json/wptsall/v2/test/client",
            "https://user:pass@example.com/wp-content/uploads/source.jpg"
        ));
    }

    // -----------------------------------------------------------------------
    // is_retryable_error (m3 audit fix: tightened retry classification)
    // -----------------------------------------------------------------------

    #[test]
    fn retryable_timeout_errors() {
        let cases = vec![
            "operation timed out",
            "request timeout while waiting for response",
            "connection timed out after 30s",
        ];
        for msg in cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(is_retryable_error(&err), "expected retryable for: {}", msg);
        }
    }

    #[test]
    fn retryable_connection_errors() {
        let cases = vec![
            "connection refused",
            "connection reset by peer",
            "connection closed before response",
            "connection aborted unexpectedly",
        ];
        for msg in cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(is_retryable_error(&err), "expected retryable for: {}", msg);
        }
    }

    #[test]
    fn retryable_server_errors() {
        let cases = vec![
            "HTTP status=429 Too Many Requests",
            "HTTP status 500 Internal Server Error",
            "server returned status=502",
            "status 503 Service Unavailable",
            "upstream status=504 Gateway Timeout",
        ];
        for msg in cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(is_retryable_error(&err), "expected retryable for: {}", msg);
        }
    }

    #[test]
    fn non_retryable_client_errors() {
        let cases = vec![
            "HTTP status=400 Bad Request",
            "HTTP status=401 Unauthorized",
            "HTTP status=403 Forbidden",
            "HTTP status=404 Not Found",
            "HTTP status=422 Unprocessable Entity",
            "invalid JSON in response body",
            "missing required field",
        ];
        for msg in cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(
                !is_retryable_error(&err),
                "expected NOT retryable for: {}",
                msg
            );
        }
    }

    #[test]
    fn non_retryable_generic_connection_word() {
        // "connection" alone (without refused/reset/closed/aborted) should NOT be retryable
        let err = anyhow::anyhow!("bad connection pool state");
        assert!(
            !is_retryable_error(&err),
            "generic 'connection' should not be retryable"
        );
    }

    #[test]
    fn non_retryable_request_failed() {
        // "request failed" alone should NOT be retryable (too broad)
        let err = anyhow::anyhow!("request failed: invalid payload");
        assert!(
            !is_retryable_error(&err),
            "'request failed' should not be retryable"
        );
    }

    // -----------------------------------------------------------------------
    // compute_backoff_ms
    // -----------------------------------------------------------------------

    #[test]
    fn test_backoff_cap_respected() {
        // When base_ms > max_ms, the result must still be <= max_ms (the cap).
        // Previously `.min(max_ms.max(base_ms))` would use base_ms as the cap
        // when base_ms > max_ms, causing the cap to be silently raised.
        let result = compute_backoff_ms(0, 6000, 5000);
        assert!(
            result <= 5000,
            "backoff exceeded max_ms cap: got {result}, expected <= 5000"
        );

        // Verify cap is never exceeded for any attempt count.
        for attempt in 0..=20 {
            let ms = compute_backoff_ms(attempt, 6000, 5000);
            assert!(
                ms <= 5000,
                "attempt={attempt}: backoff {ms} exceeded max_ms cap of 5000"
            );
        }

        // Normal case: base_ms < max_ms — cap still applies at max_ms.
        for attempt in 0..=20 {
            let ms = compute_backoff_ms(attempt, 400, 5000);
            assert!(
                ms <= 5000,
                "attempt={attempt}: backoff {ms} exceeded max_ms cap of 5000"
            );
        }
    }

    #[test]
    fn parse_retry_after_ms_from_error_works() {
        let e1 = anyhow::anyhow!("wp transport non-2xx status=429 retry_after=30");
        assert_eq!(parse_retry_after_ms_from_error(&e1), Some(30_000));

        let e2 = anyhow::anyhow!("component api non-2xx status=429 retry_after_ms=1800");
        assert_eq!(parse_retry_after_ms_from_error(&e2), Some(1800));

        let e3 = anyhow::anyhow!("status=503");
        assert_eq!(parse_retry_after_ms_from_error(&e3), None);
    }

    // -----------------------------------------------------------------------
    // L3 fault injection through the real component + retry path (plan §7
    // TEST-NETWORK-RESILIENCE-001; failure-modes FM-HTTP-429 / FM-HTTP-403 /
    // FM-RETRY-EXHAUSTION). A counting mock provider answers every request
    // with a fixed status; retry_with_backoff drives
    // translate_text_via_component, so attempt counts and backoff waits are
    // observed end-to-end, not inferred from the classifier alone.
    // -----------------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    /// Counting mock provider: every request gets `status_line` + optional
    /// `extra_headers` + a plain-text `body`. Returns (port, request count).
    async fn start_status_counting_server(
        status_line: &'static str,
        extra_headers: &'static str,
        body: &'static str,
    ) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(socket);
                    let mut content_length: usize = 0;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        let lower = line.to_lowercase();
                        if lower.starts_with("content-length:") {
                            content_length =
                                lower["content-length:".len()..].trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body_buf = vec![0u8; content_length];
                    if content_length > 0 {
                        let _ = reader.read_exact(&mut body_buf).await;
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let resp = format!(
                        "HTTP/1.1 {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status_line,
                        extra_headers,
                        body.len(),
                        body
                    );
                    let _ = reader.get_mut().write_all(resp.as_bytes()).await;
                });
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        (port, count)
    }

    fn retry_worker_config(retry_max: u32, retry_base_ms: u64, retry_max_ms: u64) -> WorkerConfig {
        WorkerConfig {
            worker_id: "test-worker".to_string(),
            task_pull_statuses: vec![],
            task_concurrency: 1,
            retry_max,
            retry_base_ms,
            retry_max_ms,
            component_fallback_enabled: false,
            default_max_input_chars: 10_000,
            default_split_strategy: "paragraph".to_string(),
            discovery_mode: false,
            review_mode: false,
        }
    }

    fn status_runtime(port: u16) -> ComponentRuntime {
        use crate::types::{ComponentRequest, ComponentResponse, ComponentTemplate};
        let template = ComponentTemplate {
            id: "status-fault-injection".to_string(),
            name: "Status Fixture".to_string(),
            version: "1.0.0".to_string(),
            kind: "text_translation".to_string(),
            client_contract: None,
            default_values: None,
            auth: None,
            prepare: None,
            request: ComponentRequest {
                method: "POST".to_string(),
                url: format!("http://127.0.0.1:{}", port),
                headers: None,
                body: Some(json!({"text": "{{input.text}}"})),
                body_type: Some("json".to_string()),
                response_type: None,
            },
            response: ComponentResponse {
                translated_text_path: Some("data.translated".to_string()),
                error_path: Some("error.message".to_string()),
                translated_ref_path: None,
                translated_media_ref_path: None,
                translated_image_ref_path: None,
                translated_video_ref_path: None,
                translated_audio_ref_path: None,
                translated_document_ref_path: None,
            },
            async_poll: None,
            source_upload: None,
            sign: None,
            constraints: None,
            editable_params: vec![],
            translation_modes: vec![],
        };
        ComponentRuntime {
            template,
            auth_values: std::collections::HashMap::new(),
            supported_business_lines: vec![],
            language_map: std::collections::HashMap::new(),
            supported_content_formats: vec![],
            supported_formats: vec![],
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
            proxy_profile_id: None,
        }
    }

    #[tokio::test]
    async fn retry_429_honors_retry_after_and_bounds_attempts() {
        // FM-HTTP-429 L3: the rate-limited provider is retried exactly
        // max_attempts times, the wait honors Retry-After (1000ms dominates
        // the 10ms base), and the final 429 error surfaces.
        let (port, count) =
            start_status_counting_server("429 Too Many Requests", "Retry-After: 1\r\n", "rate limited").await;
        let client = reqwest::Client::new();
        let comp = status_runtime(port);
        let wc = retry_worker_config(2, 10, 20);
        let started = Instant::now();

        let result = retry_with_backoff("fm-429", 1, "/dev/null", &wc, |_| {
            crate::component_rt::runner::translate_text_via_component(
                &client,
                &comp,
                "Hello",
                "en",
                "zh",
            )
        })
        .await;

        let err = result.expect_err("429 provider must end in an error");
        assert!(
            err.to_string().contains("status=429"),
            "final error should carry the 429 status, got: {err}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "retry_max=2 must cap attempts at 3"
        );
        assert!(
            started.elapsed().as_millis() >= 1000,
            "Retry-After (1s) must dominate the 10ms base backoff; elapsed {}ms",
            started.elapsed().as_millis()
        );
    }

    #[tokio::test]
    async fn retry_403_is_not_retried() {
        // FM-HTTP-403 L3: a forbidden provider answer is terminal — exactly
        // one request, no retry storm against the rate-limiting endpoint.
        let (port, count) =
            start_status_counting_server("403 Forbidden", "", "forbidden").await;
        let client = reqwest::Client::new();
        let comp = status_runtime(port);
        let wc = retry_worker_config(5, 10, 20);

        let result = retry_with_backoff("fm-403", 2, "/dev/null", &wc, |_| {
            crate::component_rt::runner::translate_text_via_component(
                &client,
                &comp,
                "Hello",
                "en",
                "zh",
            )
        })
        .await;

        let err = result.expect_err("403 provider must end in an error");
        assert!(
            err.to_string().contains("status=403"),
            "final error should carry the 403 status, got: {err}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "403 is not retryable: exactly one attempt expected"
        );
    }

    #[tokio::test]
    async fn retry_exhaustion_bounds_attempts_and_preserves_last_error() {
        // FM-RETRY-EXHAUSTION L3: a persistently failing 5xx provider is
        // attempted exactly max_attempts times, and the returned error is
        // the LAST failure (preserved, no zombie success/panic).
        let (port, count) =
            start_status_counting_server("500 Internal Server Error", "", "boom").await;
        let client = reqwest::Client::new();
        let comp = status_runtime(port);
        let wc = retry_worker_config(2, 5, 10);

        let result = retry_with_backoff("fm-exhaustion", 3, "/dev/null", &wc, |_| {
            crate::component_rt::runner::translate_text_via_component(
                &client,
                &comp,
                "Hello",
                "en",
                "zh",
            )
        })
        .await;

        let err = result.expect_err("persistent 500 must end in an error");
        assert!(
            err.to_string().contains("status=500"),
            "the LAST error must be preserved, got: {err}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "retry_max=2 must cap attempts at 3"
        );
    }
}

fn is_retryable_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    // Network transient errors
    msg.contains("timeout")
        || msg.contains("timed out")
        || msg.contains("connection refused")
        || msg.contains("connection reset")
        || msg.contains("connection closed")
        || msg.contains("connection aborted")
        || msg.contains("broken pipe")
        || msg.contains("dns")
        || msg.contains("name resolution")
        || msg.contains("no route")
        // HTTP retryable status codes
        || msg.contains("status=408") || msg.contains("status 408")
        || msg.contains("status=429") || msg.contains("status 429")
        || msg.contains("status=500") || msg.contains("status 500")
        || msg.contains("status=502") || msg.contains("status 502")
        || msg.contains("status=503") || msg.contains("status 503")
        || msg.contains("status=504") || msg.contains("status 504")
}
