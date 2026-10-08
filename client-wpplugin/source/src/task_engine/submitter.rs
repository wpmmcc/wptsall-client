use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use reqwest::Client;
use serde_json::Value;
use url::Url;

#[cfg(test)]
#[path = "../../../../tests/modules/client-wpplugin/unit/encrypted_upload_assets.rs"]
mod encrypted_upload_assets;

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

#[cfg(test)]
mod callback_receipt_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires explicitly prepared, verified owned WP callback fixture"]
    async fn callback_receipt_owned_wp_signed_encrypted_six_batch_lanes() {
        let _key = crate::db::owned_mock_bindings_key();
        let path = std::env::var("WPTSALL_OWNED_CALLBACK_HTTP_FIXTURE")
            .expect("explicit owned callback fixture required");
        let fixture: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(fixture["format"], "owned-callback-http-v1");
        let url = reqwest::Url::parse(fixture["wp_base"].as_str().unwrap()).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let owner = fixture["owner"].as_str().unwrap();
        let name = fixture["container"].as_str().unwrap();
        assert!(
            name == format!("wptsall-owned-{owner}-a")
                || name == format!("wptsall-owned-{owner}-b")
        );
        let label = std::process::Command::new("docker")
            .args([
                "inspect",
                name,
                "--format",
                "{{index .Config.Labels \"com.wptsall.owned.run\"}}",
            ])
            .output()
            .unwrap();
        assert!(label.status.success());
        assert_eq!(String::from_utf8(label.stdout).unwrap().trim(), owner);
        let root = tempfile::tempdir().unwrap();
        let _data =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let _encrypt = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "on");
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let cases = fixture["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 6);
        for case in cases {
            let payload: crate::types::I18nCallbackPayload =
                serde_json::from_value(case["payload"].clone()).unwrap();
            let mut first_id = None;
            for replay in [false, true] {
                let result = send_i18n_translation_callback(
                    &client,
                    url.as_str(),
                    fixture["token"].as_str().unwrap(),
                    &payload.worker_id,
                    fixture["device_id"].as_str().unwrap(),
                    &payload.client_task_id,
                    &payload,
                )
                .await;
                assert!(
                    result.is_ok(),
                    "owned {} callback refused (details stay private)",
                    payload.business_line
                );
                let receipt = result.unwrap();
                assert_eq!(receipt["result_status"], "synced");
                assert_eq!(receipt["entries_updated"], 2);
                assert_eq!(receipt["entries_rejected"], 0);
                if replay {
                    assert_eq!(receipt["idempotent"], true);
                    assert_eq!(first_id.as_ref(), Some(&receipt["result_id"]));
                } else {
                    first_id = Some(receipt["result_id"].clone());
                }
            }
        }
        let content_cases = fixture["content_cases"].as_array().unwrap();
        assert_eq!(content_cases.len(), 2);
        for (index, case) in content_cases.iter().enumerate() {
            let payload: crate::types::TranslationCallbackPayload =
                serde_json::from_value(case["payload"].clone()).unwrap();
            let mut first_id = None;
            for replay in [false, true] {
                let result = send_translation_callback(
                    &client,
                    url.as_str(),
                    fixture["token"].as_str().unwrap(),
                    &payload.worker_id,
                    fixture["device_id"].as_str().unwrap(),
                    &payload.client_task_id,
                    &payload,
                )
                .await;
                assert!(
                    result.is_ok(),
                    "owned content callback refused (details stay private)"
                );
                let receipt = result.unwrap();
                assert_eq!(
                    receipt["result_status"],
                    if index == 0 { "synced" } else { "partial" }
                );
                assert!(receipt["sync_result"]["success"].as_bool().unwrap());
                if replay {
                    assert_eq!(receipt["idempotent"], true);
                    assert_eq!(first_id.as_ref(), Some(&receipt["result_id"]));
                } else {
                    first_id = Some(receipt["result_id"].clone());
                }
            }
        }
    }

    #[test]
    fn callback_receipt_rejects_accepted_but_unapplied_queue() {
        let ack = serde_json::json!({
            "success": true, "result_id": 42, "sync_task_id": 7, "queued": true,
            "protocol": "v2", "result_status": "pending"
        });
        assert!(
            validate_translation_callback_ack(ack).is_err(),
            "an accepted pending queue is not applied content"
        );
    }

    #[test]
    fn callback_receipt_rejects_explicit_write_back_failure() {
        let ack = serde_json::json!({
            "success": true, "result_id": 42, "protocol": "v2",
            "result_status": "synced", "sync_result": {"success": false}
        });
        assert!(
            validate_translation_callback_ack(ack).is_err(),
            "contradictory write-back evidence must not mark the paid result done"
        );
    }

    #[test]
    fn callback_receipt_rejects_missing_durable_result_state() {
        assert!(
            validate_translation_callback_ack(serde_json::json!({
                "success": true, "result_id": 42, "protocol": "v2"
            }))
            .is_err(),
            "an arbitrary positive result id is not a durable write-back receipt"
        );
    }

    #[test]
    fn callback_receipt_accepts_terminal_applied_partial_and_skip() {
        for status in ["synced", "completed", "partial", "cancelled"] {
            let ack = serde_json::json!({
                "success": true, "result_id": 42, "protocol": "v2",
                "result_status": status
            });
            assert!(validate_translation_callback_ack(ack).is_ok(), "{status}");
        }
    }

    #[test]
    fn callback_receipt_i18n_requires_exact_complete_batch_counts() {
        let ack = serde_json::json!({
            "success":true,"result_id":42,"protocol":"v2","result_status":"synced",
            "entries_updated":2,"entries_rejected":0
        });
        assert!(validate_i18n_callback_ack(ack.clone(), 2).is_ok());
        for expected in [0, 1, 3] {
            assert!(validate_i18n_callback_ack(ack.clone(), expected).is_err());
        }
        for (key, value) in [
            ("entries_updated", serde_json::json!(1)),
            ("entries_updated", serde_json::json!("2")),
            ("entries_rejected", serde_json::json!(1)),
            ("entries_rejected", serde_json::json!(-1)),
            ("result_status", serde_json::json!("partial")),
        ] {
            let mut invalid = ack.clone();
            invalid[key] = value;
            assert!(validate_i18n_callback_ack(invalid, 2).is_err(), "{key}");
        }
        for missing in ["entries_updated", "entries_rejected"] {
            let mut invalid = ack.clone();
            invalid.as_object_mut().unwrap().remove(missing);
            assert!(validate_i18n_callback_ack(invalid, 2).is_err());
        }
    }

    #[test]
    fn callback_receipt_refuses_wrong_protocol_invalid_ids_and_terminal_flags() {
        let ack = serde_json::json!({
            "success":true,"result_id":42,"protocol":"v2","result_status":"synced"
        });
        for (key, value) in [
            ("success", serde_json::json!("true")),
            ("result_id", serde_json::json!(0)),
            ("result_id", serde_json::json!(-1)),
            ("result_id", serde_json::json!("42")),
            ("protocol", serde_json::json!("path-b")),
            ("protocol", Value::Null),
            ("result_status", serde_json::json!("failed")),
            ("result_status", Value::Null),
            ("partial", serde_json::json!(true)),
            ("partial", serde_json::json!("true")),
            ("skipped", serde_json::json!(true)),
            ("skipped", serde_json::json!("true")),
            ("sync_result", Value::Null),
            (
                "sync_result",
                serde_json::json!({"success":true,"partial":true}),
            ),
            (
                "sync_result",
                serde_json::json!({"success":true,"skipped":true}),
            ),
        ] {
            let mut invalid = ack.clone();
            invalid[key] = value;
            assert!(validate_translation_callback_ack(invalid).is_err(), "{key}");
        }
    }
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
    // AF-04 (opus5): the secret-scoped base is an explicit precondition, not
    // an implicit convention — the dead `route_secret` params are gone and
    // a base without the `/{secret}/client` segment fails fast instead of
    // silently 404ing against the unsecreted path.
    // -----------------------------------------------------------------------

    #[test]
    fn secret_client_base_guard_accepts_secret_scoped_base() {
        assert!(ensure_secret_client_base(
            "https://example.com/wp-json/wptsall/v2/abc123/client/translation-callback",
            "translation-callback"
        )
        .is_ok());
        assert!(ensure_secret_client_base(
            "https://example.com/wp-json/wptsall/v2/abc123/client/media-upload",
            "media-upload"
        )
        .is_ok());
        // Trailing slashes are tolerated.
        assert!(ensure_secret_client_base(
            "https://example.com/wp-json/wptsall/v2/abc123/client/",
            "media-upload"
        )
        .is_ok());
    }

    #[test]
    fn secret_client_base_guard_rejects_unsecreted_base_with_explicit_error() {
        let err = ensure_secret_client_base(
            "https://example.com/wp-json/wptsall/v2/translation-callback",
            "translation-callback",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("route secret missing"),
            "unexpected error: {}",
            err
        );

        let err = ensure_secret_client_base("https://example.com", "media-upload").unwrap_err();
        assert!(
            err.to_string().contains("route secret missing"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn send_translation_callback_rejects_unsecreted_base_before_any_request() {
        let payload = crate::types::I18nCallbackPayload {
            business_line: "plugin_i18n".to_string(),
            relation_id: 1,
            client_task_id: "af04-guard".to_string(),
            worker_id: "worker-test".to_string(),
            source_lang: "en".to_string(),
            target_lang: "zh".to_string(),
            entries: vec![],
        };
        let err = send_i18n_translation_callback(
            &Client::new(),
            "https://example.com/wp-json/wptsall/v2",
            "token",
            "worker-test",
            "worker-test",
            "af04-guard",
            &payload,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("route secret missing"),
            "unexpected error: {}",
            err
        );
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

    #[tokio::test]
    async fn durable_uploaded_local_result_is_retained_for_callback_recovery() {
        let _key = crate::db::owned_mock_bindings_key();
        let root = tempfile::tempdir().unwrap();
        let _data =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
        let asset = root.path().join("owned-paid-result.bin");
        std::fs::write(&asset, b"owned-paid-bytes").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).await.unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if line.to_ascii_lowercase().starts_with("content-length:") {
                    length = line
                        .split_once(':')
                        .unwrap()
                        .1
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                }
                headers.push_str(&line);
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).await.unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            let body =
                crate::web_ui::test_support::media_operation_response_from_headers(&headers, 321)
                    .to_string();
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-token",
                body.as_bytes(),
            );
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-WPTSALL-Response-Signature: {signature}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
        });
        let mut payload: TranslationCallbackPayload = serde_json::from_value(json!({
            "relation_id":7,"business_line":"content_translation","object_type":"post_type",
            "post_type":"post","object_id":42,"translated_fields":{},"translated_meta":{},
            "media_mappings":[{"source_id":9,"translated_ref":format!("file://{}",asset.display())}],
            "client_task_id":"owned-paid","worker_id":"owned-worker","source_lang":"en",
            "target_lang":"zh","execution_time_ms":0
        })).unwrap();
        upload_pending_media(
            &Client::builder().no_proxy().build().unwrap(),
            &format!("http://127.0.0.1:{port}/wp-json/wptsall/v2/owned-secret/client"),
            "owned-token",
            "owned-worker",
            "owned-device",
            1,
            7,
            &mut payload,
            "/dev/null",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            payload.media_mappings[0].attachment_id,
            Some(321),
            "real signed upload must succeed"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.await.unwrap();
        assert_eq!(
            std::fs::read(&asset).unwrap(),
            b"owned-paid-bytes",
            "successful upload is not authorization to delete a retained result"
        );
    }

    #[test]
    fn chunk_scratch_same_source_filename_cannot_overwrite_another_upload() {
        let _key = crate::db::owned_mock_bindings_key();
        let root = tempfile::tempdir().unwrap();
        let first =
            stage_chunk_upload_bytes(root.path().to_str().unwrap(), 9, "owned.bin", b"first")
                .unwrap();
        let second =
            stage_chunk_upload_bytes(root.path().to_str().unwrap(), 9, "owned.bin", b"second")
                .unwrap();
        assert_ne!(
            first, second,
            "parallel object/relation uploads need private scratch files"
        );
        assert_eq!(crate::retained_assets::read(&first, 5).unwrap(), b"first");
        assert_eq!(crate::retained_assets::read(&second, 6).unwrap(), b"second");
        assert_ne!(std::fs::read(first).unwrap(), b"first");
        assert_ne!(std::fs::read(second).unwrap(), b"second");
    }

    fn retry_worker_config(retry_max: u32, retry_base_ms: u64, retry_max_ms: u64) -> WorkerConfig {
        WorkerConfig {
            worker_id: "test-worker".to_string(),
            device_id: "test-worker".to_string(),
            task_pull_statuses: vec![],
            task_concurrency: 1,
            retry_max,
            retry_base_ms,
            retry_max_ms,
            component_fallback_enabled: false,
            default_max_input_chars: 10_000,
            default_split_strategy: "paragraph".to_string(),
            discovery_mode: false,
            discovery_max_items_per_run: 100,
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
                http_limits: None,
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
        let (port, count) = start_status_counting_server(
            "429 Too Many Requests",
            "Retry-After: 1\r\n",
            "rate limited",
        )
        .await;
        let client = reqwest::Client::new();
        let comp = status_runtime(port);
        let wc = retry_worker_config(2, 10, 20);
        let started = Instant::now();

        let result = retry_with_backoff("fm-429", 1, "/dev/null", &wc, |_| {
            crate::component_rt::runner::translate_text_via_component(
                &client, &comp, "Hello", "en", "zh",
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
        let (port, count) = start_status_counting_server("403 Forbidden", "", "forbidden").await;
        let client = reqwest::Client::new();
        let comp = status_runtime(port);
        let wc = retry_worker_config(5, 10, 20);

        let result = retry_with_backoff("fm-403", 2, "/dev/null", &wc, |_| {
            crate::component_rt::runner::translate_text_via_component(
                &client, &comp, "Hello", "en", "zh",
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
                &client, &comp, "Hello", "en", "zh",
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

    #[tokio::test]
    async fn retry_classifies_context_wrapped_transport_5xx_as_retryable() {
        // FL-17 L3: WP callback submissions wrap transport errors with
        // .with_context() ("i18n translation callback failed
        // (relation_id=N)") — the retryable "status=500" lives in the
        // CAUSE chain, not the outermost message. The classifier must
        // walk the full chain or every callback 5xx is single-shot
        // despite the retry_with_backoff wrap.
        let (port, count) =
            start_status_counting_server("500 Internal Server Error", "", "boom").await;
        let client = reqwest::Client::new();
        let comp = status_runtime(port);
        let wc = retry_worker_config(2, 1, 2);

        let result: anyhow::Result<()> =
            retry_with_backoff("fm-ctx-500", 9, "/dev/null", &wc, |_| {
                let client = client.clone();
                let comp = comp.clone();
                async move {
                    crate::component_rt::runner::translate_text_via_component(
                        &client, &comp, "Hello", "en", "zh",
                    )
                    .await
                    .map(|_| ())
                    .with_context(|| "wrapped submission failed (relation_id=9)")
                }
            })
            .await;

        let err = result.expect_err("persistent context-wrapped 500 must end in an error");
        // The OUTER context is what plain Display shows — it must NOT be
        // mistaken for the whole classification input.
        assert_eq!(
            err.to_string(),
            "wrapped submission failed (relation_id=9)",
            "outermost context stays the display message"
        );
        assert!(
            format!("{:#}", err).contains("status=500"),
            "the cause chain carries the transport status, got: {err:#}"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "context-wrapped 5xx must exhaust max_attempts=3, not single-shot"
        );
    }
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
