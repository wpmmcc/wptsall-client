use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use reqwest::Client;
use serde_json::Value;
use url::Url;



/// AF-04 (opus5): the WP client API is secret-scoped — every client route
/// lives under `/{secret}/client/...`, so the resolved URL must carry the
/// `/client` segment. The submit paths used to accept a dead `route_secret`
/// parameter and silently relied on that URL shape; a misconstructed base
/// produced an obscure 404. The parameter is gone and the URL shape is now
/// an explicit precondition: a base without the segment fails fast with a
/// clear error instead of a silent 404.
fn ensure_secret_client_base(wp_base: &str, endpoint: &str) -> anyhow::Result<()> {
    if !wp_base.trim_end_matches('/').contains("/client") {
        return Err(anyhow!(
            "route secret missing: WP base `{}` carries no `/{{secret}}/client` segment — \
             configure the binding route_secret before submitting {}",
            wp_base,
            endpoint
        ));
    }
    Ok(())
}

pub(crate) fn validate_translation_callback_ack(value: Value) -> anyhow::Result<Value> {
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
    if value.get("protocol").and_then(Value::as_str) != Some("v2") {
        return Err(anyhow!("translation callback ack missing protocol=v2"));
    }
    let result_status = value.get("result_status").and_then(Value::as_str);
    if !matches!(
        result_status,
        Some("synced" | "completed" | "partial" | "cancelled")
    ) {
        return Err(anyhow!(
            "translation callback ack missing durable terminal result_status; saved result retained"
        ));
    }
    if let Some(sync_result) = value.get("sync_result") {
        if sync_result.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(anyhow!(
                "translation callback ack contradicts write-back success; saved result retained"
            ));
        }
    }
    for (flag, status) in [("partial", "partial"), ("skipped", "cancelled")] {
        for evidence in [
            value.get(flag),
            value.get("sync_result").and_then(|result| result.get(flag)),
        ]
        .into_iter()
        .flatten()
        {
            if evidence.as_bool() != Some(result_status == Some(status)) {
                return Err(anyhow!(
                    "translation callback ack has inconsistent terminal flags"
                ));
            }
        }
    }
    Ok(value)
}

pub(crate) fn validate_i18n_callback_ack(
    value: Value,
    expected_entries: usize,
) -> anyhow::Result<Value> {
    let value = validate_translation_callback_ack(value)?;
    if !matches!(
        value.get("result_status").and_then(Value::as_str),
        Some("synced" | "completed")
    ) || expected_entries == 0
        || value.get("entries_updated").and_then(Value::as_u64)
            != u64::try_from(expected_entries).ok()
        || value.get("entries_rejected").and_then(Value::as_u64) != Some(0)
    {
        return Err(anyhow!(
            "i18n callback ack does not prove the complete original entry batch; saved result retained"
        ));
    }
    Ok(value)
}


use serde_json::json;

use crate::auth::wp_post_with_transport_and_headers;
use crate::component_rt::non_text::ChunkedUploadConfig;
use crate::logging::log_event;
use crate::types::*;

mod upload_receipt;
pub(crate) use upload_receipt::{
    upload_pending_media_durable, upload_pending_media_with_optional_db,
};

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
    device_id: &str,
    idempotency_key: &str,
    payload: &crate::types::TranslationCallbackPayload,
) -> anyhow::Result<Value> {
    let callback_url = format!("{}/translation-callback", wp_base.trim_end_matches('/'));
    ensure_secret_client_base(&callback_url, "translation-callback")?;
    let body = serde_json::to_value(payload)
        .with_context(|| "serialize translation callback payload failed")?;
    let headers: Vec<(&str, &str)> = vec![("Idempotency-Key", idempotency_key)];
    let ack = wp_post_with_transport_and_headers(
        client,
        &callback_url,
        token,
        worker_id,
        device_id,
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
    device_id: &str,
    idempotency_key: &str,
    payload: &crate::types::I18nCallbackPayload,
) -> anyhow::Result<Value> {
    let callback_url = format!("{}/translation-callback", wp_base.trim_end_matches('/'));
    ensure_secret_client_base(&callback_url, "i18n translation-callback")?;
    let body =
        serde_json::to_value(payload).with_context(|| "serialize i18n callback payload failed")?;
    let headers: Vec<(&str, &str)> = vec![("Idempotency-Key", idempotency_key)];
    let ack = wp_post_with_transport_and_headers(
        client,
        &callback_url,
        token,
        worker_id,
        device_id,
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

    validate_i18n_callback_ack(ack, payload.entries.len())
}

/// Upload a media file binary to WP's media-upload endpoint.
///
/// Returns the attachment_id on success.
///
/// Uses the same durable operation and transport policy as chunked uploads.
/// AES-GCM wraps the original bytes when required, without changing their MIME
/// type or plaintext signature. Production sites should use HTTPS because
/// application-layer body encryption does not protect the token headers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_media_to_wp(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    source_id: u64,
    filename: &str,
    content_type: &str,
    file_data: Vec<u8>,
    log_file: &str,
) -> anyhow::Result<u64> {
    let upload_url = format!("{}/media-upload", wp_base.trim_end_matches('/'));
    ensure_secret_client_base(&upload_url, "media-upload")?;
    upload_media_via_best_path(
        client,
        wp_base,
        token,
        worker_id,
        device_id,
        task_id,
        relation_id,
        source_id,
        filename,
        content_type,
        file_data,
        log_file,
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn upload_media_via_best_path(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    source_id: u64,
    filename: &str,
    content_type: &str,
    file_data: Vec<u8>,
    log_file: &str,
    route_secret: Option<&str>,
    scope: Option<&str>,
) -> anyhow::Result<u64> {
    let data_dir = std::env::var("WPTSALL_DATA_DIR")
        .unwrap_or_else(|_| crate::config::DEFAULT_DATA_DIR.to_string());
    let temp_path = stage_chunk_upload_bytes(&data_dir, source_id, filename, &file_data)?;

    let config = ChunkedUploadConfig {
        wp_base: wp_base.to_string(),
        token: token.to_string(),
        route_secret: route_secret.map(|s| s.to_string()),
        worker_id: worker_id.to_string(),
        device_id: device_id.to_string(),
        chunk_size: 0,
    };
    let result = crate::component_rt::non_text::recovery::upload(
        client,
        &config,
        temp_path.to_string_lossy().as_ref(),
        filename,
        content_type,
        i64::try_from(source_id).unwrap_or(0),
        task_id,
        i64::try_from(relation_id).unwrap_or(0),
        scope,
    )
    .await?;
    let _ = std::fs::remove_file(&temp_path);

    if result.success && result.attachment_id > 0 {
        Ok(u64::try_from(result.attachment_id).unwrap_or(0))
    } else {
        Err(anyhow!("chunked media upload failed: {}", result.error))
    }
}

fn stage_chunk_upload_bytes(
    data_dir: &str,
    _source_id: u64,
    _filename: &str,
    bytes: &[u8],
) -> anyhow::Result<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(data_dir).join("tmp-media-upload");
    let dir = crate::retained_assets::directory(&dir)?;
    let path = dir.join(format!("wpa1{}.bin", uuid::Uuid::new_v4().simple()));
    crate::retained_assets::write_bytes(&path, bytes)?;
    Ok(path)
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
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    payload: &mut TranslationCallbackPayload,
    log_file: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<()> {
    upload_pending_media_with_optional_db(
        None,
        client,
        wp_base,
        token,
        worker_id,
        device_id,
        task_id,
        relation_id,
        payload,
        log_file,
        route_secret,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_pending_media_with_scope(
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    payload: &mut TranslationCallbackPayload,
    log_file: &str,
    route_secret: Option<&str>,
    scope: Option<&str>,
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
                device_id,
                task_id,
                relation_id,
                mapping.source_id,
                &filename,
                &content_type,
                bytes,
                log_file,
                route_secret,
                scope,
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
                    device_id,
                    task_id,
                    relation_id,
                    mapping.source_id,
                    &filename,
                    &content_type,
                    bytes,
                    log_file,
                    route_secret,
                    scope,
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
                let file_data =
                    match crate::retained_assets::read_async(path_str, 512 * 1024 * 1024).await {
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
                let mut filename = crate::retained_assets::filename(&path)
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
                    device_id,
                    task_id,
                    relation_id,
                    mapping.source_id,
                    &filename,
                    &content_type,
                    file_data,
                    log_file,
                    route_secret,
                    scope,
                )
                .await
                {
                    Ok(att_id) => {
                        mapping.attachment_id = Some(att_id);
                        mapping.translated_ref = String::new();
                        // Upload acceptance is not callback acknowledgement or
                        // authorization to delete the retained paid result.
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
            device_id,
            task_id,
            relation_id,
            mapping.source_id,
            &filename,
            &content_type,
            file_data,
            log_file,
            route_secret,
            scope,
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



fn is_retryable_error(err: &anyhow::Error) -> bool {
    // FL-17: classify on the FULL error chain, not the outermost context.
    // WP-transport failures are context-wrapped on their way out (e.g.
    // "i18n translation callback failed (relation_id=N)") with the
    // retryable status ("status=500 …") living in a CAUSE. anyhow's plain
    // Display shows only the outermost context, so every callback 5xx was
    // classified permanent and the retry_max budget never engaged — the
    // submission was single-shot despite the retry_with_backoff wrap.
    err.chain()
        .any(|cause| error_message_is_retryable(&cause.to_string()))
}

fn error_message_is_retryable(message: &str) -> bool {
    let msg = message.to_lowercase();
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
